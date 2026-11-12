package orchestrator

import (
	"bufio"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestDaemonLiveBoardStreamsBoard(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "metering", Task: "Meter API calls", Agent: "claude-code", Branch: "orch/metering-api"}); err != nil {
		t.Fatalf("Start(): %v", err)
	}

	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, srv.URL+"/board/live", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("GET /board/live: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", resp.StatusCode)
	}

	reader := bufio.NewReader(resp.Body)
	line, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read stream: %v", err)
	}
	if !strings.Contains(line, "event: board") {
		t.Fatalf("first stream line = %q, want the board event header", line)
	}
}

func TestDaemonUnknownMethodIsRejected(t *testing.T) {
	fleet := NewFleet()
	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	resp, err := http.Post(srv.URL+"/board", "application/json", strings.NewReader("{}"))
	if err != nil {
		t.Fatalf("POST /board: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusMethodNotAllowed {
		t.Fatalf("status = %d, want 405", resp.StatusCode)
	}
}

func TestServeWiresWatcherAndDaemon(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{{
		{Number: 322, Branch: "orch/metering-api", Open: true,
			Checks: []Check{{Name: "ci", Status: CheckPassed}}},
	}}}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	handle, err := Serve(ctx, ServeOptions{
		ListenAddr:   "127.0.0.1:0",
		Source:       source,
		PollInterval: 10 * time.Millisecond,
	})
	if err != nil {
		t.Fatalf("Serve(): %v", err)
	}

	// Register a worker through the HTTP surface.
	body := `{"id":"metering","task":"Meter API calls","agent":"claude-code","branch":"orch/metering-api"}`
	resp, err := http.Post("http://"+handle.Addr+"/sessions", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST /sessions: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("POST /sessions status = %d, want 201", resp.StatusCode)
	}

	// The watcher polls Adopt the PR facts into the lane within a few ticks.
	deadline := time.Now().Add(2 * time.Second)
	for {
		boardResp, err := http.Get("http://" + handle.Addr + "/board")
		if err != nil {
			t.Fatalf("GET /board: %v", err)
		}
		var board map[Lane][]SessionCard
		err = json.NewDecoder(boardResp.Body).Decode(&board)
		boardResp.Body.Close()
		if err != nil {
			t.Fatalf("decode board: %v", err)
		}
		if len(board[LaneInReview]) == 1 && board[LaneInReview][0].ID == "metering" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("board never showed metering in review: %+v", board)
		}
		time.Sleep(20 * time.Millisecond)
	}

	cancel()
	select {
	case <-handle.Done:
	case <-time.After(5 * time.Second):
		t.Fatal("Serve did not drain after cancel")
	}
}

// TestServeShutdownKillsAllWorkers proves the app-close guarantee: canceling
// the daemon's context leaves no worker processes behind — not even
// grandchildren that inherited the output pipes.
func TestServeShutdownKillsAllWorkers(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes and git")
	}
	repo := initRepo(t)
	token := "gate-kill-token-7fd2"
	alive := func() bool {
		out, err := exec.Command("pgrep", "-f", token).CombinedOutput()
		return err == nil && strings.TrimSpace(string(out)) != ""
	}

	ctx, cancel := context.WithCancel(context.Background())
	handle, err := Serve(ctx, ServeOptions{
		ListenAddr:   "127.0.0.1:0",
		Source:       &fakeSource{states: [][]PRSnapshot{nil}},
		PollInterval: time.Hour,
		RepoPath:     repo,
		ConfigPath:   filepath.Join(t.TempDir(), "orchestrator.json"),
		Command: func(ctx context.Context, s Session, dir string) *workProcess {
			return startScript(ctx, dir, "sleep 30 # "+token)
		},
	})
	if err != nil {
		t.Fatalf("Serve(): %v", err)
	}

	body := `{"id":"long","task":"t","agent":"pi","branch":"orch/long","launch":true,"project":"core"}`
	resp, err := http.Post("http://"+handle.Addr+"/projects", "application/json",
		strings.NewReader(`{"id":"core","repo":"o/c","repoPath":"`+repo+`"}`))
	if err != nil {
		t.Fatalf("POST /projects: %v", err)
	}
	resp.Body.Close()
	resp, err = http.Post("http://"+handle.Addr+"/sessions", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST /sessions: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("launch status = %d, want 201", resp.StatusCode)
	}

	deadline := time.Now().Add(3 * time.Second)
	for !alive() {
		if time.Now().After(deadline) {
			t.Fatal("worker process never appeared")
		}
		time.Sleep(20 * time.Millisecond)
	}

	cancel()
	select {
	case <-handle.Done:
	case <-time.After(5 * time.Second):
		t.Fatal("Serve did not drain")
	}

	deadline = time.Now().Add(5 * time.Second)
	for alive() {
		if time.Now().After(deadline) {
			t.Fatal("worker process survived the daemon shutdown")
		}
		time.Sleep(50 * time.Millisecond)
	}
}
