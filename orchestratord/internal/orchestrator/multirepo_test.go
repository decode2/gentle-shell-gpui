package orchestrator

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// ---------- receipt gate ----------

func TestReceiptGateHoldsApprovedWorkWithoutReceipt(t *testing.T) {
	pr := pr(true, []Check{{Name: "ci", Status: CheckPassed}}, []Review{{Reviewer: "r", State: ReviewApproved}}, false)

	// A launched session without its frozen receipt stays in review.
	locked := Facts{PR: pr, ReceiptRequired: true}
	if got := DeriveLane(locked); got != LaneInReview {
		t.Fatalf("gate: lane = %q, want in-review until the receipt freezes", got)
	}

	// Once the receipt freezes, it graduates to ready-to-merge.
	released := Facts{PR: pr, ReceiptRequired: true, ReceiptValid: true}
	if got := DeriveLane(released); got != LaneReadyToMerge {
		t.Fatalf("gate release: lane = %q, want ready-to-merge", got)
	}

	// Externally managed sessions never carry the gate.
	external := Facts{PR: pr}
	if got := DeriveLane(external); got != LaneReadyToMerge {
		t.Fatalf("external session: lane = %q, want ready-to-merge without a gate", got)
	}
}

func TestLauncherFreezeReleasesTheGate(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns processes")
	}
	fleet, _, _, launcher := launcherFixture(t)
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "echo done")
	}

	session := Session{ID: "metering", Task: "t", Agent: "pi", Branch: "orch/metering"}
	if err := launcher.StartTask(context.Background(), session); err != nil {
		t.Fatalf("StartTask(): %v", err)
	}
	if _, err := fleet.Update("metering", func(f *Facts) {
		f.PR = pr(true, []Check{{Name: "ci", Status: CheckPassed}}, []Review{{Reviewer: "r", State: ReviewApproved}}, false)
	}); err != nil {
		t.Fatalf("approved PR: %v", err)
	}
	if lane, _ := fleet.Lane("metering"); lane != LaneInReview {
		t.Fatalf("lane before receipt = %q, want the gate holding it", lane)
	}

	if err := launcher.Stop("metering"); err != nil {
		// Worker may have exited already; the freeze still must have run.
		_ = err
	}
	deadline := time.Now().Add(3 * time.Second)
	for {
		if lane, _ := fleet.Lane("metering"); lane == LaneReadyToMerge {
			return
		}
		if time.Now().After(deadline) {
			lane, _ := fleet.Lane("metering")
			t.Fatalf("gate never released after freeze; lane = %q", lane)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// ---------- multi-repo ----------

func TestProjectsRegistryValidatesAndManagersPerProject(t *testing.T) {
	repo := initRepo(t)
	projects := NewProjects()

	err := projects.Add(Project{ID: "", Repo: "o/r"})
	if err == nil || !strings.Contains(err.Error(), "id") {
		t.Fatalf("empty id error = %v", err)
	}
	err = projects.Add(Project{ID: "p", Repo: "noshape"})
	if err == nil || !strings.Contains(err.Error(), "OWNER/REPOSITORY") {
		t.Fatalf("bad repo error = %v", err)
	}
	err = projects.Add(Project{ID: "p", Repo: "o/r", RepoPath: "/not/a/tree"})
	if err == nil || !strings.Contains(err.Error(), "git") {
		t.Fatalf("bad repoPath error = %v", err)
	}

	if err := projects.Add(Project{ID: "core", Repo: "o/core", RepoPath: repo}); err != nil {
		t.Fatalf("Add(): %v", err)
	}
	if err := projects.Add(Project{ID: "core", Repo: "o/other"}); !errors.Is(err, ErrDuplicateSession) && err == nil {
		t.Fatalf("duplicate Add error = %v, want an error", err)
	}

	if got := len(projects.List()); got != 1 {
		t.Fatalf("List() = %d, want 1", got)
	}
	mgr, ok := projects.ManagerFor("core")
	if !ok || mgr == nil || mgr.Repo != repo {
		t.Fatalf("ManagerFor(core) = %+v, %v", mgr, ok)
	}
	if _, ok := projects.ManagerFor("ghost"); ok {
		t.Fatal("ManagerFor(ghost) succeeded")
	}
}

func TestBoardCarriesProjectAndFilters(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "m1", Task: "t", Agent: "pi", Branch: "orch/a", Project: "core"}); err != nil {
		t.Fatalf("Start: %v", err)
	}
	if err := fleet.Start(Session{ID: "m2", Task: "t", Agent: "codex", Branch: "orch/b", Project: "web"}); err != nil {
		t.Fatalf("Start: %v", err)
	}

	all := fleet.Board()
	if len(all[LaneWorking]) != 2 {
		t.Fatalf("unfiltered working = %d, want both", len(all[LaneWorking]))
	}
	core := fleet.BoardFor("core")
	if len(core[LaneWorking]) != 1 || core[LaneWorking][0].ID != "m1" || core[LaneWorking][0].Project != "core" {
		t.Fatalf("core board = %+v, want only m1 with its project", core[LaneWorking])
	}
}

func TestProjectsEndpointsRoundTrip(t *testing.T) {
	store := &configStore{path: t.TempDir() + "/orchestrator.json"}
	srv := httptest.NewServer(newDaemonServerProjects(NewFleet(), NewJournal(), nil, nil, nil, store, NewProjects()))
	t.Cleanup(srv.Close)

	resp, err := http.Post(srv.URL+"/projects", "application/json",
		strings.NewReader(`{"id":"core","repo":"owner/core","repoPath":"`+initRepo(t)+`"}`))
	if err != nil {
		t.Fatalf("POST /projects: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want 201", resp.StatusCode)
	}

	res, err := http.Get(srv.URL + "/projects")
	if err != nil {
		t.Fatalf("GET /projects: %v", err)
	}
	defer res.Body.Close()
	var list []Project
	if err := json.NewDecoder(res.Body).Decode(&list); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if len(list) != 1 || list[0].ID != "core" {
		t.Fatalf("projects = %+v, want core", list)
	}
}
