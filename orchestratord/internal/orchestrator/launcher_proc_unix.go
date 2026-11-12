//go:build !windows

package orchestrator

import (
	"os"
	"os/exec"
	"syscall"
)

// killProcessTree kills the worker's process group (Setpgid at start).
// It must only run for a live process — callers check state under l.mu,
// because Cmd.Wait writes ProcessState concurrently.
func killProcessTree(cmd *exec.Cmd, process *os.Process) error {
	_ = cmd
	if -process.Pid != 0 {
		return syscall.Kill(-process.Pid, syscall.SIGKILL)
	}
	return process.Kill()
}

// closeWorkerPipes force-closes the worker's stdout/stderr so blocked
// scanners return even if an orphan child still holds the descriptor.
func closeWorkerPipes(cmd *exec.Cmd) {
	if f, ok := cmd.Stdout.(*os.File); ok {
		_ = f.Close()
	}
}
