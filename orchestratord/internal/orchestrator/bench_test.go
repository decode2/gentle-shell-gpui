package orchestrator

import (
	"context"
	"fmt"
	"testing"
	"time"
)

// staticSource serves one fixed snapshot list for benchmarks.
type staticSource struct{ snapshots []PRSnapshot }

func (s *staticSource) Fetch(context.Context) ([]PRSnapshot, error) { return s.snapshots, nil }

const timeHour = time.Hour

// BenchmarkBoardWithLargeFleet measures the full board projection with many
// sessions across projects — the desktop UI polls and streams this payload.
func BenchmarkBoardWithLargeFleet(b *testing.B) {
	fleet := NewFleet()
	approved := pr(true, []Check{{Name: "ci", Status: CheckPassed}}, []Review{{Reviewer: "r", State: ReviewApproved}}, false)
	for i := 0; i < 200; i++ {
		id := SessionID(fmt.Sprintf("worker-%03d", i))
		_ = fleet.Start(Session{ID: id, Task: "task text for worker", Agent: "pi", Branch: fmt.Sprintf("orch/bench-%03d", i), Project: fmt.Sprintf("proj-%d", i%5)})
		_, _ = fleet.Update(id, func(f *Facts) {
			if i%4 == 0 {
				f.PR = approved
				f.ReceiptRequired = true
				f.ReceiptValid = i%8 == 0
			} else {
				f.Activity.Running = true
				f.Activity.Detail = "running tests"
			}
		})
	}
	b.ResetTimer()
	for b.Loop() {
		board := fleet.Board()
		if len(board) == 0 {
			b.Fatal("empty board")
		}
	}
}

// BenchmarkWatchedPollFlood measures one poll over a repository pretending to
// have 200 open pull requests: snapshot absorption, lane derivation, and the
// journal mirroring for every worker.
func BenchmarkWatcherPollFlood(b *testing.B) {
	fleet := NewFleet()
	router := NewRouter(1_000_000) // the flood measures routing work, not budget exhaustion
	for i := 0; i < 200; i++ {
		id := SessionID(fmt.Sprintf("w%03d", i))
		_ = fleet.Start(Session{ID: id, Task: "t", Agent: "pi", Branch: fmt.Sprintf("orch/bench-%03d", i)})
		inbox := router.Register(id)
		_ = inbox
	}
	var snapshots []PRSnapshot
	for i := 0; i < 200; i++ {
		snapshots = append(snapshots, PRSnapshot{
			Number: 3000 + i,
			Branch: fmt.Sprintf("orch/bench-%03d", i),
			Open:   true,
			Checks: []Check{{Name: "ci", Status: CheckFailed, LogURL: "https://ci/log"}},
			Reviews: []Review{
				{Reviewer: "reviewer-a", State: ReviewApproved},
				{Reviewer: "reviewer-b", State: ReviewPending},
			},
		})
	}
	source := &staticSource{snapshots: snapshots}
	watcher := NewWatcher(fleet, router, source, timeHour)
	ctx := context.Background()

	b.ResetTimer()
	for b.Loop() {
		if err := watcher.Poll(ctx); err != nil {
			b.Fatal(err)
		}
		b.StopTimer()
		for _, id := range fleetIDs(fleet) {
			inbox := router.Register(id)
			for len(inbox) > 0 {
				<-inbox
			}
		}
		b.StartTimer()
	}
}

// fleetIDs lists the fleet's session ids for inbox draining.
func fleetIDs(fleet *Fleet) []SessionID {
	ids := make([]SessionID, 0, 200)
	for _, lane := range fleet.Board() {
		for _, card := range lane {
			ids = append(ids, SessionID(card.ID))
		}
	}
	return ids
}

// BenchmarkJournalFlood measures the capped append path under a chatty
// worker: every append past the cap pays the slide, the newest always win.
func BenchmarkJournalFlood(b *testing.B) {
	journal := NewJournal()
	b.ResetTimer()
	for b.Loop() {
		journal.Append("m", "output", "0123456789012345678901234567890123456789")
	}
	_ = journal.Get("m")
}

// BenchmarkRenderBoard measures the headless text rendering of the big
// board (CLI --print-board path).
func BenchmarkRenderBoard(b *testing.B) {
	fleet := NewFleet()
	for i := 0; i < 200; i++ {
		_ = fleet.Start(Session{ID: SessionID(fmt.Sprintf("w%03d", i)), Task: "t", Agent: "pi", Branch: fmt.Sprintf("orch/%03d", i)})
	}
	board := fleet.Board()
	b.ResetTimer()
	for b.Loop() {
		out := RenderBoard(board)
		if out == "" {
			b.Fatal("empty render")
		}
	}
}
