package orchestrator

import "testing"

func pr(open bool, checks []Check, reviews []Review, conflict bool) *PullRequest {
	return &PullRequest{
		Number:        322,
		Open:          open,
		Checks:        checks,
		Reviews:       reviews,
		MergeConflict: conflict,
	}
}

func TestDeriveLane(t *testing.T) {
	tests := []struct {
		name  string
		facts Facts
		want  Lane
	}{
		{
			name:  "agent running without PR is working",
			facts: Facts{Activity: Activity{Running: true, Detail: "editing metering.go"}},
			want:  LaneWorking,
		},
		{
			name:  "idle agent without PR is still working",
			facts: Facts{},
			want:  LaneWorking,
		},
		{
			name: "awaiting human beats everything else",
			facts: Facts{
				Activity: Activity{Running: true, AwaitingHuman: true, Question: "Postmark or SES for the invoice email?"},
				PR: pr(true,
					[]Check{{Name: "ci", Status: CheckPassed}},
					[]Review{{Reviewer: "reviewer-agent", State: ReviewApproved}},
					false),
			},
			want: LaneNeedsYou,
		},
		{
			name: "failed check needs the human",
			facts: Facts{
				Activity: Activity{Running: true},
				PR:       pr(true, []Check{{Name: "e2e (chrome)", Status: CheckFailed, LogURL: "https://ci/log/1"}}, nil, false),
			},
			want: LaneNeedsYou,
		},
		{
			name: "merge conflict needs the human",
			facts: Facts{
				PR: pr(true, []Check{{Name: "ci", Status: CheckPassed}}, nil, true),
			},
			want: LaneNeedsYou,
		},
		{
			name: "changes requested need the human",
			facts: Facts{
				Activity: Activity{Running: true},
				PR: pr(true,
					[]Check{{Name: "ci", Status: CheckPassed}},
					[]Review{{Reviewer: "nnishant", State: ReviewChangesRequested}},
					false),
			},
			want: LaneNeedsYou,
		},
		{
			name: "approved with all checks passed is ready to merge",
			facts: Facts{
				PR: pr(true,
					[]Check{{Name: "ci", Status: CheckPassed}, {Name: "e2e", Status: CheckPassed}},
					[]Review{{Reviewer: "nnishant", State: ReviewApproved}},
					false),
			},
			want: LaneReadyToMerge,
		},
		{
			name: "approved but checks still running stays in review",
			facts: Facts{
				PR: pr(true,
					[]Check{{Name: "ci", Status: CheckRunning}},
					[]Review{{Reviewer: "nnishant", State: ReviewApproved}},
					false),
			},
			want: LaneInReview,
		},
		{
			name: "approved while another review is pending stays in review",
			facts: Facts{
				PR: pr(true,
					[]Check{{Name: "ci", Status: CheckPassed}},
					[]Review{{Reviewer: "nnishant", State: ReviewApproved}, {Reviewer: "reviewer-agent", State: ReviewPending}},
					false),
			},
			want: LaneInReview,
		},
		{
			name: "open PR without verdicts is in review",
			facts: Facts{
				Activity: Activity{Running: true},
				PR:       pr(true, nil, nil, false),
			},
			want: LaneInReview,
		},
		{
			name: "failed check beats approval",
			facts: Facts{
				PR: pr(true,
					[]Check{{Name: "ci", Status: CheckFailed}},
					[]Review{{Reviewer: "nnishant", State: ReviewApproved}},
					false),
			},
			want: LaneNeedsYou,
		},
		{
			name: "closed PR falls back to working",
			facts: Facts{
				Activity: Activity{Running: true, Detail: "cleanup"},
				PR:       pr(false, nil, nil, false),
			},
			want: LaneWorking,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := DeriveLane(tt.facts)
			if got != tt.want {
				t.Fatalf("DeriveLane() = %q, want %q", got, tt.want)
			}
		})
	}
}
