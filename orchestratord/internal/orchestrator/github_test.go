package orchestrator

import (
	"encoding/json"
	"testing"
)

const ghFixture = `{
  "data": [
    {
      "number": 322,
      "headRefName": "orch/metering-api",
      "state": "OPEN",
      "mergeable": "MERGEABLE",
      "statusCheckRollup": [
        {"__typename": "CheckRun", "name": "e2e (chrome)", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://ci/log/1"},
        {"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS"}
      ],
      "reviews": [
        {"author": {"login": "reviewer-agent"}, "state": "APPROVED"}
      ]
    },
    {
      "number": 331,
      "headRefName": "orch/usage-chart",
      "state": "MERGED",
      "mergeable": "CONFLICTING",
      "statusCheckRollup": [],
      "reviews": []
    }
  ]
}`

func TestParseGhPullListMapsSnapshots(t *testing.T) {
	var payload struct {
		Data []ghPull `json:"data"`
	}
	if err := json.Unmarshal([]byte(ghFixture), &payload); err != nil {
		t.Fatalf("unmarshal fixture: %v", err)
	}
	if len(payload.Data) != 2 {
		t.Fatalf("parsed %d pulls, want 2", len(payload.Data))
	}

	first := mapSnapshot(payload.Data[0])
	if first.Number != 322 || first.Branch != "orch/metering-api" || !first.Open {
		t.Fatalf("snapshot = %+v, want open PR 322 on orch/metering-api", first)
	}
	if len(first.Checks) != 2 {
		t.Fatalf("checks = %+v, want 2", first.Checks)
	}
	if first.Checks[0].Name != "e2e (chrome)" || first.Checks[0].Status != CheckFailed || first.Checks[0].LogURL != "https://ci/log/1" {
		t.Fatalf("check[0] = %+v, want failed e2e (chrome) with log URL", first.Checks[0])
	}
	if first.Checks[1].Status != CheckRunning {
		t.Fatalf("check[1] = %+v, want running", first.Checks[1])
	}
	if len(first.Reviews) != 1 || first.Reviews[0].Reviewer != "reviewer-agent" || first.Reviews[0].State != ReviewApproved {
		t.Fatalf("reviews = %+v, want approved reviewer-agent", first.Reviews)
	}

	merged := mapSnapshot(payload.Data[1])
	if merged.Open {
		t.Fatalf("MERGED pull mapped as open: %+v", merged)
	}
}
