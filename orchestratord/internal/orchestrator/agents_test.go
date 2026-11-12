package orchestrator

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestAgentDetectorReportsFoundAndMissing(t *testing.T) {
	var probed []string
	detector := NewAgentDetector(func(command string) (string, string) {
		probed = append(probed, command)
		switch command {
		case "pi":
			return "/usr/local/bin/pi", "pi 1.5.0"
		case "claude":
			return "/usr/local/bin/claude", "2.0.1 (Claude Code)"
		default:
			return "", ""
		}
	}, time.Minute)

	agents := detector.Detect(false)
	if len(agents) != len(knownAgents) {
		t.Fatalf("roster = %d agents, want %d", len(agents), len(knownAgents))
	}

	byAgent := make(map[string]DetectedAgent, len(agents))
	for _, a := range agents {
		byAgent[a.Agent] = a
	}
	pi := byAgent["pi"]
	if !pi.Found || pi.Path != "/usr/local/bin/pi" || pi.Version != "pi 1.5.0" {
		t.Fatalf("pi = %+v, want found with path and version", pi)
	}
	if pi.DisplayName != "Gentle Shell" {
		t.Fatalf("pi display name = %q, want Gentle Shell (the CLI is pi; the product is the shell)", pi.DisplayName)
	}
	claude := byAgent["claude"]
	if !claude.Found || claude.Version == "" {
		t.Fatalf("claude = %+v, want found with version", claude)
	}
	if byAgent["codex"].Found {
		t.Fatalf("codex = %+v, want not found", byAgent["codex"])
	}
	if len(probed) != len(knownAgents) {
		t.Fatalf("probed %d commands, want the full roster", len(probed))
	}
}

func TestAgentDetectorCachesUntilTTL(t *testing.T) {
	calls := 0
	detector := NewAgentDetector(func(string) (string, string) {
		calls++
		return "/bin/x", "1"
	}, time.Minute)

	detector.Detect(false)
	detector.Detect(false)
	if calls != len(knownAgents) {
		t.Fatalf("second Detect re-probed: %d calls, want cached", calls)
	}

	detector.Detect(true)
	if calls != 2*len(knownAgents) {
		t.Fatalf("force did not rescan: %d calls", calls)
	}
}

func TestDaemonAgentsEndpoint(t *testing.T) {
	detector := NewAgentDetector(func(command string) (string, string) {
		if command == "pi" {
			return "/usr/bin/pi", "pi 1.5.0"
		}
		return "", ""
	}, time.Minute)

	fleet := NewFleet()
	srv := httptest.NewServer(NewDaemonServerWithDetector(fleet, NewJournal(), nil, detector))
	t.Cleanup(srv.Close)

	resp, err := http.Get(srv.URL + "/agents")
	if err != nil {
		t.Fatalf("GET /agents: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", resp.StatusCode)
	}
	var agents []DetectedAgent
	if err := json.NewDecoder(resp.Body).Decode(&agents); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if len(agents) != len(knownAgents) {
		t.Fatalf("agents = %d, want the roster", len(agents))
	}
	for _, a := range agents {
		if a.Agent == "pi" && (!a.Found || a.Version != "pi 1.5.0") {
			t.Fatalf("pi entry = %+v", a)
		}
	}
}

func TestDaemonAgentsWithoutDetectorReturnsEmptyList(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServer(NewFleet(), NewJournal(), nil))
	t.Cleanup(srv.Close)

	resp, err := http.Get(srv.URL + "/agents")
	if err != nil {
		t.Fatalf("GET /agents: %v", err)
	}
	defer resp.Body.Close()
	var agents []DetectedAgent
	if err := json.NewDecoder(resp.Body).Decode(&agents); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if agents == nil || len(agents) != 0 {
		t.Fatalf("agents = %+v, want empty non-nil list", agents)
	}
}
