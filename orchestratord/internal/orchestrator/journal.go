package orchestrator

import (
	"sync"
	"time"
)

// JournalEntry is one recorded fact about a worker session's life: launch,
// output lines, exit. Entries are append-only; the board reads them.
type JournalEntry struct {
	At   time.Time `json:"at"`
	Kind string    `json:"kind"` // launched | output | exited | stopped
	Text string    `json:"text"`
}

// maxJournalEntriesPerSession bounds each session's history: a chatty worker
// cannot grow the daemon's memory without limit. The first entry (the
// launch) is anchored and the newest are kept — the middle ages out.
const maxJournalEntriesPerSession = 2000

// Journal is the per-session event record. It is the daemon-side evidence
// trail (PRD F4): what the worker did, in order, kept even after the board
// lane changes.
type Journal struct {
	mu       sync.RWMutex
	entries  map[SessionID][]JournalEntry
	anchored map[SessionID]bool // first entry pinned per session
}

// NewJournal returns an empty journal.
func NewJournal() *Journal {
	return &Journal{
		entries:  make(map[SessionID][]JournalEntry),
		anchored: make(map[SessionID]bool),
	}
}

// Append records one entry, capping the session's history at
// maxJournalEntriesPerSession: the first entry stays pinned and the newest
// are kept.
func (j *Journal) Append(id SessionID, kind, text string) {
	j.mu.Lock()
	defer j.mu.Unlock()
	entry := JournalEntry{At: time.Now(), Kind: kind, Text: text}
	if !j.anchored[id] {
		j.anchored[id] = true
		j.entries[id] = append(j.entries[id], entry)
		return
	}
	history := j.entries[id]
	if len(history) >= maxJournalEntriesPerSession {
		// Drop the oldest unpinned entry (history[1]) and keep the order.
		tail := make([]JournalEntry, 0, len(history))
		tail = append(tail, history[0])
		tail = append(tail, history[2:]...)
		tail = append(tail, entry)
		j.entries[id] = tail
		return
	}
	j.entries[id] = append(history, entry)
}

// ForEvidence returns the newest n entries with a truncation head when the
// history was cut: what the API responses serve.
func (j *Journal) ForEvidence(id SessionID, n int) ([]JournalEntry, bool) {
	entries := j.Get(id)
	if len(entries) <= n {
		return entries, false
	}
	tail := make([]JournalEntry, 0, n)
	tail = append(tail, entries[0]) // the launch anchor
	tail = append(tail, entries[len(entries)-n+1:]...)
	return tail, true
}

// Get returns a copy of the session's entries; unknown sessions read empty.
func (j *Journal) Get(id SessionID) []JournalEntry {
	j.mu.RLock()
	defer j.mu.RUnlock()
	out := make([]JournalEntry, len(j.entries[id]))
	copy(out, j.entries[id])
	return out
}
