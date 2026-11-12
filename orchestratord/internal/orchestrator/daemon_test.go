package orchestrator

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestDaemonBoardEndpointMirrorsFleet(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "metering", Task: "Meter API calls", Agent: "claude-code", Branch: "orch/metering-api"}); err != nil {
		t.Fatalf("Start(): %v", err)
	}

	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	resp, err := http.Get(srv.URL + "/board")
	if err != nil {
		t.Fatalf("GET /board: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("GET /board status = %d, want 200", resp.StatusCode)
	}
	var board map[Lane][]SessionCard
	if err := json.NewDecoder(resp.Body).Decode(&board); err != nil {
		t.Fatalf("decode board: %v", err)
	}
	if len(board[LaneWorking]) != 1 || board[LaneWorking][0].ID != "metering" {
		t.Fatalf("board = %+v, want metering in working", board)
	}
}

func TestDaemonStartSessionRegisters(t *testing.T) {
	fleet := NewFleet()
	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	body := []byte(`{"id":"chart","task":"Chart usage","agent":"codex","branch":"orch/usage-chart"}`)
	resp, err := http.Post(srv.URL+"/sessions", "application/json", bytes.NewReader(body))
	if err != nil {
		t.Fatalf("POST /sessions: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("POST /sessions status = %d, want 201", resp.StatusCode)
	}

	if got := fleet.Count(); got != 1 {
		t.Fatalf("fleet count = %d, want 1", got)
	}

	// A duplicate registration is rejected as a conflict.
	resp2, err := http.Post(srv.URL+"/sessions", "application/json", bytes.NewReader(body))
	if err != nil {
		t.Fatalf("duplicate POST: %v", err)
	}
	defer resp2.Body.Close()
	if resp2.StatusCode != http.StatusConflict {
		t.Fatalf("duplicate POST status = %d, want 409", resp2.StatusCode)
	}
}

func TestDaemonMalformedBodyIsBadRequest(t *testing.T) {
	fleet := NewFleet()
	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	resp, err := http.Post(srv.URL+"/sessions", "application/json", strings.NewReader("{not-json"))
	if err != nil {
		t.Fatalf("POST /sessions: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", resp.StatusCode)
	}
	if got := fleet.Count(); got != 0 {
		t.Fatalf("fleet count = %d, want 0 after rejected request", got)
	}
}
