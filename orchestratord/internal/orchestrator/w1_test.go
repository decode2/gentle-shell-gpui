package orchestrator

import (
	"encoding/json"
	"errors"
	"os"
	"sync"
	"testing"
)

// Two sessions claiming the same branch is an ambiguity the daemon must
// refuse at registration: a PR has exactly one owner.
func TestFleetRejectsDuplicateBranch(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "a", Task: "t", Agent: "pi", Branch: "orch/same"}); err != nil {
		t.Fatalf("first Start(): %v", err)
	}
	err := fleet.Start(Session{ID: "b", Task: "t", Agent: "codex", Branch: "orch/same"})
	if err == nil {
		t.Fatal("second Start() with the same branch succeeded; branch ownership is now ambiguous")
	}
	if !errors.Is(err, ErrDuplicateBranch) {
		t.Fatalf("error = %v, want ErrDuplicateBranch", err)
	}
}

// Concurrent config saves are serialized (W8): a torn write corrupts the
// daemon's onboarding file.
func TestConfigStoreConcurrentSavesAreSafe(t *testing.T) {
	store := &configStore{path: t.TempDir() + "/orchestrator.json"}
	var wg sync.WaitGroup
	for i := 0; i < 20; i++ {
		wg.Add(1)
		go func(n int) {
			defer wg.Done()
			_ = store.save(configRequest{Repo: "owner/repo", RepoPath: "", Listen: string(rune('a' + n%26))})
		}(i)
	}
	wg.Wait()
	var cfg configRequest
	raw, err := os.ReadFile(store.path)
	if err != nil {
		t.Fatalf("read after concurrent saves: %v", err)
	}
	if err := json.Unmarshal(raw, &cfg); err != nil {
		t.Fatalf("config file corrupted by concurrent saves: %v", err)
	}
}
