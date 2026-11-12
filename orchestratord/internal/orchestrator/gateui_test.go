package orchestrator

import (
	"testing"
)

func TestSessionCardCarriesGatePending(t *testing.T) {
	approvedPR := pr(true, []Check{{Name: "ci", Status: CheckPassed}}, []Review{{Reviewer: "r", State: ReviewApproved}}, false)

	fleet := NewFleet()
	sessions := []Session{
		{ID: "gated", Task: "t", Agent: "pi", Branch: "orch/gated", Project: "core"},
		{ID: "external", Task: "t", Agent: "codex", Branch: "orch/external"},
	}
	for _, s := range sessions {
		if err := fleet.Start(s); err != nil {
			t.Fatalf("Start(%s): %v", s.ID, err)
		}
	}
	if _, err := fleet.Update("gated", func(f *Facts) {
		f.PR = approvedPR
		f.ReceiptRequired = true
	}); err != nil {
		t.Fatalf("Update(gated): %v", err)
	}
	if _, err := fleet.Update("external", func(f *Facts) {
		f.PR = approvedPR
	}); err != nil {
		t.Fatalf("Update(external): %v", err)
	}

	board := fleet.Board()
	// The gated session is still in review with the gate pending.
	if got := len(board[LaneInReview]); got != 1 || board[LaneInReview][0].ID != "gated" {
		t.Fatalf("in-review = %+v, want only the gated session", board[LaneInReview])
	}
	if !board[LaneInReview][0].GatePending {
		t.Fatalf("gated card = %+v, want GatePending", board[LaneInReview][0])
	}
	// The external session graduates normally.
	if got := len(board[LaneReadyToMerge]); got != 1 || board[LaneReadyToMerge][0].ID != "external" {
		t.Fatalf("ready-to-merge = %+v, want only the external session", board[LaneReadyToMerge])
	}
	if board[LaneReadyToMerge][0].GatePending {
		t.Fatalf("external card = %+v, want no gate", board[LaneReadyToMerge][0])
	}
}
