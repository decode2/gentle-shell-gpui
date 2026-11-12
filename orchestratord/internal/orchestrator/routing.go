package orchestrator

import (
	"errors"
	"sync"
)

// EventKind classifies one thing that happened to a pull request or session.
type EventKind string

const (
	EventCheckFailed   EventKind = "check-failed"
	EventCheckPassed   EventKind = "check-passed"
	EventReviewComment EventKind = "review-comment"
	EventApproved      EventKind = "approved"
	EventMergeConflict EventKind = "merge-conflict"
	EventFixPushed     EventKind = "fix-pushed"
	EventQuestion      EventKind = "question"
)

// fixRequested reports whether the event asks the owning worker for a fix.
// These events consume the worker's fix budget.
func (k EventKind) fixRequested() bool {
	switch k {
	case EventCheckFailed, EventReviewComment, EventMergeConflict:
		return true
	default:
		return false
	}
}

// fixSucceeded reports whether the event proves a fix landed or the PR
// recovered. These events reset the worker's fix budget.
func (k EventKind) fixSucceeded() bool {
	switch k {
	case EventCheckPassed, EventFixPushed, EventApproved:
		return true
	default:
		return false
	}
}

// Event is one piece of feedback routed to the session that owns the branch.
type Event struct {
	Kind    EventKind
	Session SessionID
	PR      int
	Check   Check
	Review  Review
	Note    string
}

var (
	// ErrFixBudgetExhausted is returned when a worker burned its fix cycles
	// without recovering; the router stops auto-routing and the board must
	// escalate to the human instead of pushing more agent attempts.
	ErrFixBudgetExhausted = errors.New("orchestrator: fix budget exhausted, escalate to human")
)

// Router delivers feedback events to the inbox of the session that owns the
// branch, with a bounded fix budget per session: after maxFixCycles
// consecutive fix requests without recovery the router refuses further ones.
// All methods are safe for concurrent use.
type Router struct {
	mu           sync.Mutex
	maxFixCycles int
	failures     map[SessionID]int
	inboxes      map[SessionID]chan Event
	dropped      map[SessionID]int
}

// inboxCapacity bounds how many undelivered events a session may queue.
const inboxCapacity = 8

// NewRouter returns a router that allows each session maxFixCycles
// consecutive fix requests before escalating.
func NewRouter(maxFixCycles int) *Router {
	return &Router{
		maxFixCycles: maxFixCycles,
		failures:     make(map[SessionID]int),
		inboxes:      make(map[SessionID]chan Event),
		dropped:      make(map[SessionID]int),
	}
}

// Register creates the session's inbox and returns the receive side.
func (r *Router) Register(id SessionID) <-chan Event {
	r.mu.Lock()
	defer r.mu.Unlock()

	ch, ok := r.inboxes[id]
	if !ok {
		ch = make(chan Event, inboxCapacity)
		r.inboxes[id] = ch
	}
	return ch
}

// Route delivers ev to the owning session's inbox.
//
//   - A fix request consumes budget; once exhausted it returns
//     ErrFixBudgetExhausted without delivering.
//   - A fix success or approval resets the budget.
//   - A question is delivered and never consumes budget.
func (r *Router) Route(ev Event) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	ch, ok := r.inboxes[ev.Session]
	if !ok {
		return ErrUnknownSession
	}

	switch {
	case ev.Kind.fixRequested():
		if r.failures[ev.Session] >= r.maxFixCycles {
			return ErrFixBudgetExhausted
		}
		r.failures[ev.Session]++
	case ev.Kind.fixSucceeded():
		delete(r.failures, ev.Session)
	}

	select {
	case ch <- ev:
		return nil
	default:
		r.dropped[ev.Session]++
		return errors.New("orchestrator: session inbox full")
	}
}

// Dropped reports how many events were refused for a session because its
// inbox was full; a non-zero count is a visibility signal, not an error.
func (r *Router) Dropped(id SessionID) int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.dropped[id]
}
