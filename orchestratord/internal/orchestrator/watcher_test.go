package orchestrator

import (
	"context"
	"errors"
	"testing"
	"time"
)

// fakeSource is a scripted PullSource: each Fetch consumes the next state.
type fakeSource struct {
	states  [][]PRSnapshot
	fetches int
	err     error
}

func (f *fakeSource) Fetch(context.Context) ([]PRSnapshot, error) {
	if f.fetches < len(f.states) {
		state := f.states[f.fetches]
		f.fetches++
		return state, nil
	}
	if f.err != nil {
		return nil, f.err
	}
	if len(f.states) == 0 {
		return nil, nil
	}
	return f.states[len(f.states)-1], nil
}

func newWatcherFixture(t *testing.T, source PullSource, maxFixCycles int) (*Fleet, *Router, *Watcher, <-chan Event) {
	t.Helper()

	fleet := NewFleet()
	router := NewRouter(maxFixCycles)
	inbox := router.Register("metering")
	if err := fleet.Start(Session{
		ID:     "metering",
		Task:   "Meter API calls per workspace",
		Agent:  "claude-code",
		Branch: "orch/metering-api",
	}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	watcher := NewWatcher(fleet, router, source, time.Hour)
	return fleet, router, watcher, inbox
}

func TestWatcherFirstPollAdoptsFactsWithoutEvents(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{{
		{Number: 322, Branch: "orch/metering-api", Open: true,
			Checks: []Check{{Name: "e2e (chrome)", Status: CheckRunning}}},
	}}}
	fleet, _, watcher, inbox := newWatcherFixture(t, source, 2)

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("Poll() error = %v, want nil", err)
	}
	assertLane(t, fleet, "metering", LaneInReview)

	select {
	case ev := <-inbox:
		t.Fatalf("first poll routed unexpected event %q", ev.Kind)
	default:
	}
}

func TestWatcherRoutesCheckFailedToOwningSession(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{
		{
			{Number: 322, Branch: "orch/metering-api", Open: true,
				Checks: []Check{{Name: "e2e (chrome)", Status: CheckRunning}}},
		},
		{
			{Number: 322, Branch: "orch/metering-api", Open: true,
				Checks: []Check{{Name: "e2e (chrome)", Status: CheckFailed, LogURL: "https://ci/log/1"}}},
		},
	}}
	fleet, _, watcher, inbox := newWatcherFixture(t, source, 2)

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("first Poll() error = %v, want nil", err)
	}
	select {
	case ev := <-inbox:
		t.Fatalf("baseline poll routed unexpected event %q", ev.Kind)
	default:
	}

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("second Poll() error = %v, want nil", err)
	}

	ev := receive(t, inbox, time.Second)
	if ev.Kind != EventCheckFailed || ev.Check.Name != "e2e (chrome)" || ev.Check.LogURL != "https://ci/log/1" {
		t.Fatalf("routed event = %+v, want the failed check", ev)
	}
	assertLane(t, fleet, "metering", LaneNeedsYou)
}

func TestWatcherRoutesApprovalWhenReady(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{
		{
			{Number: 322, Branch: "orch/metering-api", Open: true,
				Checks:  []Check{{Name: "ci", Status: CheckPassed}},
				Reviews: []Review{{Reviewer: "reviewer-agent", State: ReviewPending}}},
		},
		{
			{Number: 322, Branch: "orch/metering-api", Open: true,
				Checks:  []Check{{Name: "ci", Status: CheckPassed}},
				Reviews: []Review{{Reviewer: "reviewer-agent", State: ReviewApproved}}},
		},
	}}
	fleet, _, watcher, inbox := newWatcherFixture(t, source, 2)

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("first Poll() error = %v, want nil", err)
	}
	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("second Poll() error = %v, want nil", err)
	}

	ev := receive(t, inbox, time.Second)
	if ev.Kind != EventApproved || ev.PR != 322 {
		t.Fatalf("routed event = %+v, want approval of PR 322", ev)
	}
	assertLane(t, fleet, "metering", LaneReadyToMerge)
}

