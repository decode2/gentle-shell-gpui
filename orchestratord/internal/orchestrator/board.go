package orchestrator

import (
	"fmt"
	"sort"
	"strings"
)

// SessionCard is the board's projection of one session: what a card shows,
// nothing else.
type SessionCard struct {
	ID       string `json:"id"`
	Task     string `json:"task"`
	Agent    string `json:"agent"`
	Branch   string `json:"branch"`
	Project  string `json:"project,omitempty"`
	PR       int    `json:"pr,omitempty"`
	Running  bool   `json:"running,omitempty"`
	Status   string `json:"status,omitempty"`
	Question string `json:"question,omitempty"`
	// GatePending marks a launched session whose evidence receipt has not
	// frozen yet (F4): the card explains why ready-to-merge waits.
	GatePending bool `json:"gatePending,omitempty"`
}

// Board groups every session's card by its fact-derived lane. All four lanes
// are present so a renderer can draw the board without nil checks.
func (f *Fleet) Board() map[Lane][]SessionCard {
	return f.BoardFor("")
}

// AllCards returns every session's card ungrouped, for persistence sweeps.
func (f *Fleet) AllCards() []SessionCard {
	f.mu.RLock()
	defer f.mu.RUnlock()

	cards := make([]SessionCard, 0, len(f.sessions))
	for id, s := range f.sessions {
		card := SessionCard{ID: string(id), Task: s.Task, Agent: s.Agent, Branch: s.Branch, Project: s.Project}
		if p := s.Facts.PR; p != nil {
			card.PR = p.Number
			card.GatePending = s.Facts.ReceiptRequired && !s.Facts.ReceiptValid
		}
		card.Running = s.Facts.Activity.Running
		card.Status = cardStatus(&s.Facts)
		card.Question = s.Facts.Activity.Question
		cards = append(cards, card)
	}
	return cards
}

// BoardFor groups the board restricted to one project; an empty project
// means every session.
func (f *Fleet) BoardFor(project string) map[Lane][]SessionCard {
	groups := make(map[Lane][]SessionCard, 4)
	for _, lane := range []Lane{LaneWorking, LaneNeedsYou, LaneInReview, LaneReadyToMerge} {
		groups[lane] = nil
	}

	f.mu.RLock()
	defer f.mu.RUnlock()
	for id, s := range f.sessions {
		if project != "" && s.Project != project {
			continue
		}
		card := SessionCard{
			ID:          string(id),
			Task:        s.Task,
			Agent:       s.Agent,
			Branch:      s.Branch,
			Project:     s.Project,
			Running:     s.Facts.Activity.Running,
			Status:      cardStatus(&s.Facts),
			Question:    s.Facts.Activity.Question,
			GatePending: s.Facts.ReceiptRequired && !s.Facts.ReceiptValid,
		}
		if p := s.Facts.PR; p != nil {
			card.PR = p.Number
		}
		lane := DeriveLane(s.Facts)
		groups[lane] = append(groups[lane], card)
	}

	for lane := range groups {
		cards := groups[lane]
		sort.Slice(cards, func(i, j int) bool { return cards[i].ID < cards[j].ID })
	}
	return groups
}

// cardStatus derives the one-line live status a card shows, from facts only.
func cardStatus(f *Facts) string {
	if f.Activity.AwaitingHuman {
		return "Esperándote"
	}
	if f.Activity.Detail != "" {
		return f.Activity.Detail
	}
	if p := f.PR; p != nil && p.Open {
		if p.MergeConflict {
			return "Conflicto con la base"
		}
		for _, c := range p.Checks {
			if c.Status == CheckFailed {
				return "CI roto: " + c.Name
			}
		}
		runningChecks := false
		for _, c := range p.Checks {
			if c.Status == CheckRunning {
				runningChecks = true
			}
		}
		for _, r := range p.Reviews {
			if r.State == ReviewApproved {
				if runningChecks {
					return "Checks corriendo"
				}
				return "Aprobado"
			}
		}
		return "En revisión"
	}
	if f.Activity.Running {
		return "Trabajando"
	}
	return "En espera"
}

// RenderBoard draws the lanes as a plain-text board. It is the headless
// fallback view; the TUI and desktop apps render from Board() directly.
func RenderBoard(board map[Lane][]SessionCard) string {
	var b strings.Builder
	for _, lane := range []Lane{LaneWorking, LaneNeedsYou, LaneInReview, LaneReadyToMerge} {
		fmt.Fprintf(&b, "== %s ==\n", lane)
		cards := board[lane]
		if len(cards) == 0 {
			b.WriteString("  (empty)\n")
			continue
		}
		for _, card := range cards {
			fmt.Fprintf(&b, "  %-16s %-12s %-24s", card.ID, card.Agent, card.Branch)
			if card.PR != 0 {
				fmt.Fprintf(&b, " #%d", card.PR)
			}
			b.WriteString("\n")
			if card.Task != "" {
				fmt.Fprintf(&b, "  %-32s %s\n", "", card.Task)
			}
		}
	}
	return b.String()
}
