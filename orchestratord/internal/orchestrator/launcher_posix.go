//go:build !windows

package orchestrator

import (
	"os/exec"
	"syscall"
)

// applyProcessPlatformTraits ties each worker to the daemon's life on Unix:
// if the daemon dies for any reason, the kernel SIGKILLs its workers
// (PARENT_DEATH_SIGNAL), and workers run in their own process group so Stop
// kills the whole tree (agent CLIs spawn children that inherit the pipes).
func applyProcessPlatformTraits(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Pdeathsig = syscall.SIGKILL
	cmd.SysProcAttr.Setpgid = true
}

// applyInteractiveTraits suits the embedded interactive shell: daemon-death
// linkage without its own process group (PTY setup with Setpgid is not
// permitted in every sandbox and the shell dies with the hub anyway).
func applyInteractiveTraits(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Pdeathsig = syscall.SIGKILL
}