func TestWatcherRoutesReviewCommentOnChangesRequested(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{
		{
			{Number: 322, Branch: "orch/metering-api", Open: true,
				Checks: []Check{{Name: "ci", Status: CheckPassed}}},
		},
		{
			{Number: 322, Branch: "orch/metering-api", Open: true,
				Checks:  []Check{{Name: "ci", Status: CheckPassed}},
				Reviews: []Review{{Reviewer: "nnishant", State: ReviewChangesRequested, Thread: "rate-limit discussion"}}},
		},
	}}
	_, _, watcher, inbox := newWatcherFixture(t, source, 2)

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("first Poll() error = %v, want nil", err)
	}
	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("second Poll() error = %v, want nil", err)
	}

	ev := receive(t, inbox, time.Second)
	if ev.Kind != EventReviewComment || ev.Review.Thread != "rate-limit discussion" {
		t.Fatalf("routed event = %+v, want the review thread", ev)
	}
}

func TestWatcherIgnoresPullRequestsWithoutSession(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{{
		{Number: 999, Branch: "human/own-branch", Open: true,
			Checks: []Check{{Name: "ci", Status: CheckFailed}}},
	}}}
	fleet, router, watcher, inbox := newWatcherFixture(t, source, 2)
	router.Register("metering")

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("Poll() error = %v, want nil", err)
	}
	if got := fleet.Count(); got != 1 {
		t.Fatalf("fleet grew to %d sessions, want 1", got)
	}
	select {
	case ev := <-inbox:
		t.Fatalf("routed unexpected event %q", ev.Kind)
	default:
	}
}

func TestWatcherSurfacesFixBudgetExhaustion(t *testing.T) {
	failing := []PRSnapshot{
		{Number: 322, Branch: "orch/metering-api", Open: true,
			Checks: []Check{{Name: "e2e", Status: CheckFailed, LogURL: "https://ci/log"}}},
	}
	source := &fakeSource{states: [][]PRSnapshot{failing, failing, failing}}
	_, _, watcher, inbox := newWatcherFixture(t, source, 1)

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("baseline Poll() error = %v, want nil", err)
	}
	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("first fix cycle Poll() error = %v, want nil", err)
	}
	receive(t, inbox, time.Second)

	err := watcher.Poll(context.Background())
	if !errors.Is(err, ErrFixBudgetExhausted) {
		t.Fatalf("second fix cycle Poll() error = %v, want ErrFixBudgetExhausted", err)
	}
	if got := len(inbox); got != 0 {
		t.Fatalf("escalated events still delivered: %d", got)
	}
}

func TestWatcherSourceErrorKeepsLastState(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{{
		{Number: 322, Branch: "orch/metering-api", Open: true,
			Checks: []Check{{Name: "ci", Status: CheckPassed}}},
	}}, err: errors.New("gh unavailable")}
	fleet, _, watcher, _ := newWatcherFixture(t, source, 2)

	if err := watcher.Poll(context.Background()); err != nil {
		t.Fatalf("baseline Poll() error = %v, want nil", err)
	}

	// Third state is an error: nothing changes.
	if err := watcher.Poll(context.Background()); err == nil {
		t.Fatal("Poll() error = nil, want the source error")
	}
	assertLane(t, fleet, "metering", LaneInReview)
}

func TestWatcherRunStopsOnContextCancel(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{nil}}
	_, _, watcher, _ := newWatcherFixture(t, source, 2)

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- watcher.Run(ctx) }()

	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("Run() error = %v, want context.Canceled", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Run() did not stop after context cancel")
	}
}

func TestWatcherConcurrentPollsAreRaceFree(t *testing.T) {
	source := &fakeSource{states: [][]PRSnapshot{
		{{Number: 322, Branch: "orch/metering-api", Open: true,
			Checks: []Check{{Name: "ci", Status: CheckPassed}}}},
	}}
	fleet, _, watcher, _ := newWatcherFixture(t, source, 2)

	done := make(chan struct{})
	go func() {
		defer close(done)
		for i := 0; i < 20; i++ {
			if err := watcher.Poll(context.Background()); err != nil {
				t.Errorf("Poll() error = %v", err)
				return
			}
		}
	}()
	for i := 0; i < 20; i++ {
		if _, err := fleet.Lane("metering"); err != nil {
			t.Fatalf("Lane() error = %v", err)
		}
	}
	<-done
}
