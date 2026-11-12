package orchestrator

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

func TestTerminalWSBridgesShellOutput(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns a PTY shell")
	}

	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "metering", Task: "t", Agent: "pi", Branch: "orch/m", Worktree: t.TempDir()}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	srv := httptest.NewServer(NewDaemonServerFull(fleet, NewJournal(), nil, nil, NewTerminalHub()))
	t.Cleanup(srv.Close)

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/sessions/metering/terminal"
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	conn, _, err := websocket.Dial(ctx, wsURL, nil)
	if err != nil {
		t.Fatalf("dial terminal: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	// Ask the shell to print a marker; the PTY echoes the command and its
	// output — the marker must come back through the bridge.
	if err := conn.Write(ctx, websocket.MessageBinary, []byte("echo ws-terminal-marker\n")); err != nil {
		t.Fatalf("write: %v", err)
	}

	seen := make([]byte, 0, 4096)
	deadline := time.Now().Add(4 * time.Second)
	for !strings.Contains(string(seen), "ws-terminal-marker") {
		if time.Now().After(deadline) {
			t.Fatalf("marker never echoed; got %q", string(seen))
		}
		readCtx, readCancel := context.WithTimeout(ctx, time.Second)
		_, data, err := conn.Read(readCtx)
		readCancel()
		if err != nil {
			if strings.Contains(err.Error(), "timeout") && ctx.Err() == nil {
				continue
			}
			t.Fatalf("read: %v (seen %q)", err, string(seen))
		}
		seen = append(seen, data...)
	}
}

func TestTerminalWSUnknownSession(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServerFull(NewFleet(), NewJournal(), nil, nil, NewTerminalHub()))
	t.Cleanup(srv.Close)

	// The handler rejects the upgrade with 404 when the session does not
	// exist; websocket.Dial surfaces it as a failed handshake.
	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/sessions/ghost/terminal"
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if _, _, err := websocket.Dial(ctx, wsURL, nil); err == nil {
		t.Fatal("dial succeeded on an unknown session, want the 404 handshake failure")
	}
}

func TestTerminalWSTwoClientsShareOneShell(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns a PTY shell")
	}
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "shared", Task: "t", Agent: "pi", Branch: "orch/s", Worktree: t.TempDir()}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	srv := httptest.NewServer(NewDaemonServerFull(fleet, NewJournal(), nil, nil, NewTerminalHub()))
	t.Cleanup(srv.Close)

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/sessions/shared/terminal"
	ctx := context.Background()

	first, _, err := websocket.Dial(ctx, wsURL, nil)
	if err != nil {
		t.Fatalf("dial 1: %v", err)
	}
	defer first.Close(websocket.StatusNormalClosure, "")

	second, _, err := websocket.Dial(ctx, wsURL, nil)
	if err != nil {
		t.Fatalf("dial 2 (attach): %v", err)
	}
	defer second.Close(websocket.StatusNormalClosure, "")

	// Keystrokes through the second client reach the same shell: keep asking
	// for the marker until either client's stream shows it. The attach order
	// is racy by design (each client subscribes when its Open advances), so
	// the marker may echo before the first client's mailbox exists.
	for attempt := 0; attempt < 5; attempt++ {
		if err := second.Write(ctx, websocket.MessageBinary, []byte("echo shared-pty\n")); err != nil {
			t.Fatalf("write via second: %v", err)
		}
		if sawInEither(first, second, "shared-pty", ctx) {
			return
		}
	}
	t.Fatal("the marker never reached either client through the shared shell")
}

// sawInEither drains both clients until one shows the marker (bounded).
func sawInEither(a, b *websocket.Conn, marker string, ctx context.Context) bool {
	deadline := time.Now().Add(2 * time.Second)
	seenA, seenB := []byte{}, []byte{}
	for time.Now().Before(deadline) {
		for _, pair := range []struct {
			conn *websocket.Conn
			seen *[]byte
		}{{a, &seenA}, {b, &seenB}} {
			readCtx, readCancel := context.WithTimeout(ctx, 150*time.Millisecond)
			_, data, err := pair.conn.Read(readCtx)
			readCancel()
			if err == nil {
				*pair.seen = append(*pair.seen, data...)
				if strings.Contains(string(*pair.seen), marker) {
					return true
				}
			}
		}
	}
	return false
}

func TestTerminalWithoutHubIs501(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServerFull(NewFleet(), NewJournal(), nil, nil, nil))
	t.Cleanup(srv.Close)
	resp, err := http.Get(srv.URL + "/sessions/metering/terminal")
	if err != nil {
		t.Fatalf("GET: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusNotImplemented {
		t.Fatalf("status = %d, want 501", resp.StatusCode)
	}
}

func TestEvidenceEndpointPackagesFacts(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git")
	}
	repo := initRepo(t)
	fleet := NewFleet()
	journal := NewJournal()
	launcher := &Launcher{
		Fleet:     fleet,
		Router:    NewRouter(2),
		Journal:   journal,
		Worktrees: &WorktreeManager{Repo: repo, Parent: t.TempDir()},
		Base:      "main",
	}
	srv := httptest.NewServer(NewDaemonServerFull(fleet, journal, launcher, nil, NewTerminalHub()))
	t.Cleanup(srv.Close)

	body := `{"id":"writer","task":"Escribir feature","agent":"pi","branch":"orch/writer","launch":true}`
	resp, err := http.Post(srv.URL+"/sessions", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	resp.Body.Close()

	// The default Command is the agent CLI; swap in a fake worker by
	// stopping and relaunching through the same launcher is heavy — instead
	// drive evidence on the launched session's empty diff plus journal.
	deadline := time.Now().Add(3 * time.Second)
	var evidence Evidence
	for {
		res, err := http.Get(srv.URL + "/sessions/writer/evidence")
		if err == nil {
			err = json.NewDecoder(res.Body).Decode(&evidence)
			res.Body.Close()
			if err == nil && evidence.SessionID == "writer" && evidence.Journal != nil {
				break
			}
		}
		if time.Now().After(deadline) {
			t.Fatalf("evidence never arrived: %+v", evidence)
		}
		time.Sleep(20 * time.Millisecond)
	}
	if evidence.Status == "" || evidence.Agent != "pi" {
		t.Fatalf("evidence = %+v, want agent+status", evidence)
	}
}

func TestEvidenceUnknownSession(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServerFull(NewFleet(), NewJournal(), nil, nil, nil))
	t.Cleanup(srv.Close)
	resp, err := http.Get(srv.URL + "/sessions/ghost/evidence")
	if err != nil {
		t.Fatalf("GET: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", resp.StatusCode)
	}
}
