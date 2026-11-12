package orchestrator

import (
	"fmt"
	"strings"
	"sync"
)

// Project is one managed repository in a multi-repo fleet.
type Project struct {
	ID       string `json:"id"`
	Repo     string `json:"repo"`
	RepoPath string `json:"repoPath,omitempty"`
}

// Projects is the registry of managed repositories. ManagerFor hands each
// project its own worktree manager, so worktrees of different projects never
// share a parent directory.
type Projects struct {
	mu   sync.RWMutex
	byID map[string]Project
}

// NewProjects returns an empty registry.
func NewProjects() *Projects {
	return &Projects{byID: make(map[string]Project)}
}

// Add validates and registers a project: id required, repo in
// OWNER/REPOSITORY shape, and a repoPath — when given — must be a git tree.
func (p *Projects) Add(project Project) error {
	if strings.TrimSpace(project.ID) == "" {
		return fmt.Errorf("orchestrator: project id required")
	}
	if !strings.Contains(project.Repo, "/") || strings.ContainsAny(project.Repo, " \t") {
		return fmt.Errorf("orchestrator: repo must be OWNER/REPOSITORY")
	}
	if project.RepoPath != "" {
		if out, err := git(project.RepoPath, "rev-parse", "--is-inside-work-tree"); err != nil || strings.TrimSpace(out) != "true" {
			return fmt.Errorf("orchestrator: repoPath is not a git working tree")
		}
	}

	p.mu.Lock()
	defer p.mu.Unlock()
	if p.byID == nil {
		p.byID = make(map[string]Project)
	}
	if _, exists := p.byID[project.ID]; exists {
		return ErrDuplicateSession
	}
	p.byID[project.ID] = project
	return nil
}

// List returns the registered projects ordered by id.
func (p *Projects) List() []Project {
	p.mu.RLock()
	defer p.mu.RUnlock()

	out := make([]Project, 0, len(p.byID))
	for id := range p.byID {
		out = append(out, p.byID[id])
	}
	for i := 1; i < len(out); i++ {
		for j := i; j > 0 && out[j-1].ID > out[j].ID; j-- {
			out[j-1], out[j] = out[j], out[j-1]
		}
	}
	return out
}

// ManagerFor returns the project's worktree manager; ok is false for unknown
// projects or projects without a local checkout.
func (p *Projects) ManagerFor(id string) (*WorktreeManager, bool) {
	p.mu.RLock()
	defer p.mu.RUnlock()

	project, ok := p.byID[id]
	if !ok || project.RepoPath == "" {
		return nil, false
	}
	return &WorktreeManager{Repo: project.RepoPath, Parent: project.RepoPath + "/.orchestrator-worktrees"}, true
}
