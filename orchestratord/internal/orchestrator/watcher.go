package orchestrator

import (
	"context"
	"errors"
	"sync"
	"time"
)

// PRSnapshot is one external pull request observation: an immutable projection
// of what a provider reports right now.
type PRSnapshot struct {
	Number        int
	Branch        string
	Open          bool
	Checks        []Check
	Reviews       []Review
	MergeConflict bool
}

func (s PRSnapshot) pullRequest() *PullRequest {
	return &PullRequest{
		Number:        s.Number,
		Open:          s.Open,
		Checks:        s.Checks,
		Reviews:       s.Reviews,
		MergeConflict: s.MergeConflict,
	}
}

// PullSource yields the current pull request snapshots for a repository.
// Implementations wrap a provider (GitHub, GitLab) or a test fake.
type PullSource interface {
	Fetch(ctx context.Context) ([]PRSnapshot, error)
}

// Watcher turns provider snapshots into fleet facts and routed feedback: it
// mirrors each snapshot into the owning session's Facts and delivers fix
// requests to the session that owns the branch. Failed checks re-route on
// every poll until they pass; the Router's fix budget bounds the loop.
type Watcher struct {
	fleet    *Fleet
	router   *Router
	source   PullSource
	interval time.Duration

	mu   sync.Mutex
	last map[int]PRSnapshot // last seen snapshot per PR number
}

// NewWatcher returns a watcher polling source every interval. An interval of
// zero or less falls back to 30 seconds.
func NewWatcher(fleet *Fleet, router *Router, source PullSource, interval time.Duration) *Watcher {
	if interval <= 0 {
		interval = 30 * time.Second
	}
	return &Watcher{
		fleet:    fleet,
		router:   router,
		source:   source,
		interval: interval,
		last:     make(map[int]PRSnapshot),
	}
}

// Poll performs one tick. Errors from routing fix requests are joined into
// the returned error so budget exhaustion surfaces to the caller, while
// unknown-session routing failures are ignored (the snapshot belongs to a PR
// no worker owns).
func (w *Watcher) Poll(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err // a dying daemon routes nothing
	}
	snapshots, err := w.source.Fetch(ctx)
	if err != nil {
		return err
	}

	seen := make(map[int]struct{}, len(snapshots))
	var problems []error
	for _, snap := range snapshots {
		if snap.Number == 0 || snap.Branch == "" {
			continue
		}
		seen[snap.Number] = struct{}{}

		id, ok := w.fleet.SessionByBranch(snap.Branch)
		if !ok {
			continue
		}
		problems = append(problems, w.absorb(id, snap)...)
	}

	w.mu.Lock()
	for num := range w.last {
		if _, ok := seen[num]; !ok {
			delete(w.last, num)
		}
	}
	w.mu.Unlock()

	return errors.Join(problems...)
}

// Run polls once immediately, then on the configured interval, until ctx is
// canceled. Per-tick errors do not stop the loop: the next tick retries.
func (w *Watcher) Run(ctx context.Context) error {
	ticker := time.NewTicker(w.interval)
	defer ticker.Stop()

	for {
		_ = w.Poll(ctx)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
		}
	}
}

// absorb records the snapshot for a PR and reports a joined error list: the
// first sight of a PR only adopts facts, later deltas route feedback.
func (w *Watcher) absorb(id SessionID, snap PRSnapshot) []error {
	w.mu.Lock()
	previous, existed := w.last[snap.Number]
	w.last[snap.Number] = snap
	w.mu.Unlock()

	var problems []error
	if existed {
		problems = append(problems, routeDeltas(w.router, id, previous, snap)...)
	}

	if _, err := w.fleet.Update(id, func(f *Facts) { f.PR = snap.pullRequest() }); err != nil {
		problems = append(problems, err)
	}
	return problems
}

// routeDeltas derives the feedback events between two snapshots of the same
// PR. Checks that failed re-route every poll until they pass; the fix budget
// bounds how often a session is asked. Reviews, approvals, and conflicts
// route on their transition.
func routeDeltas(router *Router, id SessionID, previous, next PRSnapshot) []error {
	var problems []error
	route := func(ev Event) {
		ev.Session = id
		if err := router.Route(ev); err != nil && !errors.Is(err, ErrUnknownSession) {
			problems = append(problems, err)
		}
	}

	previousChecks := checkStates(previous)
	for _, check := range next.Checks {
		if check.Status == CheckFailed {
			route(Event{Kind: EventCheckFailed, PR: next.Number, Check: check})
			continue
		}
		if check.Status == CheckPassed && previousChecks[check.Name] != CheckPassed {
			route(Event{Kind: EventCheckPassed, PR: next.Number, Check: check})
		}
	}

	previousReviews := reviewStates(previous)
	for _, review := range next.Reviews {
		if review.State != ReviewChangesRequested {
			continue
		}
		if previousReviews[review.Reviewer] != ReviewChangesRequested {
			route(Event{Kind: EventReviewComment, PR: next.Number, Review: review})
		}
	}

	if approvedAndClear(next) && !approvedAndClear(previous) {
		route(Event{Kind: EventApproved, PR: next.Number})
	}
	if next.MergeConflict && !previous.MergeConflict {
		route(Event{Kind: EventMergeConflict, PR: next.Number})
	}
	return problems
}

func checkStates(s PRSnapshot) map[string]CheckStatus {
	states := make(map[string]CheckStatus, len(s.Checks))
	for _, c := range s.Checks {
		states[c.Name] = c.Status
	}
	return states
}

func reviewStates(s PRSnapshot) map[string]ReviewState {
	states := make(map[string]ReviewState, len(s.Reviews))
	for _, r := range s.Reviews {
		states[r.Reviewer] = r.State
	}
	return states
}

func approvedAndClear(s PRSnapshot) bool {
	approved := false
	for _, r := range s.Reviews {
		switch r.State {
		case ReviewApproved:
			approved = true
		case ReviewPending:
			return false
		}
	}
	return approved
}
