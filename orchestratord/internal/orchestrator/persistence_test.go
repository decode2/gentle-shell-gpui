package orchestrator

import (
	"context"
	"encoding/json"
	"net/http"
	"os"
	"testing"
	"time"
)

func TestSessionStoreRoundTrip(t *testing.T) {
	store := &SessionStore{path: t.TempDir() + "/sessions.json"}

	saved := []PersistedSession{
		{Session: Session{ID: "m", Task: "Medir API", Agent: "pi", Branch: "orch/m", Project: "core", Base: "main"}},
		{Session: Session{ID: "w", Task: "Escribir", Agent: "codex", Branch: "orch/w", Project: "core", Base: "main"},
			Summary: "listo con tests verdes"},
	}
	if err := store.Save(saved); err != nil {
		t.Fatalf("Save(): %v", err)
	}

	loaded, err := store.Load()
	if err != nil {
		t.Fatalf("Load(): %v", err)
	}
	if len(loaded) != 2 || loaded[1].Summary != "listo con tests verdes" {
		t.Fatalf("loaded = %+v, want the saved sessions with summaries", loaded)
	}
}

func TestSessionStoreLoadMissingFileIsEmpty(t *testing.T) {
	store := &SessionStore{path: t.TempDir() + "/nope.json"}
	loaded, err := store.Load()
	if err != nil {
		t.Fatalf("Load(): %v", err)
	}
	if loaded != nil && len(loaded) != 0 {
		t.Fatalf("loaded = %+v, want empty", loaded)
	}
}

func TestSessionStoreCorruptFileTolerated(t *testing.T) {
	path := t.TempDir() + "/sessions.json"
	if err := os.WriteFile(path, []byte("{not-json"), 0o644); err != nil {
		t.Fatal(err)
	}
	store := &SessionStore{path: path}
	loaded, err := store.Load()
	if err != nil {
		t.Fatalf("corrupt file must not fail Load: %v", err)
	}
	if loaded != nil && len(loaded) != 0 {
		t.Fatalf("loaded = %+v, want empty on corrupt file", loaded)
	}
}

func TestServePersistSessionsAcrossBoot(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns server")
	}
	sessionsPath := t.TempDir() + "/sessions.json"
	store := &SessionStore{path: sessionsPath}
	_ = store.Save([]PersistedSession{{Session: Session{
		ID: "metering", Task: "previous run", Agent: "pi", Branch: "orch/metering", Project: "core", Base: "main",
	}}})

	preLoad, preErr := store.Load()
	if preErr != nil || len(preLoad) != 1 {
		t.Fatalf("pre-Serve Load: %d items, err=%v — the store lost the session before Serve", len(preLoad), preErr)
	}

	source := &fakeSource{states: [][]PRSnapshot{nil}}
	handle, err := Serve(context.Background(), ServeOptions{
		ListenAddr:   "127.0.0.1:0",
		Source:       source,
		PollInterval: time.Hour,
		ConfigPath:   t.TempDir() + "/orchestrator.json",
		SessionsPath: sessionsPath,
	})
	if err != nil {
		t.Fatalf("Serve(): %v", err)
	}

	if handle.Rehydrated != 1 {
		t.Fatalf("Serve rehydrated %d sessions, want 1 — the load or the registration dropped it", handle.Rehydrated)
	}
	resp, err := http.Get("http://" + handle.Addr + "/board")
	if err != nil {
		t.Fatalf("GET /board: %v", err)
	}
	defer resp.Body.Close()
	var board map[Lane][]SessionCard
	if err := json.NewDecoder(resp.Body).Decode(&board); err != nil {
		t.Fatalf("decode: %v", err)
	}
	found := false
	for _, card := range board[LaneWorking] {
		if card.ID == "metering" && card.Task == "previous run" {
			found = true
		}
	}
	if !found {
		t.Fatalf("persisted session missing from the booted board: %+v", board)
	}
}
