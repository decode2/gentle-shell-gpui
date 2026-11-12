package orchestrator

import (
	"net/http"
	"strings"
	"time"
)

// maxEvidenceJournalEntries bounds the journal served in API responses.
const maxEvidenceJournalEntries = 500

// Evidence is the per-worker audit package (PRD F4): what the session did,
// in facts. The UI shows it; RDD receipts join it in a later sprint.
type Evidence struct {
	SessionID string         `json:"sessionId"`
	Task      string         `json:"task"`
	Agent     string         `json:"agent"`
	Branch    string         `json:"branch"`
	Worktree  string         `json:"worktree,omitempty"`
	Status    string         `json:"status"`
	Summary   string         `json:"summary,omitempty"`
	Decisions []string       `json:"decisions,omitempty"`
	Receipt   string         `json:"receipt,omitempty"`
	Valid     bool           `json:"valid"`
	Commits   []string       `json:"commits,omitempty"`
	StartedAt *time.Time     `json:"startedAt,omitempty"`
	UpdatedAt *time.Time     `json:"updatedAt,omitempty"`
	Files     []DiffFile     `json:"files,omitempty"`
	Added     int            `json:"totalAdditions"`
	Deleted   int            `json:"totalDeletions"`
	Journal   []JournalEntry `json:"journal"`
}

// sessionEvidence assembles the worker's evidence package: identity, live
// status, worktree diff summary, and the full journal.
func (d *DaemonHTTP) sessionEvidence(w http.ResponseWriter, r *http.Request) {
	s, err := d.fleet.Get(SessionID(r.PathValue("id")))
	if err != nil {
		http.Error(w, "unknown session", http.StatusNotFound)
		return
	}

	ev := Evidence{
		SessionID: string(s.ID),
		Task:      s.Task,
		Agent:     s.Agent,
		Branch:    s.Branch,
		Worktree:  s.Worktree,
		Status:    cardStatus(&s.Facts),
		Journal:   []JournalEntry{},
	}
	if d.journal != nil {
		ev.Journal, _ = d.journal.ForEvidence(s.ID, maxEvidenceJournalEntries)
		if len(ev.Journal) > 0 {
			first, last := ev.Journal[0].At, ev.Journal[len(ev.Journal)-1].At
			ev.StartedAt = &first
			ev.UpdatedAt = &last
		}
		for _, entry := range ev.Journal {
			switch entry.Kind {
			case "summary":
				ev.Summary = entry.Text
			case "decision":
				ev.Decisions = append(ev.Decisions, entry.Text)
			}
		}
	}
	ev.Commits = workerCommits(s.Worktree, s.Base, 50)
	if s.Facts.UpdatedAt != (time.Time{}) {
		ev.UpdatedAt = &s.Facts.UpdatedAt
	}

	if info, diffErr := worktreeDiff(s.Worktree, s.Base); diffErr == nil {
		ev.Files = info.Files
		for _, f := range info.Files {
			ev.Added += f.Additions
			ev.Deleted += f.Deletions
		}
	}
	// The frozen receipt lives in the journal (kind "receipt"); when present
	// it is authoritative and the package reports whether its current facts
	// still match. Without a frozen receipt the package computes one fresh.
	for _, entry := range ev.Journal {
		if entry.Kind == "receipt" && strings.HasPrefix(entry.Text, "rcpt1_") {
			ev.Receipt = entry.Text
			break
		}
	}
	if ev.Receipt == "" {
		ev.Receipt = evidenceReceipt(ev)
		ev.Valid = true
	} else {
		ev.Valid = evidenceReceiptValid(ev)
	}
	writeJSON(w, http.StatusOK, ev)
}
