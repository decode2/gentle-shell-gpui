package orchestrator

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
)

// fakeEngram records the requests a test client sends.
type fakeEngram struct {
	mu           sync.Mutex
	sessions     []map[string]any
	observations []map[string]any
}

func (f *fakeEngram) handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /sessions", func(w http.ResponseWriter, r *http.Request) {
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		f.mu.Lock()
		f.sessions = append(f.sessions, body)
		f.mu.Unlock()
		w.WriteHeader(http.StatusCreated)
	})
	mux.HandleFunc("POST /observations", func(w http.ResponseWriter, r *http.Request) {
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		if title, _ := body["title"].(string); strings.TrimSpace(title) == "" {
			http.Error(w, "title required", http.StatusBadRequest)
			return
		}
		f.mu.Lock()
		f.observations = append(f.observations, body)
		f.mu.Unlock()
		w.WriteHeader(http.StatusCreated)
	})
	return mux
}

func TestEngramBridgeCreatesSessionAndPublishesOutcome(t *testing.T) {
	fake := &fakeEngram{}
	srv := httptest.NewServer(fake.handler())
	t.Cleanup(srv.Close)

	bridge := &EngramBridge{BaseURL: srv.URL, Project: "gentle-ai"}
	if err := bridge.StartSession(context.Background(), Session{ID: "metering", Task: "Meter API calls", Agent: "pi"}); err != nil {
		t.Fatalf("StartSession(): %v", err)
	}
	fake.mu.Lock()
	if len(fake.sessions) != 1 {
		t.Fatalf("sessions recorded = %d, want 1", len(fake.sessions))
	}
	if fake.sessions[0]["id"] != "metering" || fake.sessions[0]["project"] != "gentle-ai" {
		t.Fatalf("session body = %+v, want id+project", fake.sessions[0])
	}
	fake.mu.Unlock()

	err := bridge.PublishOutcome(context.Background(), "metering", "Meter API calls", "PR 322 approved; receipts recorded")
	if err != nil {
		t.Fatalf("PublishOutcome(): %v", err)
	}
	fake.mu.Lock()
	defer fake.mu.Unlock()
	if len(fake.observations) != 1 {
		t.Fatalf("observations recorded = %d, want 1", len(fake.observations))
	}
	obs := fake.observations[0]
	if obs["session_id"] != "metering" || obs["type"] != "decision" || obs["scope"] != "project" {
		t.Fatalf("observation = %+v, want session/type/scope", obs)
	}
	if title, _ := obs["title"].(string); !strings.Contains(title, "metering") {
		t.Fatalf("observation title = %q, want the session id", title)
	}
}

func TestEngramBridgeDegradesWhenServerDown(t *testing.T) {
	bridge := &EngramBridge{BaseURL: "http://127.0.0.1:1", Project: "gentle-ai"}
	if err := bridge.StartSession(context.Background(), Session{ID: "x", Task: "t", Agent: "pi"}); err != nil {
		t.Fatalf("StartSession() error = %v, want nil (degrade)", err)
	}
	if err := bridge.PublishOutcome(context.Background(), "x", "t", "done"); err != nil {
		t.Fatalf("PublishOutcome() error = %v, want nil (degrade)", err)
	}
}

func TestEngramBridgeEmptyOutcomeIsLocalNoop(t *testing.T) {
	fake := &fakeEngram{}
	srv := httptest.NewServer(fake.handler())
	t.Cleanup(srv.Close)

	bridge := &EngramBridge{BaseURL: srv.URL, Project: "gentle-ai"}
	if err := bridge.PublishOutcome(context.Background(), "metering", "t", "   "); err != nil {
		t.Fatalf("PublishOutcome(): %v", err)
	}
	fake.mu.Lock()
	defer fake.mu.Unlock()
	if len(fake.observations) != 0 {
		t.Fatalf("empty outcome still published: %+v", fake.observations)
	}
}

// newFakeEngramServer wraps the fake's handler in a test server.
func newFakeEngramServer(f *fakeEngram) *httptest.Server {
	return httptest.NewServer(f.handler())
}
