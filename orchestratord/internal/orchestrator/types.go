// Package orchestrator is the Phase 0 spike of the Gentle-AI Orchestrator
// (PRD-ORCHESTRATOR.md). It keeps the fleet registry, derives kanban lanes
// from live facts, routes CI/review feedback to the session that owns the
// branch, and owns worker worktrees. The daemon, watchers, and UI arrive in
// later phases; this package only carries the derivable core.
package orchestrator

import "time"

// SessionID identifies one worker session inside the fleet.
type SessionID string

// Lane is the kanban lane a session sorts itself into. Lanes are always
// derived from Facts; no component may set a lane manually.
type Lane string

const (
	LaneWorking      Lane = "working"
	LaneNeedsYou     Lane = "needs-you"
	LaneInReview     Lane = "in-review"
	LaneReadyToMerge Lane = "ready-to-merge"
)

// CheckStatus is the lifecycle state of one CI check.
type CheckStatus string

const (
	CheckRunning CheckStatus = "running"
	CheckPassed  CheckStatus = "passed"
	CheckFailed  CheckStatus = "failed"
)

// ReviewState is the verdict carried by one review thread.
type ReviewState string

const (
	ReviewPending          ReviewState = "pending"
	ReviewChangesRequested ReviewState = "changes-requested"
	ReviewApproved         ReviewState = "approved"
)

// Check is one CI check attached to a pull request.
type Check struct {
	Name   string
	Status CheckStatus
	LogURL string
}

// Review is one review thread attached to a pull request.
type Review struct {
	Reviewer string
	State    ReviewState
	Thread   string
}

// PullRequest is the PR a worker owns. A nil PR means the worker has not
// opened one yet.
type PullRequest struct {
	Number        int
	Open          bool
	Checks        []Check
	Reviews       []Review
	MergeConflict bool
}

// Activity is what the agent session is doing right now.
type Activity struct {
	Running       bool
	Detail        string // e.g. "editing metering.go", "running tests"
	AwaitingHuman bool
	Question      string
}

// Facts are the live facts a lane is derived from. Facts never carry a lane:
// the lane is a projection, so nothing can drift out of sync with reality.
type Facts struct {
	Activity  Activity
	PR        *PullRequest
	UpdatedAt time.Time
	// Receipt gate (F4): sessions the daemon launched require their frozen
	// evidence receipt before ready-to-merge; externally registered sessions
	// never set ReceiptRequired and skip the gate.
	ReceiptRequired bool
	ReceiptValid    bool
}

// Session is one worker registered in the fleet.
type Session struct {
	ID       SessionID
	Task     string
	Agent    string
	Branch   string
	Project  string
	Worktree string
	Base     string
	Facts    Facts
}
