package orchestrator

import (
	"context"
	"os/exec"
	"runtime"
	"strings"
	"sync"
	"time"
)

// KnownAgent is a coding agent CLI the launcher knows how to run. Detection
// scans this list against the machine's PATH.
type KnownAgent struct {
	// Command is the executable name to look for.
	Command string
	// Agent is the id used in POST /sessions (agent field).
	Agent string
	// DisplayName is what the UI shows: the product the CLI belongs to
	// (gentle-pi's pi is Gentle Shell, not "pi").
	DisplayName string
}

// knownAgents is the detection roster: the CLIs the fleet can drive today
// (headless forms in agentArgTemplates, extended by the adapters phase).
var knownAgents = []KnownAgent{
	{Command: "pi", Agent: "pi", DisplayName: "Gentle Shell"},
	{Command: "claude", Agent: "claude", DisplayName: "Claude Code"},
	{Command: "codex", Agent: "codex", DisplayName: "Codex"},
	{Command: "gemini", Agent: "gemini", DisplayName: "Gemini CLI"},
	{Command: "opencode", Agent: "opencode", DisplayName: "OpenCode"},
	{Command: "aider", Agent: "aider", DisplayName: "Aider"},
	{Command: "cursor-agent", Agent: "cursor", DisplayName: "Cursor"},
	{Command: "qwen", Agent: "qwen", DisplayName: "Qwen Code"},
	{Command: "kimi", Agent: "kimi", DisplayName: "Kimi CLI"},
	{Command: "crush", Agent: "crush", DisplayName: "Crush"},
	{Command: "amp", Agent: "amp", DisplayName: "Amp"},
	{Command: "goose", Agent: "goose", DisplayName: "Goose"},
}

// DetectedAgent is one roster entry resolved against this machine.
type DetectedAgent struct {
	Agent       string `json:"agent"`
	Command     string `json:"command"`
	DisplayName string `json:"displayName"`
	Path        string `json:"path,omitempty"`
	Version     string `json:"version,omitempty"`
	Found       bool   `json:"found"`
}

// Prober resolves one command name against the machine: path and version.
// Tests replace it; the default uses PATH and a short --version call.
type Prober func(command string) (path, version string)

// DefaultProber looks the command up on PATH and asks --version with a short
// timeout. Any failure yields an empty path and version.
func DefaultProber(command string) (string, string) {
	path, err := exec.LookPath(command)
	if err != nil {
		return "", ""
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	// Version probes happen at app open on Windows, where npm shims would
	// flash a console window per probe — the detection is silent everywhere,
	// and .cmd/.bat shims resolve through cmd /c exactly like the launcher.
	invocation := agentInvocation(path, runtime.GOOS)
	probe := exec.CommandContext(ctx, invocation.exe, append(invocation.prefix, "--version")...)
	applyProcessPlatformTraits(probe)
	out, err := probe.Output()
	if err != nil {
		return path, ""
	}
	return path, strings.TrimSpace(strings.SplitN(string(out), "\n", 2)[0])
}

// AgentDetector resolves the known roster against the machine and caches the
// result briefly: the board asks on every form open without re-probing.
type AgentDetector struct {
	prober Prober
	ttl    time.Duration

	mu         sync.Mutex
	last       []DetectedAgent
	cachedAt   time.Time
	rescanning bool
}

// NewAgentDetector returns a detector using prober (nil = default) with the
// given cache TTL (<= 0 falls back to 30 seconds).
func NewAgentDetector(prober Prober, ttl time.Duration) *AgentDetector {
	if prober == nil {
		prober = DefaultProber
	}
	if ttl <= 0 {
		ttl = 30 * time.Second
	}
	return &AgentDetector{prober: prober, ttl: ttl}
}

// Detect returns the roster resolved against this machine, from cache when
// it is fresh. force bypasses the cache.
func (d *AgentDetector) Detect(force bool) []DetectedAgent {
	d.mu.Lock()
	if !force && d.last != nil && time.Since(d.cachedAt) < d.ttl {
		out := d.last
		d.mu.Unlock()
		return out
	}
	d.mu.Unlock()

	agents := make([]DetectedAgent, 0, len(knownAgents))
	for _, known := range knownAgents {
		path, version := d.prober(known.Command)
		agents = append(agents, DetectedAgent{
			Agent:       known.Agent,
			Command:     known.Command,
			DisplayName: known.DisplayName,
			Path:        path,
			Version:     version,
			Found:       path != "",
		})
	}

	d.mu.Lock()
	d.last = agents
	d.cachedAt = time.Now()
	d.mu.Unlock()
	return agents
}
