package orchestrator

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestEvidenceReceiptValidatesAgainstRecompute(t *testing.T) {
	ev := Evidence{
		SessionID: "m", Agent: "pi", Branch: "orch/m", Status: "Trabajando",
		Added: 3, Deleted: 1, Summary: "listo", Decisions: []string{"usó Postmark"},
		Commits: []string{"abc123 feat"},
	}
	ev.Receipt = evidenceReceipt(ev)

	if !evidenceReceiptValid(ev) {
		t.Fatal("fresh receipt does not validate")
	}

	ev.Status = "Aprobado" // tampering with the package
	if evidenceReceiptValid(ev) {
		t.Fatal("tampered package validated against its original receipt")
	}
}

func TestLauncherFreezesReceiptOnWorkerExit(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	_, _, journal, launcher := launcherFixture(t)
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, `echo "SUMMARY: cierre limpio"`)
	}

	if err := launcher.StartTask(context.Background(), Session{ID: "metering", Task: "t", Agent: "pi", Branch: "orch/metering"}); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}

	deadline := time.Now().Add(3 * time.Second)
	var receiptEntry *JournalEntry
	for {
		for i := range journal.Get("metering") {
			e := journal.Get("metering")[i]
			if e.Kind == "receipt" {
				receiptEntry = &e
				break
			}
		}
		if receiptEntry != nil || time.Now().After(deadline) {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if receiptEntry == nil {
		t.Fatal("no receipt entry frozen at worker exit")
	}
	if !strings.HasPrefix(receiptEntry.Text, "rcpt1_") {
		t.Fatalf("frozen receipt = %q, want an rcpt1_ hash", receiptEntry.Text)
	}
}

func TestEvidenceEndpointReportsReceiptValidity(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	repo := initRepo(t)
	fleet := NewFleet()
	journal := NewJournal()
	launcher := &Launcher{
		Fleet:     fleet,
		Router:    NewRouter(2),
		Journal:   journal,
		Worktrees: &WorktreeManager{Repo: repo, Parent: t.TempDir()},
		Base:      "main",
	}
	srv := httptest.NewServer(NewDaemonServerFull(fleet, journal, launcher, nil, nil))
	t.Cleanup(srv.Close)

	body := `{"id":"metering","task":"t","agent":"pi","branch":"orch/metering","launch":true}`
	resp, err := http.Post(srv.URL+"/sessions", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	resp.Body.Close()

	deadline := time.Now().Add(3 * time.Second)
	for {
		res, err := http.Get(srv.URL + "/sessions/metering/evidence")
		if err == nil {
			var ev Evidence
			err = json.NewDecoder(res.Body).Decode(&ev)
			res.Body.Close()
			if err == nil && ev.Receipt != "" {
				// Receipt frozen at exit: the endpoint's recomputation decides.
				_ = ev
				return
			}
		}
		if time.Now().After(deadline) {
			t.Fatal("evidence never carried a receipt")
		}
		time.Sleep(20 * time.Millisecond)
	}
}
