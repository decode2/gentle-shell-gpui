package orchestrator

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

func TestTerminalWSRejectsForeignOrigins(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "m", Task: "t", Agent: "pi", Branch: "orch/m", Worktree: t.TempDir()}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	srv := httptest.NewServer(NewDaemonServerFull(fleet, NewJournal(), nil, nil, NewTerminalHub()))
	t.Cleanup(srv.Close)

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/sessions/m/terminal"
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	// A malicious web page's origin must not reach the shell upgrade.
	conn, _, err := websocket.Dial(ctx, wsURL, &websocket.DialOptions{
		HTTPHeader: http.Header{"Origin": []string{"https://evil.example"}},
	})
	if err == nil {
		conn.Close(websocket.StatusNormalClosure, "")
		t.Fatal("dial with a foreign Origin succeeded, want rejection")
	}
}

func TestTerminalWSAcceptsShellAndNoOrigin(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "m", Task: "t", Agent: "pi", Branch: "orch/m", Worktree: t.TempDir()}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	srv := httptest.NewServer(NewDaemonServerFull(fleet, NewJournal(), nil, nil, NewTerminalHub()))
	t.Cleanup(srv.Close)

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/sessions/m/terminal"
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	// The desktop shell (tauri origin) connects...
	conn, _, err := websocket.Dial(ctx, wsURL, &websocket.DialOptions{
		HTTPHeader: http.Header{"Origin": []string{"http://tauri.localhost"}},
	})
	if err != nil {
		t.Fatalf("tauri origin rejected: %v", err)
	}
	conn.Close(websocket.StatusNormalClosure, "")

	// ...and so does a non-browser client with no Origin at all.
	conn2, _, err := websocket.Dial(ctx, wsURL, nil)
	if err != nil {
		t.Fatalf("origin-less dial rejected: %v", err)
	}
	conn2.Close(websocket.StatusNormalClosure, "")
}

func TestDaemonRejectsForeignHostHeaders(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServer(NewFleet(), NewJournal(), nil))
	t.Cleanup(srv.Close)

	// A rebound host (evil.com → 127.0.0.1) must not read the daemon.
	req, err := http.NewRequest(http.MethodGet, srv.URL+"/board", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Host = "evil.example"

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("rebound host status = %d, want 403", resp.StatusCode)
	}
}

func TestDaemonAcceptsLoopbackHosts(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServer(NewFleet(), NewJournal(), nil))
	t.Cleanup(srv.Close)

	for _, host := range []string{"127.0.0.1:8094", "localhost:8094", strings.TrimPrefix(srv.URL, "http://")} {
		req, err := http.NewRequest(http.MethodGet, srv.URL+"/board", nil)
		if err != nil {
			t.Fatalf("new request: %v", err)
		}
		req.Host = host
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("do %q: %v", host, err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("host %q status = %d, want 200", host, resp.StatusCode)
		}
	}
}
