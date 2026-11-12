package orchestrator

import (
	"context"
	"os/exec"
	"testing"
	"time"
)

func TestDbgStopLive(t *testing.T) {
	_, _, journal, launcher := launcherFixture(t)
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "sleep 30")
	}
	if err := launcher.StartTask(context.Background(), Session{ID: "long", Task: "t", Agent: "pi", Branch: "orch/long"}); err != nil {
		t.Fatal(err)
	}
	time.Sleep(200 * time.Millisecond)
	out, _ := exec.Command("sh", "-c", "ps -ef | grep 'sleep 30' | grep -v grep").CombinedOutput()
	t.Logf("ALIVE?: %s", out)
	err := launcher.Stop("long")
	t.Logf("STOP-ERR: %v", err)
	_ = journal
}
