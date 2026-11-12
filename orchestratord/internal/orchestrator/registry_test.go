package orchestrator

import (
	"errors"
	"testing"
)

func TestFleetStartRegistersSessionAsWorking(t *testing.T) {
	fleet := NewFleet()

	err := fleet.Start(Session{
		ID:     "metering",
		Task:   "Meter API calls per workspace",
		Agent:  "claude-code",
		Branch: "orch/metering-api",
	})
	if err != nil {
		t.Fatalf("Start() error = %v, want nil", err)
	}

	lane, err := fleet.Lane("metering")
	if err != nil {
		t.Fatalf("Lane() error = %v, want nil", err)
	}
	if lane != LaneWorking {
		t.Fatalf("Lane() = %q, want %q", lane, LaneWorking)
	}
	if got := fleet.Count(); got != 1 {
		t.Fatalf("Count() = %d, want 1", got)
	}
}

func TestFleetStartRejectsDuplicateSession(t *testing.T) {
	fleet := NewFleet()
	session := Session{ID: "metering", Task: "t", Agent: "claude-code", Branch: "orch/metering-api"}

	if err := fleet.Start(session); err != nil {
		t.Fatalf("first Start() error = %v, want nil", err)
	}
	err := fleet.Start(session)
	if !errors.Is(err, ErrDuplicateSession) {
		t.Fatalf("second Start() error = %v, want ErrDuplicateSession", err)
	}
}

func TestFleetUpdateDerivesLaneFromFacts(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "metering", Task: "t", Agent: "claude-code", Branch: "orch/metering-api"}); err != nil {
		t.Fatalf("Start() error = %v, want nil", err)
	}

	lane, err := fleet.Update("metering", func(f *Facts) {
		f.PR = pr(true, nil, nil, false)
	})
	if err != nil {
		t.Fatalf("Update() error = %v, want nil", err)
	}
	if lane != LaneInReview {
		t.Fatalf("Update() lane = %q, want %q", lane, LaneInReview)
	}

	lane, err = fleet.Update("metering", func(f *Facts) {
		f.PR.Checks = []Check{{Name: "ci", Status: CheckFailed, LogURL: "https://ci/log"}}
	})
	if err != nil {
		t.Fatalf("Update() error = %v, want nil", err)
	}
	if lane != LaneNeedsYou {
		t.Fatalf("Update() lane = %q, want %q", lane, LaneNeedsYou)
	}
}

func TestFleetUpdateUnknownSession(t *testing.T) {
	fleet := NewFleet()

	_, err := fleet.Update("ghost", func(*Facts) {})
	if !errors.Is(err, ErrUnknownSession) {
		t.Fatalf("Update() error = %v, want ErrUnknownSession", err)
	}
}

func TestFleetByLaneGroupsSessions(t *testing.T) {
	fleet := NewFleet()
	sessions := []Session{
		{ID: "metering", Task: "t", Agent: "claude-code", Branch: "orch/metering-api"},
		{ID: "chart", Task: "t", Agent: "codex", Branch: "orch/usage-chart"},
		{ID: "invoice", Task: "t", Agent: "cursor", Branch: "orch/invoice-email"},
	}
	for _, s := range sessions {
		if err := fleet.Start(s); err != nil {
			t.Fatalf("Start(%s) error = %v, want nil", s.ID, err)
		}
	}

	if _, err := fleet.Update("invoice", func(f *Facts) {
		f.Activity.AwaitingHuman = true
		f.Activity.Question = "which mail provider?"
	}); err != nil {
		t.Fatalf("Update() error = %v, want nil", err)
	}
	if _, err := fleet.Update("chart", func(f *Facts) {
		f.PR = pr(true, []Check{{Name: "ci", Status: CheckPassed}}, []Review{{Reviewer: "r", State: ReviewApproved}}, false)
	}); err != nil {
		t.Fatalf("Update() error = %v, want nil", err)
	}

	groups := fleet.ByLane()
	if got := len(groups[LaneWorking]); got != 1 {
		t.Fatalf("working lane has %d sessions, want 1 (metering)", got)
	}
	if got := len(groups[LaneInReview]); got != 0 {
		t.Fatalf("in-review lane has %d sessions, want 0", got)
	}
	if len(groups[LaneReadyToMerge]) != 1 || len(groups[LaneNeedsYou]) != 1 {
		t.Fatalf("groups = %+v, want one ready-to-merge (chart) and one needs-you (invoice)", groups)
	}
	if groups[LaneReadyToMerge][0] != "chart" || groups[LaneNeedsYou][0] != "invoice" {
		t.Fatalf("groups = %+v, want chart ready to merge and invoice needing the human", groups)
	}
}
