package orchestrator

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// workerCommits lists the session's commits against base, newest first,
// capped: the verifiable units an evidence receipt attests to.
func workerCommits(worktree, base string, limit int) []string {
	if worktree == "" {
		return nil
	}
	if base == "" {
		base = "main"
	}
	out, err := git(worktree, "log", "--oneline", "--no-decorate", base+"...HEAD", "-n", fmt.Sprint(limit))
	if err != nil {
		return nil
	}
	var commits []string
	for _, line := range strings.Split(out, "\n") {
		if line = strings.TrimSpace(line); line != "" {
			commits = append(commits, line)
		}
	}
	return commits
}

// evidenceReceipt hashes the canonical evidence body: same facts → same
// receipt, so an edited package no longer matches its published hash.
func evidenceReceipt(ev Evidence) string {
	canonical, _ := json.Marshal(struct {
		Session, Agent, Branch, Status, Summary string
		Added, Deleted, DecisionCount           int
		Files                                   []string
		Commits                                 []string
	}{
		Session: ev.SessionID, Agent: ev.Agent, Branch: ev.Branch, Status: ev.Status, Summary: ev.Summary,
		Added: ev.Added, Deleted: ev.Deleted, DecisionCount: len(ev.Decisions),
		Files: diffPaths(ev.Files), Commits: ev.Commits,
	})
	sum := sha256.Sum256(canonical)
	return "rcpt1_" + hex.EncodeToString(sum[:8])
}

func diffPaths(files []DiffFile) []string {
	paths := make([]string, 0, len(files))
	for _, f := range files {
		paths = append(paths, f.Path)
	}
	return paths
}

// configRequest is the onboarding body: what the first-run panel collects.
type configRequest struct {
	Repo     string `json:"repo"`
	RepoPath string `json:"repoPath"`
	Listen   string `json:"listen,omitempty"`
}

// configStore abstracts where orchestrator.json lives; the daemon writes next
// to its own executable (the app dir the shell re-reads on restart).
type configStore struct {
	path string
	mu   sync.Mutex
}

func (c *configStore) load() configRequest {
	var cfg configRequest
	raw, err := os.ReadFile(c.path)
	if err != nil {
		return cfg
	}
	_ = json.Unmarshal(raw, &cfg)
	return cfg
}

func (c *configStore) save(cfg configRequest) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	payload, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(c.path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(c.path, append(payload, '\n'), 0o644)
}

// getConfig returns the daemon's current onboarding config.
func (d *DaemonHTTP) getConfig(w http.ResponseWriter, _ *http.Request) {
	if d.config == nil {
		writeJSON(w, http.StatusOK, configRequest{})
		return
	}
	writeJSON(w, http.StatusOK, d.config.load())
}

// saveConfig validates and persists the onboarding config: repo must look
// like OWNER/REPOSITORY and repoPath must be an existing git worktree.
func (d *DaemonHTTP) saveConfig(w http.ResponseWriter, r *http.Request) {
	if d.config == nil {
		http.Error(w, "config storage unavailable", http.StatusInternalServerError)
		return
	}
	var cfg configRequest
	if err := json.NewDecoder(r.Body).Decode(&cfg); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}
	if !strings.Contains(cfg.Repo, "/") || strings.ContainsAny(cfg.Repo, " \t") {
		http.Error(w, "repo must be OWNER/REPOSITORY", http.StatusBadRequest)
		return
	}
	if cfg.RepoPath != "" {
		if out, err := git(cfg.RepoPath, "rev-parse", "--is-inside-work-tree"); err != nil || strings.TrimSpace(out) != "true" {
			http.Error(w, "repoPath is not a git working tree", http.StatusBadRequest)
			return
		}
	}
	if err := d.config.save(cfg); err != nil {
		http.Error(w, "could not write config", http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "saved", "note": "reiniciá la app para aplicar la configuración"})
}

// defaultConfigPath anchors orchestrator.json beside the daemon binary.
func defaultConfigPath() string {
	exe, err := os.Executable()
	if err != nil {
		return "orchestrator.json"
	}
	return filepath.Join(filepath.Dir(exe), "orchestrator.json")
}

// evidenceReceiptValid recomputes the receipt over the package's current
// facts and compares it with the receipt the package carries: false means
// the evidence was edited after its worker froze it. Receipts are frozen at
// worker exit (journal entry kind "receipt").
func evidenceReceiptValid(ev Evidence) bool {
	if ev.Receipt == "" {
		return false
	}
	return evidenceReceipt(ev) == ev.Receipt
}

// listProjects reports the managed repositories.
func (d *DaemonHTTP) listProjects(w http.ResponseWriter, _ *http.Request) {
	roster := []Project{}
	if d.projects != nil {
		roster = d.projects.List()
	}
	writeJSON(w, http.StatusOK, roster)
}

// addProject validates and registers one repository for a multi-repo fleet.
func (d *DaemonHTTP) addProject(w http.ResponseWriter, r *http.Request) {
	if d.projects == nil {
		http.Error(w, "projects registry unavailable", http.StatusInternalServerError)
		return
	}
	var project Project
	if err := json.NewDecoder(r.Body).Decode(&project); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}
	if err := d.projects.Add(project); err != nil {
		if err.Error() == ErrDuplicateSession.Error() {
			http.Error(w, "project already registered", http.StatusConflict)
			return
		}
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusCreated, project)
}
