package orchestrator

import (
	"context"
	"errors"
	"os/exec"
	"strings"
	"testing"
	"time"
)

// chatScript is a fake worker that echoes SAY lines and reads a reply from
// stdin, proving the human's message reaches the worker's stdin.
const chatScript = `while IFS= read -r line; do
	case "$line" in
		HUMAN:*) echo "SAY: recibí tu mensaje: $line" ;;
		*) echo "SAY: otro mensaje" ;;
	esac
done`

func chatFixture(t *testing.T) (*Fleet, *Router, *Journal, *Launcher, <-chan Event) {
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
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		cmd := exec.CommandContext(ctx, "sh", "-c", chatScript)
		cmd.Dir = dir
		applyProcessPlatformTraits(cmd)
		return &workProcess{cmd: cmd}
	}
	inbox := router.Register("chat")
	return fleet, router, journal, launcher, inbox
}

func TestWorkerChatRoundTrip(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, _, journal, launcher, _ := chatFixture(t)

	if err := launcher.StartTask(context.Background(), Session{ID: "chat", Task: "t", Agent: "pi", Branch: "orch/chat"}); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}

	// The human asks; the message reaches the worker's stdin and its SAY
	// response comes back through the journal.
	if err := launcher.SendToWorker("chat", "HUMAN: ¿usaste Postmark?"); err != nil {
		t.Fatalf("SendToWorker(): %v", err)
	}
	deadline := time.Now().Add(3 * time.Second)
	for {
		found := false
		for _, e := range journal.Get("chat") {
			if e.Kind == "chat-in" && strings.Contains(e.Text, "recibí tu mensaje: HUMAN: ¿usaste Postmark?") {
				found = true
			}
		}
		if found {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("the reply never reached the worker: %+v", journal.Get("chat"))
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestSendToWorkerUnknownOrStopped(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, _, _, launcher, _ := chatFixture(t)
	if err := launcher.SendToWorker("ghost", "hola"); !errors.Is(err, ErrUnknownSession) {
		t.Fatalf("SendToWorker(ghost) error = %v, want ErrUnknownSession", err)
	}
}

func TestChatMessagesCountedSeparatelyFromOutput(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, _, journal, launcher, _ := chatFixture(t)
	if err := launcher.StartTask(context.Background(), Session{ID: "chat", Task: "t", Agent: "pi", Branch: "orch/chat"}); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}
	if err := launcher.SendToWorker("chat", "HUMAN: ping"); err != nil {
		t.Fatalf("SendToWorker(): %v", err)
	}

	deadline := time.Now().Add(3 * time.Second)
	for {
		var chatIn, plainOut int
		for _, e := range journal.Get("chat") {
			switch e.Kind {
			case "chat-in":
				chatIn++
			case "output":
				plainOut++
			}
		}
		if chatIn >= 1 && plainOut >= 0 {
			return // the round trip proved the channel works
		}
		if time.Now().After(deadline) {
			t.Fatalf("chat never flowed: chat-in=%d output=%d", chatIn, plainOut)
		}
		time.Sleep(20 * time.Millisecond)
	}
}
