package orchestrator

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os/exec"
	"strings"
	"testing"
	"time"
)

func TestWorktreeDiffListsCommittedAndWorkingChanges(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git")
	}
	repo := initRepo(t)
	mgr := &WorktreeManager{Repo: repo, Parent: t.TempDir()}
	wt, err := mgr.Create("orch/metering", "main")
	if err != nil {
		t.Fatalf("Create(): %v", err)
	}

	// One committed file and one unstaged modification on a tracked file.
	if out, err := exec.Command("sh", "-c", "printf 'package main\\n' > "+wt+"/metering.go && git -C "+wt+" add metering.go && git -C "+wt+" -c user.email=t@l -c user.name=t commit -qm m").CombinedOutput(); err != nil {
		t.Fatalf("commit in worktree: %v %s", err, out)
	}
	if out, err := exec.Command("sh", "-c", "printf 'x\\n' >> "+wt+"/metering.go").CombinedOutput(); err != nil {
		t.Fatalf("append: %v %s", err, out)
	}

	info, err := worktreeDiff(wt, "main")
	if err != nil {
		t.Fatalf("worktreeDiff(): %v", err)
	}
	var metering *DiffFile
	for i := range info.Files {
		if info.Files[i].Path == "metering.go" {
			metering = &info.Files[i]
		}
	}
	if metering == nil {
		t.Fatalf("metering.go missing from %+v", info.Files)
	}
	if metering.Additions < 1 {
		t.Fatalf("metering.go = %+v, want at least the committed + unstaged additions", metering)
	}
	if !strings.Contains(info.Patch, "metering.go") {
		t.Fatalf("patch missing the file:\n%s", info.Patch)
	}
}

func TestWorktreeDiffEmptyWhenNoChanges(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git")
	}
	repo := initRepo(t)
	mgr := &WorktreeManager{Repo: repo, Parent: t.TempDir()}
	wt, err := mgr.Create("orch/clean", "main")
	if err != nil {
		t.Fatalf("Create(): %v", err)
	}

	info, err := worktreeDiff(wt, "main")
	if err != nil {
		t.Fatalf("worktreeDiff(): %v", err)
	}
	if len(info.Files) != 0 {
		t.Fatalf("files = %+v, want empty", info.Files)
	}
}

func TestWorktreeDiffRequiresWorktree(t *testing.T) {
	if _, err := worktreeDiff("", "main"); err == nil {
		t.Fatal("worktreeDiff(\"\") succeeded, want an error")
	}
}

func TestSessionDiffEndpoint(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: runs git")
	}
	repo := initRepo(t)
	fleet := NewFleet()
	journal := NewJournal()
	launcher := &Launcher{
		Fleet:     fleet,
		Router:    NewRouter(2),
		Journal:   journal,
		Worktrees: &WorktreeManager{Repo: repo, Parent: t.TempDir()},
		Base:      "main",
	}
	srv := httptest.NewServer(NewDaemonServer(fleet, journal, launcher))
	t.Cleanup(srv.Close)

	body := `{"id":"metering","task":"t","agent":"pi","branch":"orch/metering","launch":true}`
	resp, err := http.Post(srv.URL+"/sessions", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST launch: %v", err)
	}
	resp.Body.Close()

	// A worker that writes a file; after it runs, the diff shows it.
	launcher.mu.Lock()
	launcher.Command = func(ctx context.Context, s Session, dir string) *workProcess {
		return startScript(ctx, dir, "printf 'hello\\n' > feature.txt && git add feature.txt && git -c user.email=w@l -c user.name=w commit -m feat >/dev/null 2>&1 || true; sleep 30")
	}
	launcher.mu.Unlock()
	// Relaunch a second session with the writing command.
	body2 := `{"id":"writer","task":"t","agent":"pi","branch":"orch/writer","launch":true}`
	resp, err = http.Post(srv.URL+"/sessions", "application/json", strings.NewReader(body2))
	if err != nil {
		t.Fatalf("POST launch writer: %v", err)
	}
	resp.Body.Close()

	deadline := time.Now().Add(3 * time.Second)
	for {
		res, err := http.Get(srv.URL + "/sessions/writer/diff")
		if err == nil {
			var info DiffInfo
			err = json.NewDecoder(res.Body).Decode(&info)
			res.Body.Close()
			if err == nil {
				found := false
				for _, f := range info.Files {
					if f.Path == "feature.txt" {
						found = true
					}
				}
				if found {
					return
				}
			}
		}
		if time.Now().After(deadline) {
			t.Fatal("diff never showed feature.txt")
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestSessionDiffUnknownSession(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServer(NewFleet(), NewJournal(), nil))
	t.Cleanup(srv.Close)
	resp, err := http.Get(srv.URL + "/sessions/ghost/diff")
	if err != nil {
		t.Fatalf("GET: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", resp.StatusCode)
	}
}

func TestTerminalEndpointOpensAtWorktree(t *testing.T) {
	if testing.Short() {
		t.Skip("integration: spawns a process")
	}
	fleet := NewFleet()
	journal := NewJournal()
	var openedAt string
	original := terminalCommandForOS
	terminalCommandForOS = func(dir string) (*exec.Cmd, bool) {
		openedAt = dir
		return exec.Command("true"), true
	}
	t.Cleanup(func() { terminalCommandForOS = original })

	if err := fleet.Start(Session{ID: "metering", Task: "t", Agent: "pi", Branch: "orch/m", Worktree: "/tmp/wt-mtr"}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	srv := httptest.NewServer(NewDaemonServer(fleet, journal, nil))
	t.Cleanup(srv.Close)

	resp, err := http.Post(srv.URL+"/sessions/metering/terminal", "application/json", nil)
	if err != nil {
		t.Fatalf("POST terminal: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusAccepted {
		t.Fatalf("status = %d, want 202", resp.StatusCode)
	}
	if openedAt != "/tmp/wt-mtr" {
		t.Fatalf("terminal opened at %q, want the worktree", openedAt)
	}
}

func TestTerminalEndpointUnknownSession(t *testing.T) {
	srv := httptest.NewServer(NewDaemonServer(NewFleet(), NewJournal(), nil))
	t.Cleanup(srv.Close)
	resp, err := http.Post(srv.URL+"/sessions/ghost/terminal", "application/json", nil)
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", resp.StatusCode)
	}
}
