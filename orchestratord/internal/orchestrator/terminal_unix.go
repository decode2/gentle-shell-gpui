//go:build !windows

package orchestrator

import (
	"context"
	"errors"
	"os"
	"os/exec"

	"github.com/creack/pty"
)

var errNoWorktree = errors.New("session has no accessible worktree")

func statDir(dir string) error {
	_, err := os.Stat(dir)
	return err
}

// newSessionTerminal starts the interactive shell under a Unix PTY.
func newSessionTerminal(ctx context.Context, dir string) (*sessionTerminal, error) {
	shell := os.Getenv("SHELL")
	if shell == "" {
		shell = "/bin/sh"
	}
	cmd := exec.CommandContext(ctx, shell, "-i")
	cmd.Dir = dir
	applyInteractiveTraits(cmd)

	ptmx, err := pty.Start(cmd)
	if err != nil {
		return nil, err
	}

	term := &sessionTerminal{io: ptmx, exited: make(chan struct{})}
	go func() {
		_ = cmd.Wait()
		close(term.exited)
	}()
	return term, nil
}
