package orchestrator

import (
	"errors"
	"testing"
)

// TestTwoWorkersEndToEnd drives the Phase 0 spike scenario end to end:
// two workers in their own worktrees, one full feedback loop each, and the
// fix-budget escalation. It only exercises in-process behavior (no git, no
// network), so it is safe under -short.
func TestTwoWorkersEndToEnd(t *testing.T) {
	repo := initRepo(t)
	mgr := &WorktreeManager{Repo: repo, Parent: t.TempDir()}

	wtMetering, err := mgr.Create("orch/metering-api", "main")
	if err != nil {
		t.Fatalf("worktree for metering: %v", err)
	}
	wtChart, err := mgr.Create("orch/usage-chart", "main")
	if err != nil {
		t.Fatalf("worktree for chart: %v", err)
	}

	fleet := NewFleet()
	router := NewRouter(2)
	inboxMetering := router.Register("metering")
	router.Register("chart")

	if err := fleet.Start(Session{
		ID:       "metering",
		Task:     "Meter API calls per workspace",
		Agent:    "claude-code",
		Branch:   "orch/metering-api",
		Worktree: wtMetering,
	}); err != nil {
		t.Fatalf("Start(metering): %v", err)
	}
	if err := fleet.Start(Session{
		ID:       "chart",
		Task:     "Chart usage on the billing page",
		Agent:    "codex",
		Branch:   "orch/usage-chart",
		Worktree: wtChart,
	}); err != nil {
		t.Fatalf("Start(chart): %v", err)
	}

	assertLane(t, fleet, "metering", LaneWorking)
	assertLane(t, fleet, "chart", LaneWorking)

	// Metering asks the human a question, then continues after the answer.
	if _, err := fleet.Update("metering", func(f *Facts) {
		f.Activity.Running = true
		f.Activity.AwaitingHuman = true
		f.Activity.Question = "Meter per workspace or per user?"
	}); err != nil {
		t.Fatalf("metering question: %v", err)
	}
	assertLane(t, fleet, "metering", LaneNeedsYou)

	if err := router.Route(Event{Kind: EventQuestion, Session: "metering", Note: "Meter per workspace or per user?"}); err != nil {
		t.Fatalf("route question: %v", err)
	}
	if got := (<-inboxMetering).Kind; got != EventQuestion {
		t.Fatalf("metering inbox got %q, want %q", got, EventQuestion)
	}

	if _, err := fleet.Update("metering", func(f *Facts) {
		f.Activity.AwaitingHuman = false
		f.Activity.Question = ""
	}); err != nil {
		t.Fatalf("metering resumed: %v", err)
	}
	assertLane(t, fleet, "metering", LaneWorking)

	// Metering opens its PR: CI runs, then e2e fails in chrome.
	if _, err := fleet.Update("metering", func(f *Facts) {
		f.PR = &PullRequest{Number: 322, Open: true, Checks: []Check{{Name: "e2e (chrome)", Status: CheckRunning}}}
	}); err != nil {
		t.Fatalf("metering PR opened: %v", err)
	}
	assertLane(t, fleet, "metering", LaneInReview)

	failedCheck := Check{Name: "e2e (chrome)", Status: CheckFailed, LogURL: "https://ci/log/1"}
	if err := router.Route(Event{Kind: EventCheckFailed, Session: "metering", PR: 322, Check: failedCheck}); err != nil {
		t.Fatalf("route failed check: %v", err)
	}
	if _, err := fleet.Update("metering", func(f *Facts) {
		f.PR.Checks = []Check{failedCheck}
	}); err != nil {
		t.Fatalf("metering check failed: %v", err)
	}
	assertLane(t, fleet, "metering", LaneNeedsYou)

	// The owning worker pushes a fix; checks go green and the PR is approved.
	if err := router.Route(Event{Kind: EventFixPushed, Session: "metering", PR: 322, Note: "export.ts +14 -3"}); err != nil {
		t.Fatalf("route fix pushed: %v", err)
	}
	if _, err := fleet.Update("metering", func(f *Facts) {
		f.PR.Checks = []Check{{Name: "e2e (chrome)", Status: CheckPassed}}
		f.PR.Reviews = []Review{{Reviewer: "reviewer-agent", State: ReviewApproved}}
	}); err != nil {
		t.Fatalf("metering fixed: %v", err)
	}
	assertLane(t, fleet, "metering", LaneReadyToMerge)

	// Meanwhile chart went straight to approved without incidents.
	if _, err := fleet.Update("chart", func(f *Facts) {
		f.PR = &PullRequest{Number: 331, Open: true,
			Checks:  []Check{{Name: "ci", Status: CheckPassed}},
			Reviews: []Review{{Reviewer: "nnishant", State: ReviewApproved}}}
	}); err != nil {
		t.Fatalf("chart PR approved: %v", err)
	}
	assertLane(t, fleet, "chart", LaneReadyToMerge)

	groups := fleet.ByLane()
	if len(groups[LaneReadyToMerge]) != 2 {
		t.Fatalf("ready-to-merge lane = %v, want both sessions", groups[LaneReadyToMerge])
	}

	// Budget escalation: the checkout worker burns its 2 fix cycles and stops.
	inboxCheckout := router.Register("checkout")
	if err := fleet.Start(Session{
		ID:     "checkout",
		Task:   "Fix the flaky checkout test",
		Agent:  "opencode",
		Branch: "orch/checkout-flake",
	}); err != nil {
		t.Fatalf("Start(checkout): %v", err)
	}
	for cycle := 1; cycle <= 2; cycle++ {
		if err := router.Route(Event{Kind: EventCheckFailed, Session: "checkout", PR: 479}); err != nil {
			t.Fatalf("checkout cycle %d: %v", cycle, err)
		}
		<-inboxCheckout
	}
	err = router.Route(Event{Kind: EventCheckFailed, Session: "checkout", PR: 479})
	if !errors.Is(err, ErrFixBudgetExhausted) {
		t.Fatalf("checkout third failure: error = %v, want ErrFixBudgetExhausted", err)
	}
	assertNoEvent(t, inboxCheckout)

	if _, err := fleet.Update("checkout", func(f *Facts) {
		f.PR = &PullRequest{Number: 479, Open: true,
			Checks:  []Check{{Name: "e2e", Status: CheckFailed}},
			Reviews: []Review{{Reviewer: "reviewer-agent", State: ReviewChangesRequested}}}
	}); err != nil {
		t.Fatalf("checkout facts: %v", err)
	}
	assertLane(t, fleet, "checkout", LaneNeedsYou)
}

func assertLane(t *testing.T, fleet *Fleet, id SessionID, want Lane) {
	t.Helper()
	lane, err := fleet.Lane(id)
	if err != nil {
		t.Fatalf("Lane(%s) error = %v", id, err)
	}
	if lane != want {
		t.Fatalf("Lane(%s) = %q, want %q", id, lane, want)
	}
}
