//go:build windows

package orchestrator

import (
	"os/exec"
	"syscall"
)

// createNoWindow is Windows' CREATE_NO_WINDOW: console subprocesses (npm
// shims through cmd, agent CLIs) start without flashing a console/PowerShell
// window over the app.
const createNoWindow = 0x08000000

// applyProcessPlatformTraits on Windows: workers inherit the daemon's job
// object (the desktop shell creates it with KILL_ON_JOB_CLOSE), and console
// windows never appear — the fleet is a board, not a window farm.
func applyProcessPlatformTraits(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.HideWindow = true
	cmd.SysProcAttr.CreationFlags = createNoWindow
}

// applyInteractiveTraits: no attributes needed on Windows.
func applyInteractiveTraits(*exec.Cmd) {}
