package orchestrator

import (
	"errors"
	"fmt"
	"os/exec"
	"path/filepath"
	"strings"
)

// ErrProtectedBranch is returned when a worktree operation would create or
// manage a branch that must never be owned by a worker.
var ErrProtectedBranch = errors.New("orchestrator: refusing worktree for protected branch")

// ErrInvalidBranch is returned for branch names a worker must never own.
var ErrInvalidBranch = errors.New("orchestrator: invalid branch name")

// WorktreeManager owns worker worktrees under one parent directory. It never
// touches protected branches: the merge-authority rule forbids workers from
// owning main, so the manager refuses such branches outright.
type WorktreeManager struct {
	// Repo is the path of the main checkout that owns the git directory.
	Repo string
	// Parent is the directory under which worktrees are created.
	Parent string
}

// Create adds a git worktree for branch based on base, inside Parent, and
// returns the worktree path.
func (m *WorktreeManager) Create(branch, base string) (string, error) {
	if err := validateBranch(branch); err != nil {
		return "", err
	}

	path := filepath.Join(m.Parent, worktreeDirName(branch))
	out, err := git(m.Repo, "worktree", "add", "-b", branch, path, base)
	if err != nil {
		return "", fmt.Errorf("orchestrator: create worktree %s: %w: %s", branch, err, out)
	}
	return path, nil
}

// Remove removes the worktree attached to branch. The branch itself is kept:
// its pull request may still be open and the evidence must survive.
func (m *WorktreeManager) Remove(branch string) error {
	if err := validateBranch(branch); err != nil {
		return err
	}

	path := filepath.Join(m.Parent, worktreeDirName(branch))
	out, err := git(m.Repo, "worktree", "remove", path)
	if err != nil {
		return fmt.Errorf("orchestrator: remove worktree %s: %w: %s", branch, err, out)
	}
	return nil
}

// validateBranch refuses names that are protected, unsafe, or not usable as
// a worktree directory name.
func validateBranch(branch string) error {
	switch branch {
	case "main", "master", "HEAD":
		return ErrProtectedBranch
	}
	if branch == "" || strings.ContainsAny(branch, " \t\n") ||
		strings.Contains(branch, "..") || strings.HasPrefix(branch, "-") {
		return ErrInvalidBranch
	}
	for _, part := range strings.Split(branch, "/") {
		if part == "" || part == "." || part == ".." {
			return ErrInvalidBranch
		}
	}
	return nil
}

// worktreeDirName maps a branch like orch/metering-api to a flat directory
// name like orch-metering-api.
func worktreeDirName(branch string) string {
	return strings.ReplaceAll(branch, "/", "-")
}

func git(repo string, args ...string) (string, error) {
	out, err := exec.Command("git", append([]string{"-C", repo}, args...)...).CombinedOutput()
	return string(out), err
}
