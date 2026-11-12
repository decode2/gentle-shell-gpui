package orchestrator

import (
	"errors"
	"os"
	"os/exec"
	"strings"
	"testing"
)

// initRepo creates a minimal git repository with one commit on its main branch
// and returns its path. The repo is cleaned up by t.TempDir().
func initRepo(t *testing.T) string {
	t.Helper()

	dir := t.TempDir()
	run := func(args ...string) {
		t.Helper()
		out, err := exec.Command("git", append([]string{"-C", dir}, args...)...).CombinedOutput()
		if err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}

	run("init", "-b", "main")
	run("config", "user.email", "spike@gentle.local")
	run("config", "user.name", "Orchestrator Spike")
	run("commit", "--allow-empty", "-m", "initial")
	return dir
}

func TestWorktreeCreateAndRemove(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git commands")
	}

	repo := initRepo(t)
	mgr := &WorktreeManager{Repo: repo, Parent: t.TempDir()}

	path, err := mgr.Create("orch/metering-api", "main")
	if err != nil {
		t.Fatalf("Create() error = %v, want nil", err)
	}
	if path == "" {
		t.Fatal("Create() path = empty, want the worktree path")
	}
	if _, statErr := os.Stat(path); statErr != nil {
		t.Fatalf("worktree path %q not created: %v", path, statErr)
	}

	// The worktree exists on disk and git knows the branch.
	out, err := exec.Command("git", "-C", repo, "worktree", "list", "--porcelain").CombinedOutput()
	if err != nil {
		t.Fatalf("git worktree list: %v\n%s", err, out)
	}
	if !containsLine(string(out), "refs/heads/orch/metering-api") {
		t.Fatalf("worktree list missing refs/heads/orch/metering-api:\n%s", out)
	}

	if err := mgr.Remove("orch/metering-api"); err != nil {
		t.Fatalf("Remove() error = %v, want nil", err)
	}

	out, err = exec.Command("git", "-C", repo, "worktree", "list", "--porcelain").CombinedOutput()
	if err != nil {
		t.Fatalf("git worktree list after remove: %v\n%s", err, out)
	}
	if containsLine(string(out), "refs/heads/orch/metering-api") {
		t.Fatalf("worktree for orch/metering-api still listed after Remove():\n%s", out)
	}
}

func TestWorktreeCreateRefusesProtectedBranch(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git commands")
	}

	repo := initRepo(t)
	mgr := &WorktreeManager{Repo: repo, Parent: t.TempDir()}

	for _, branch := range []string{"main", "master"} {
		path, err := mgr.Create(branch, "main")
		if !errors.Is(err, ErrProtectedBranch) {
			t.Fatalf("Create(%q) error = %v, want ErrProtectedBranch", branch, err)
		}
		if path != "" {
			t.Fatalf("Create(%q) path = %q, want empty on refusal", branch, path)
		}
	}
}

func TestWorktreeCreateRejectsUnsafeBranchName(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git commands")
	}

	repo := initRepo(t)
	mgr := &WorktreeManager{Repo: repo, Parent: t.TempDir()}

	for _, branch := range []string{"-oProxyCommand=rm", "a/../escape", "", "has space"} {
		if _, err := mgr.Create(branch, "main"); err == nil {
			t.Fatalf("Create(%q) succeeded, want validation error", branch)
		} else if errors.Is(err, ErrProtectedBranch) {
			t.Fatalf("Create(%q) returned ErrProtectedBranch, want branch-name validation error", branch)
		}
	}
}

func containsLine(haystack, needle string) bool {
	for _, line := range strings.Split(haystack, "\n") {
		if strings.Contains(line, needle) {
			return true
		}
	}
	return false
}
