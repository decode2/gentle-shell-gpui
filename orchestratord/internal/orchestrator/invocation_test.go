package orchestrator

import (
	"context"
	"runtime"
	"strings"
	"testing"
)

func TestAgentInvocationWrapsNpmShimsOnWindows(t *testing.T) {
	tests := []struct {
		name      string
		resolved  string
		goos      string
		wantExe   string
		wantFirst []string
	}{
		{
			name:      "windows npm shim runs through cmd",
			resolved:  `C:\Users\x\AppData\Roaming\npm\pi.cmd`,
			goos:      "windows",
			wantExe:   "cmd",
			wantFirst: []string{"/c", `C:\Users\x\AppData\Roaming\npm\pi.cmd`},
		},
		{
			name:      "windows batch shim runs through cmd",
			resolved:  `C:\tools\claude.bat`,
			goos:      "windows",
			wantExe:   "cmd",
			wantFirst: []string{"/c", `C:\tools\claude.bat`},
		},
		{
			name:      "windows exe runs directly",
			resolved:  `C:\tools\codex.exe`,
			goos:      "windows",
			wantExe:   `C:\tools\codex.exe`,
			wantFirst: nil,
		},
		{
			name:      "unix binary runs directly even with .cmd-like name",
			resolved:  "/usr/local/bin/pi",
			goos:      "linux",
			wantExe:   "/usr/local/bin/pi",
			wantFirst: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			invocation := agentInvocation(tt.resolved, tt.goos)
			if invocation.exe != tt.wantExe {
				t.Fatalf("exe = %q, want %q", invocation.exe, tt.wantExe)
			}
			if len(invocation.prefix) != len(tt.wantFirst) {
				t.Fatalf("prefix = %v, want %v", invocation.prefix, tt.wantFirst)
			}
			for i := range tt.wantFirst {
				if invocation.prefix[i] != tt.wantFirst[i] {
					t.Fatalf("prefix = %v, want %v", invocation.prefix, tt.wantFirst)
				}
			}
		})
	}
}

func TestDefaultAgentCommandUsesInvocation(t *testing.T) {
	original := resolveAgentExecutable
	t.Cleanup(func() { resolveAgentExecutable = original })
	resolveAgentExecutable = func(string) string { return "/usr/local/bin/pi" }

	s := Session{ID: "m", Task: "hacer algo", Agent: "pi", Branch: "orch/m"}
	wp := defaultAgentCommand(context.Background(), s, "/tmp")
	if wp.cmd.Path != "/usr/local/bin/pi" {
		t.Fatalf("command path = %q, want the resolved binary", wp.cmd.Path)
	}
	joined := strings.Join(wp.cmd.Args, " ")
	if !strings.Contains(joined, "hacer algo") {
		t.Fatalf("args = %q, want the task text", joined)
	}
	if runtime.GOOS == "linux" && !strings.Contains(joined, "ODD") {
		t.Fatalf("args = %q, want the ODD worker prompt", joined)
	}
}

func TestAgentInvocationResolvesThroughLookPath(t *testing.T) {
	original := resolveAgentExecutable
	t.Cleanup(func() { resolveAgentExecutable = original })
	resolveAgentExecutable = func(command string) string {
		if command == "pi" {
			return "/usr/local/bin/pi"
		}
		return ""
	}

	wp := defaultAgentCommand(context.Background(), Session{ID: "m", Task: "t", Agent: "pi"}, "/tmp")
	if wp.cmd.Path != "/usr/local/bin/pi" {
		t.Fatalf("path = %q, want the LookPath result", wp.cmd.Path)
	}
}
