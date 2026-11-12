package orchestrator

import (
	"strings"
	"testing"
)

func TestFleetBoardGroupsCardsByLane(t *testing.T) {
	fleet := NewFleet()
	sessions := []Session{
		{ID: "metering", Task: "Meter API calls per workspace", Agent: "claude-code", Branch: "orch/metering-api"},
		{ID: "chart", Task: "Chart usage on the billing page", Agent: "codex", Branch: "orch/usage-chart"},
		{ID: "invoice", Task: "Email a monthly invoice", Agent: "cursor", Branch: "orch/invoice-email"},
	}
	for _, s := range sessions {
		if err := fleet.Start(s); err != nil {
			t.Fatalf("Start(%s): %v", s.ID, err)
		}
	}
	if _, err := fleet.Update("invoice", func(f *Facts) {
		f.Activity.AwaitingHuman = true
		f.Activity.Question = "¿Postmark o SES para el email de factura?"
	}); err != nil {
		t.Fatalf("Update(invoice): %v", err)
	}
	if _, err := fleet.Update("chart", func(f *Facts) {
		f.PR = &PullRequest{Number: 331, Open: true,
			Checks:  []Check{{Name: "ci", Status: CheckPassed}},
			Reviews: []Review{{Reviewer: "r", State: ReviewApproved}}}
	}); err != nil {
		t.Fatalf("Update(chart): %v", err)
	}

	board := fleet.Board()

	cards := board[LaneReadyToMerge]
	if len(cards) != 1 || cards[0].ID != "chart" {
		t.Fatalf("ready-to-merge cards = %+v, want only chart", cards)
	}
	if cards[0].PR != 331 || cards[0].Agent != "codex" {
		t.Fatalf("card = %+v, want PR 331 by codex", cards[0])
	}
	if cards[0].Status != "Aprobado" {
		t.Fatalf("chart status = %q, want Aprobado", cards[0].Status)
	}
	if len(board[LaneNeedsYou]) != 1 || board[LaneNeedsYou][0].ID != "invoice" {
		t.Fatalf("needs-you cards = %+v, want only invoice", board[LaneNeedsYou])
	}
	invoice := board[LaneNeedsYou][0]
	if invoice.Status != "Esperándote" || invoice.Question == "" {
		t.Fatalf("invoice card = %+v, want Esperándote with the question", invoice)
	}
	if len(board[LaneWorking]) != 1 || board[LaneWorking][0].ID != "metering" {
		t.Fatalf("working cards = %+v, want only metering", board[LaneWorking])
	}
	if len(board[LaneInReview]) != 0 {
		t.Fatalf("in-review cards = %+v, want none", board[LaneInReview])
	}
}

func TestRenderBoardShowsLanesAndCards(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "metering", Task: "Meter API calls", Agent: "claude-code", Branch: "orch/metering-api"}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	if _, err := fleet.Update("metering", func(f *Facts) {
		f.PR = &PullRequest{Number: 322, Open: true, Checks: []Check{{Name: "ci", Status: CheckFailed}}, MergeConflict: true}
	}); err != nil {
		t.Fatalf("Update(): %v", err)
	}

	out := RenderBoard(fleet.Board())
	for _, want := range []string{"working", "needs-you", "in-review", "ready-to-merge", "metering", "322", "claude-code"} {
		if !strings.Contains(out, want) {
			t.Fatalf("RenderBoard() missing %q:\n%s", want, out)
		}
	}
}
