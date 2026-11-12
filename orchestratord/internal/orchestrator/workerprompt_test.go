package orchestrator

import (
	"context"
	"strings"
	"testing"
	"time"
)

func TestWorkerPromptCarriesTaskAndODDContract(t *testing.T) {
	prompt := workerPrompt(Session{ID: "metering", Task: "Medir llamadas API", Agent: "pi", Branch: "orch/metering"})

	for _, want := range []string{
		"Medir llamadas API",
		"orch/metering",
		"## Fases", // ODD workflow phases
		"## Verificación",
		"DECISION:",
		"SUMMARY:",
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("worker prompt missing %q:\n%s", want, prompt)
		}
	}
	if len(prompt) > 4000 {
		t.Fatalf("worker prompt is %d bytes; the headless brief must stay compact", len(prompt))
	}
}

func TestLauncherCapturesDecisionsAndSummary(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	fake := &fakeEngram{}
	srv := newFakeEngramServer(fake)
	t.Cleanup(srv.Close)

	_, _, journal, launcher := launcherFixture(t)
	launcher.Engram = &EngramBridge{BaseURL: srv.URL, Project: "gentle-ai"}
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, `echo "DECISION: Postmark porque ya está en package.json"; echo "SUMMARY: metering listo con receipts"`)
	}

	if err := launcher.StartTask(context.Background(), Session{ID: "metering", Task: "t", Agent: "pi", Branch: "orch/metering"}); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}

	deadline := time.Now().Add(2 * time.Second)
	for {
		entries := journal.Get("metering")
		var decisions, summary int
		for _, e := range entries {
			if e.Kind == "decision" {
				decisions++
			}
			if e.Kind == "summary" {
				summary++
			}
		}
		if decisions >= 1 && summary >= 1 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("journal never captured decision/summary: %+v", entries)
		}
		time.Sleep(10 * time.Millisecond)
	}

	// Decisions publish to fleet memory without waiting for exit.
	fake.mu.Lock()
	defer fake.mu.Unlock()
	published := false
	for _, obs := range fake.observations {
		if content, _ := obs["content"].(string); strings.Contains(content, "Postmark") {
			published = true
		}
	}
	if !published {
		t.Fatalf("decision never reached Engram: %+v", fake.observations)
	}
}
