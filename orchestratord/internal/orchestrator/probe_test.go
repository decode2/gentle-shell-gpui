package orchestrator

import (
	"testing"
)

// npm shims (.cmd/.bat) must resolve through cmd /c on Windows: a direct
// spawn is not executable, so agents installed via npm were never detected.
func TestDefaultProberWrapsShimsLikeTheLauncher(t *testing.T) {
	resolved := agentInvocation(`C:\Users\x\AppData\Roaming\npm\pi.cmd`, "windows")
	if resolved.exe != "cmd" || len(resolved.prefix) != 2 || resolved.prefix[0] != "/c" {
		t.Fatalf("shim resolution = %+v, want cmd /c", resolved)
	}
}
