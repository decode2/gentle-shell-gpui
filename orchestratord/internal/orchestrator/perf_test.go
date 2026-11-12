package orchestrator

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestJournalCapsPerSessionHistory(t *testing.T) {
	journal := NewJournal()
	for i := 0; i < maxJournalEntriesPerSession+500; i++ {
		journal.Append("m", "output", "line")
	}
	if got := len(journal.Get("m")); got != maxJournalEntriesPerSession {
		t.Fatalf("journal kept %d entries, want the cap %d", got, maxJournalEntriesPerSession)
	}
}

func TestJournalKeepsOldestAfterCap(t *testing.T) {
	journal := NewJournal()
	journal.Append("m", "launched", "first")
	for i := 0; i < maxJournalEntriesPerSession; i++ {
		journal.Append("m", "output", "filler")
	}
	entries := journal.Get("m")
	if entries[0].Kind != "launched" || entries[0].Text != "first" {
		t.Fatalf("the launch entry was dropped: %+v", entries[0])
	}
	if last := entries[len(entries)-1]; last.Text != "filler" {
		t.Fatalf("latest entry lost: %+v", last)
	}
}

func TestEvidenceJournalComesCappedAndTagged(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git")
	}
	repo := initRepo(t)
	fleet := NewFleet()
	journal := NewJournal()
	launcher := &Launcher{
		Fleet:       fleet,
		Router:      NewRouter(2),
		Journal:     journal,
		Worktrees:   &WorktreeManager{Repo: repo, Parent: t.TempDir()},
		Base:        "main",
		ReceiptGate: true,
	}
	srv := httptest.NewServer(NewDaemonServerFull(fleet, journal, launcher, nil, nil))
	t.Cleanup(srv.Close)

	if err := fleet.Start(Session{ID: "m", Task: "t", Agent: "pi", Branch: "orch/m"}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	// Overfill beyond the cap; the evidence must carry the tail with a
	// truncation marker, not the whole history.
	for i := 0; i < maxJournalEntriesPerSession+50; i++ {
		journal.Append("m", "output", strings.Repeat("x", 120))
	}
	journal.Append("m", "summary", "hecho")

	res, err := http.Get(srv.URL + "/sessions/m/evidence")
	if err != nil {
		t.Fatalf("GET evidence: %v", err)
	}
	defer res.Body.Close()
	var ev Evidence
	if err := json.NewDecoder(res.Body).Decode(&ev); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if len(ev.Journal) > maxEvidenceJournalEntries {
		t.Fatalf("evidence journal = %d entries, want capped at %d", len(ev.Journal), maxEvidenceJournalEntries)
	}
	last := ev.Journal[len(ev.Journal)-1]
	if last.Kind != "summary" {
		t.Fatalf("evidence tail lost the latest entry: %+v", last)
	}
}

func TestRouterDroppedEventsAreCounted(t *testing.T) {
	router := NewRouter(2)
	inbox := router.Register("m")

	// Fill the inbox beyond capacity with questions (no budget consumed).
	for i := 0; i < inboxCapacity+5; i++ {
		_ = router.Route(Event{Kind: EventQuestion, Session: "m", Note: "q"})
	}
	if got := len(inbox); got != inboxCapacity {
		t.Fatalf("inbox = %d, want %d", got, inboxCapacity)
	}
	if got := router.Dropped("m"); got != 5 {
		t.Fatalf("Dropped() = %d, want 5", got)
	}

	// A successful deliver reschedules nothing but the counter persists.
	<-inbox
	if got := router.Dropped("m"); got != 5 {
		t.Fatalf("Dropped() reset itself: %d", got)
	}
}

func TestWatcherStopsRoutingAfterCancel(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{{
		{Number: 322, Branch: "orch/m", Open: true, Checks: []Check{{Name: "ci", Status: CheckFailed}}},
	}}}
	fleet := NewFleet()
	router := NewRouter(100)
	inbox := router.Register("m")
	_ = fleet.Start(Session{ID: "m", Task: "t", Agent: "pi", Branch: "orch/m"})

	ctx, cancel := context.WithCancel(context.Background())
	watcher := NewWatcher(fleet, router, source, time.Hour)
	watcher.Poll(ctx) // baseline
	cancel()          // daemon is going down

	// Polls after cancel must not route anything: the fleet is dying.
	err := watcher.Poll(ctx)
	if err == nil || !errors.Is(err, context.Canceled) {
		t.Fatalf("Poll() after cancel = %v, want context.Canceled", err)
	}
	if got := len(inbox); got != 0 {
		t.Fatalf("routed events after cancel: %d", got)
	}
}
