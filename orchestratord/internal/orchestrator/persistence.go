package orchestrator

import (
	"encoding/json"
	"os"
	"path/filepath"
	"sync"
	"time"
)

// PersistedSession is one session's durable state: everything needed to
// bring the session back on a daemon restart — identity plus the worker's
// final summary. Facts (PR, CI) rehydrate from the next watcher poll; the
// journal rehydrates from the journal store.
type PersistedSession struct {
	Session Session `json:"session"`
	Summary string  `json:"summary,omitempty"`
}

// SessionStore persists the fleet's sessions beside the daemon config, with
// serialized writes and atomic swaps (same rules as the onboarding config).
type SessionStore struct {
	path string
	mu   sync.Mutex
}

// NewSessionStore returns a store anchoring sessions.json in dir.
func NewSessionStore(dir string) *SessionStore {
	return &SessionStore{path: filepath.Join(dir, "sessions.json")}
}

// Save writes the fleet's sessions atomically.
func (s *SessionStore) Save(sessions []PersistedSession) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	payload, err := json.MarshalIndent(sessions, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(s.path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(s.path, append(payload, '\n'), 0o644)
}

// Load reads the sessions from disk; a missing or corrupt file is an empty
// fleet history, never a daemon failure.
func (s *SessionStore) Load() ([]PersistedSession, error) {
	raw, err := os.ReadFile(s.path)
	if err != nil {
		return nil, nil // missing: nothing persisted yet
	}
	var sessions []PersistedSession
	if err := json.Unmarshal(raw, &sessions); err != nil {
		return nil, nil // corrupt: tolerate, history starts over
	}
	return sessions, nil
}

// persist_period and the drift guard listed below keep the store honest.
const persistEvery = 3 * time.Second
