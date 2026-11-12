package orchestrator

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os/exec"
	"strings"
	"testing"
)

func TestConfigRoundTripAndValidation(t *testing.T) {
	repo := initRepo(t)
	store := &configStore{path: t.TempDir() + "/orchestrator.json"}
	srv := httptest.NewServer(newDaemonServer(NewFleet(), NewJournal(), nil, nil, nil, store))
	t.Cleanup(srv.Close)

	// Invalid repo shape is refused.
	resp, err := http.Post(srv.URL+"/config", "application/json", strings.NewReader(`{"repo":"nope","repoPath":""}`))
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("bad repo status = %d, want 400", resp.StatusCode)
	}

	// Invalid repoPath (not a git tree) is refused.
	resp, err = http.Post(srv.URL+"/config", "application/json",
		strings.NewReader(`{"repo":"o/r","repoPath":"/definitely/not/a/repo"}`))
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("bad repoPath status = %d, want 400", resp.StatusCode)
	}

	// Valid config saves and reads back.
	resp, err = http.Post(srv.URL+"/config", "application/json",
		strings.NewReader(`{"repo":"owner/repo","repoPath":"`+repo+`"}`))
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("valid config status = %d, want 200", resp.StatusCode)
	}

	res, err := http.Get(srv.URL + "/config")
	if err != nil {
		t.Fatalf("GET /config: %v", err)
	}
	defer res.Body.Close()
	var cfg configRequest
	if err := json.NewDecoder(res.Body).Decode(&cfg); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if cfg.Repo != "owner/repo" || cfg.RepoPath != repo {
		t.Fatalf("config = %+v, want the saved values", cfg)
	}
}

func TestEvidenceReceiptIsStableAndSensitive(t *testing.T) {
	ev := Evidence{SessionID: "m", Agent: "pi", Branch: "orch/m", Status: "Trabajando", Added: 3}
	first := evidenceReceipt(ev)
	if first == "" || !strings.HasPrefix(first, "rcpt1_") {
		t.Fatalf("receipt = %q, want an rcpt1_ hash", first)
	}
	if again := evidenceReceipt(ev); again != first {
		t.Fatalf("receipt changed for identical facts: %q vs %q", first, again)
	}
	ev.Status = "Aprobado"
	if changed := evidenceReceipt(ev); changed == first {
		t.Fatal("receipt ignored a fact change")
	}
}

func TestWorkerCommitsListAgainstBase(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git")
	}
	repo := initRepo(t)
	mgr := &WorktreeManager{Repo: repo, Parent: t.TempDir()}
	wt, err := mgr.Create("orch/committed", "main")
	if err != nil {
		t.Fatalf("Create(): %v", err)
	}
	if out, err := exec.Command("sh", "-c", "printf 'x\\n' > "+wt+"/f.txt && git -C "+wt+" add f.txt && git -C "+wt+" -c user.email=w@l -c user.name=w commit -qm 'feat: f.txt'").CombinedOutput(); err != nil {
		t.Fatalf("commit: %v %s", err, out)
	}

	commits := workerCommits(wt, "main", 10)
	if len(commits) != 1 || !strings.Contains(commits[0], "feat: f.txt") {
		t.Fatalf("commits = %+v, want the one feature commit", commits)
	}
}
