package orchestrator

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestDaemonSessionEventsEndpoint(t *testing.T) {
	fleet := NewFleet()
	journal := NewJournal()
	srv := httptest.NewServer(NewDaemonServer(fleet, journal, nil))
	t.Cleanup(srv.Close)

	body := `{"id":"metering","task":"t","agent":"pi","branch":"orch/metering"}`
	resp, err := http.Post(srv.URL+"/sessions", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST /sessions: %v", err)
	}
	resp.Body.Close()

	// No entries yet: an empty list, not an error.
	res, err := http.Get(srv.URL + "/sessions/metering/events")
	if err != nil {
		t.Fatalf("GET events: %v", err)
	}
	var entries []JournalEntry
	if err := json.NewDecoder(res.Body).Decode(&entries); err != nil {
		t.Fatalf("decode events: %v", err)
	}
	res.Body.Close()
	if len(entries) != 0 {
		t.Fatalf("events = %+v, want empty", entries)
	}

	journal.Append("metering", "output", "hello from the worker")
	res, err = http.Get(srv.URL + "/sessions/metering/events")
	if err != nil {
		t.Fatalf("GET events: %v", err)
	}
	defer res.Body.Close()
	if err := json.NewDecoder(res.Body).Decode(&entries); err != nil {
		t.Fatalf("decode events: %v", err)
	}
	if len(entries) != 1 || entries[0].Text != "hello from the worker" || entries[0].Kind != "output" {
		t.Fatalf("events = %+v, want the recorded entry", entries)
	}
}

func TestDaemonStopWithoutLauncherIsBadRequest(t *testing.T) {
	fleet := NewFleet()
	srv := httptest.NewServer(NewDaemonServer(fleet, NewJournal(), nil))
	t.Cleanup(srv.Close)

	resp, err := http.Post(srv.URL+"/sessions/metering/stop", "application/json", nil)
	if err != nil {
		t.Fatalf("POST stop: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", resp.StatusCode)
	}
}

func TestDaemonLaunchEndpointSpawnsWorker(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes and git worktrees")
	}

	repo := initRepo(t)
	fleet := NewFleet()
	router := NewRouter(2)
	journal := NewJournal()
	launcher := &Launcher{
		Fleet:     fleet,
		Router:    router,
		Journal:   journal,
		Worktrees: &WorktreeManager{Repo: repo, Parent: t.TempDir()},
		Base:      "main",
		Command: func(ctx context.Context, s Session, dir string) *workProcess {
			return startScript(ctx, dir, "echo launched-line")
		},
	}
	srv := httptest.NewServer(NewDaemonServer(fleet, journal, launcher))
	t.Cleanup(srv.Close)

	body := `{"id":"metering","task":"Meter API calls","agent":"pi","branch":"orch/metering","launch":true}`
	resp, err := http.Post(srv.URL+"/sessions", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST launch: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("launch status = %d, want 201", resp.StatusCode)
	}

	// The worker runs in its own worktree and its output lands in the journal.
	deadline := time.Now().Add(2 * time.Second)
	for {
		entries := journal.Get("metering")
		var sawOutput bool
		for _, e := range entries {
			if e.Kind == "output" && strings.Contains(e.Text, "launched-line") {
				sawOutput = true
			}
		}
		if sawOutput {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("journal never recorded the launched worker: %+v", entries)
		}
		time.Sleep(10 * time.Millisecond)
	}

	// A launch on a protected branch is refused with 400.
	resp, err = http.Post(srv.URL+"/sessions", "application/json",
		strings.NewReader(`{"id":"evil","task":"t","agent":"pi","branch":"main","launch":true}`))
	if err != nil {
		t.Fatalf("POST protected launch: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("protected launch status = %d, want 400", resp.StatusCode)
	}
}
