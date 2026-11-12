package orchestrator

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
)

// ghPull mirrors the JSON fields of `gh pr list` used by the watcher.
type ghPull struct {
	Number            int        `json:"number"`
	HeadRefName       string     `json:"headRefName"`
	State             string     `json:"state"`
	Mergeable         string     `json:"mergeable"`
	StatusCheckRollup []ghCheck  `json:"statusCheckRollup"`
	Reviews           []ghReview `json:"reviews"`
}

// ghCheck mirrors one statusCheckRollup entry: CheckRun carries status and
// conclusion; other entry types are skipped.
type ghCheck struct {
	TypeName   string `json:"__typename"`
	Name       string `json:"name"`
	Status     string `json:"status"`
	Conclusion string `json:"conclusion"`
	DetailsURL string `json:"detailsUrl"`
}

type ghReview struct {
	Author ghUser `json:"author"`
	State  string `json:"state"`
}

type ghUser struct {
	Login string `json:"login"`
}

type ghPullList struct {
	Data []ghPull `json:"data"`
}

// mapSnapshot projects one gh pull onto a provider-neutral PRSnapshot.
func mapSnapshot(p ghPull) PRSnapshot {
	snap := PRSnapshot{
		Number: p.Number,
		Branch: p.HeadRefName,
		Open:   p.State == "OPEN",
	}
	if p.Mergeable == "CONFLICTING" {
		snap.MergeConflict = true
	}
	for _, c := range p.StatusCheckRollup {
		status := mapCheck(c)
		if status == "" {
			continue
		}
		snap.Checks = append(snap.Checks, Check{Name: c.Name, Status: status, LogURL: c.DetailsURL})
	}
	for _, r := range p.Reviews {
		snap.Reviews = append(snap.Reviews, Review{Reviewer: r.Author.Login, State: mapReview(r.State)})
	}
	return snap
}

func mapCheck(c ghCheck) CheckStatus {
	if c.TypeName != "CheckRun" {
		return ""
	}
	switch c.Status {
	case "IN_PROGRESS":
		return CheckRunning
	case "COMPLETED":
		if c.Conclusion == "FAILURE" {
			return CheckFailed
		}
		return CheckPassed
	default:
		return CheckRunning
	}
}

func mapReview(state string) ReviewState {
	switch state {
	case "APPROVED":
		return ReviewApproved
	case "CHANGES_REQUESTED":
		return ReviewChangesRequested
	default:
		return ReviewPending
	}
}

// GhPullSource implements PullSource against the gh CLI.
type GhPullSource struct {
	// Repo is the OWNER/REPOSITORY handle passed to gh -R.
	Repo string
}

const ghPullFields = "number,headRefName,state,mergeable,statusCheckRollup,reviews"

// Fetch lists the repository's pull requests and maps every one it can. gh
// emits a bare JSON array; the {"data": [...]} envelope is accepted too.
func (s GhPullSource) Fetch(ctx context.Context) ([]PRSnapshot, error) {
	gh := exec.CommandContext(ctx, "gh", "pr", "list", "-R", s.Repo, "--json", ghPullFields)
	// gh is an exe (no window), but shimmed or wrapped installs would flash
	// one on Windows — the watcher is always silent.
	applyProcessPlatformTraits(gh)
	out, err := gh.Output()
	if err != nil {
		return nil, fmt.Errorf("orchestrator: gh pr list: %w", err)
	}

	pulls, err := parsePullList(out)
	if err != nil {
		return nil, fmt.Errorf("orchestrator: parse gh pr list: %w", err)
	}
	snapshots := make([]PRSnapshot, 0, len(pulls))
	for _, p := range pulls {
		snapshots = append(snapshots, mapSnapshot(p))
	}
	return snapshots, nil
}

func parsePullList(raw []byte) ([]ghPull, error) {
	var list ghPullList
	if err := json.Unmarshal(raw, &list); err == nil {
		return list.Data, nil
	}
	var pulls []ghPull
	if err := json.Unmarshal(raw, &pulls); err != nil {
		return nil, err
	}
	return pulls, nil
}
