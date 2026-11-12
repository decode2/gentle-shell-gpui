package orchestrator

import (
	"context"
	"errors"
	"os/exec"
	"strings"
	"testing"
	"time"
)

func launcherFixture(t *testing.T) (*Fleet, *Router, *Journal, *Launcher) {
	t.Helper()
	repo := initRepo(t)
	fleet := NewFleet()
	router := NewRouter(2)
	journal := NewJournal()
	launcher := &Launcher{
		Fleet:       fleet,
		Router:      router,
		Journal:     journal,
		Worktrees:   &WorktreeManager{Repo: repo, Parent: t.TempDir()},
		Base:        "main",
		ReceiptGate: true,
	}
	return fleet, router, journal, launcher
}

func TestLauncherStartsWorkerInItsOwnWorktree(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	fleet, _, journal, launcher := launcherFixture(t)
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "echo worker-output")
	}

	session := Session{ID: "metering", Task: "Meter API calls", Agent: "pi", Branch: "orch/metering"}
	if err := launcher.StartTask(context.Background(), session); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}

	stored, err := fleet.Get("metering")
	if err != nil {
		t.Fatalf("Get(): %v", err)
	}
	if stored.Worktree == "" {
		t.Fatal("session has no worktree assigned")
	}

	// The worker finishes; the journal records launch, output, and exit.
	deadline := time.Now().Add(2 * time.Second)
	var entries []JournalEntry
	for {
		entries = journal.Get("metering")
		if len(entries) >= 3 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("journal never recorded the run: %+v", entries)
		}
		time.Sleep(10 * time.Millisecond)
	}
	if entries[0].Kind != "launched" || entries[1].Kind != "started" {
		t.Fatalf("journal head = %+v, want launched then started", entries[:2])
	}
	sawOutput := false
	for _, e := range entries {
		if e.Kind == "output" && strings.Contains(e.Text, "worker-output") {
			sawOutput = true
		}
	}
	if !sawOutput {
		t.Fatalf("journal = %+v, want the worker output", entries)
	}
	var sawExit bool
	for _, e := range entries {
		if e.Kind == "exited" {
			sawExit = true
		}
	}
	if !sawExit {
		t.Fatalf("journal has no exited entry: %+v", entries)
	}

	lane, err := fleet.Lane("metering")
	if err != nil || lane != LaneWorking {
		t.Fatalf("lane after exit = %q (err %v), want working", lane, err)
	}
}

func TestLauncherRefusesProtectedBranch(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, _, journal, launcher := launcherFixture(t)

	err := launcher.StartTask(context.Background(), Session{ID: "bad", Task: "t", Agent: "pi", Branch: "main"})
	if !errors.Is(err, ErrProtectedBranch) {
		t.Fatalf("StartTask() error = %v, want ErrProtectedBranch", err)
	}
	if got := len(journal.Get("bad")); got != 0 {
		t.Fatalf("journal recorded %d entries for a refused launch", got)
	}
}

func TestLauncherRejectsDuplicateSession(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, _, _, launcher := launcherFixture(t)
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "sleep 30")
	}

	session := Session{ID: "metering", Task: "t", Agent: "pi", Branch: "orch/metering"}
	if err := launcher.StartTask(context.Background(), session); err != nil {
		t.Fatalf("first StartTask(): %v", err)
	}
	err := launcher.StartTask(context.Background(), session)
	if !errors.Is(err, ErrDuplicateSession) {
		t.Fatalf("second StartTask() error = %v, want ErrDuplicateSession", err)
	}
}

func TestLauncherRoutesWorkerQuestions(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, router, _, launcher := launcherFixture(t)
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "echo 'QUESTION: Postmark or SES for the invoice email?'")
	}
	inbox := router.Register("invoice")

	if err := launcher.StartTask(context.Background(), Session{ID: "invoice", Task: "t", Agent: "pi", Branch: "orch/invoice"}); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}

	ev := receive(t, inbox, 2*time.Second)
	if ev.Kind != EventQuestion || !strings.Contains(ev.Note, "Postmark or SES") {
		t.Fatalf("routed event = %+v, want the worker question", ev)
	}
}

func TestLauncherStopTerminatesWorker(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, _, journal, launcher := launcherFixture(t)
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "sleep 30")
	}

	session := Session{ID: "long", Task: "t", Agent: "pi", Branch: "orch/long"}
	if err := launcher.StartTask(context.Background(), session); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}
	if err := launcher.Stop("long"); err != nil {
		t.Fatalf("Stop(): %v", err)
	}

	deadline := time.Now().Add(2 * time.Second)
	for {
		entries := journal.Get("long")
		last := entries[len(entries)-1]
		if last.Kind == "receipt" { // freeze runs right after the stop entry
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("journal never recorded the stop: %+v", entries)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// startScript builds a worker process that runs the shell script inside the
// session's worktree; the launcher test suite uses it as the Command impl.
func startScript(ctx context.Context, dir, script string) *workProcess {
	cmd := exec.CommandContext(ctx, "sh", "-c", script)
	cmd.Dir = dir
	return &workProcess{cmd: cmd}
}

func TestLauncherRootCancelKillsWorkers(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	fleet, _, journal, launcher := launcherFixture(t)
	root, cancelRoot := context.WithCancel(context.Background())
	launcher.Root = root
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "echo started; sleep 30")
	}

	if err := launcher.StartTask(context.Background(), Session{ID: "long", Task: "t", Agent: "pi", Branch: "orch/long"}); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}
	if _, err := fleet.Lane("long"); err != nil {
		t.Fatalf("session not registered: %v", err)
	}

	cancelRoot()

	// The worker must die with the daemon's lifecycle, not run 30 seconds.
	deadline := time.Now().Add(3 * time.Second)
	for {
		entries := journal.Get("long")
		last := entries[len(entries)-1]
		if last.Kind == "exited" || last.Kind == "stopped" || last.Kind == "receipt" {
			return // the freeze entry always trails the exit record
		}
		if time.Now().After(deadline) {
			t.Fatalf("worker outlived the lifecycle context: %+v", entries)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
