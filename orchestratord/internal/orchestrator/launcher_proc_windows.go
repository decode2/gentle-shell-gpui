//go:build windows

package orchestrator

import (
	"os"
	"os/exec"
)

// killProcessTree on Windows: the daemon's Job Object (KILL_ON_JOB_CLOSE)
// bound by the desktop shell covers the whole tree; the direct kill remains
// for headless runs.
func killProcessTree(cmd *exec.Cmd, process *os.Process) error {
	return process.Kill()
}

func closeWorkerPipes(*exec.Cmd) {}
