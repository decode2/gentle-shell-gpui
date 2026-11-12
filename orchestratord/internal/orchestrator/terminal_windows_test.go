//go:build windows

package orchestrator

import (
	"strings"
	"testing"
)

// The Windows terminal must anchor the shell in the session worktree, not
// in the daemon's cwd (audit W5).
func TestTerminalCommandLineAnchorsWorktree(t *testing.T) {
	dir := `C:\repos\project`
	commandLine := terminalCommandLine(dir)
	if !strings.Contains(commandLine, dir) {
		t.Fatalf("terminal command line %q lost the worktree", commandLine)
	}
	if !strings.Contains(commandLine, "cd /d") {
		t.Fatalf("terminal command line %q does not change drive+directory", commandLine)
	}
}
