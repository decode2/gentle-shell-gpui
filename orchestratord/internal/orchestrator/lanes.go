package orchestrator

// DeriveLane projects live facts onto a kanban lane.
//
// Precedence, highest first:
//  1. The agent is waiting on the human (question, approval).
//  2. The PR is blocked: a check failed, the review asked for changes, or
//     the branch conflicts with its base. Any of these needs the human or a
//     routed fix cycle, so it lands in needs-you.
//  3. The PR is approved and every check passed: ready-to-merge.
//  4. The PR is open and unblocked: in-review (checks running, review
//     pending, or no verdicts yet).
//  5. Otherwise (no PR, closed PR, agent idle): working.
func DeriveLane(f Facts) Lane {
	if f.Activity.AwaitingHuman {
		return LaneNeedsYou
	}

	if p := f.PR; p != nil && p.Open {
		if p.MergeConflict {
			return LaneNeedsYou
		}
		for _, c := range p.Checks {
			if c.Status == CheckFailed {
				return LaneNeedsYou
			}
		}
		approved := false
		pendingReview := false
		for _, r := range p.Reviews {
			switch r.State {
			case ReviewApproved:
				approved = true
			case ReviewChangesRequested:
				return LaneNeedsYou
			case ReviewPending:
				pendingReview = true
			}
		}
		checksPassed := checksAllPassed(p)
		switch {
		case approved && checksPassed && !pendingReview:
			// Evidence gate (F4): launched sessions wait for their frozen
			// receipt before graduating; external sessions skip the gate.
			if f.ReceiptRequired && !f.ReceiptValid {
				return LaneInReview
			}
			return LaneReadyToMerge
		default:
			return LaneInReview
		}
	}

	return LaneWorking
}

func checksAllPassed(p *PullRequest) bool {
	if len(p.Checks) == 0 {
		return false
	}
	for _, c := range p.Checks {
		if c.Status != CheckPassed {
			return false
		}
	}
	return true
}
