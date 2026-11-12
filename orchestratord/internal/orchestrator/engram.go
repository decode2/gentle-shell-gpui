package orchestrator

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"time"
)

// EngramBridge publishes fleet memory to a local Engram server (POST
// /sessions, POST /observations). Every method degrades to nil on transport
// or server errors: memory must never break a worker run, and the journal
// remains the local evidence trail.
type EngramBridge struct {
	// BaseURL is the engram serve root, e.g. http://127.0.0.1:7437.
	BaseURL string
	// Project is the Engram project observations attach to.
	Project string

	client *http.Client
}

func (b *EngramBridge) httpClient() *http.Client {
	if b.client == nil {
		b.client = &http.Client{Timeout: 10 * time.Second}
	}
	return b.client
}

// post sends a JSON body and accepts any 2xx.
func (b *EngramBridge) post(ctx context.Context, path string, body any) error {
	payload, err := json.Marshal(body)
	if err != nil {
		return err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, b.BaseURL+path, bytes.NewReader(payload))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := b.httpClient().Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("engram %s returned %d", path, resp.StatusCode)
	}
	return nil
}

// StartSession registers the worker's session in Engram so its observations
// have a home.
func (b *EngramBridge) StartSession(ctx context.Context, s Session) error {
	if b.BaseURL == "" {
		return nil
	}
	err := b.post(ctx, "/sessions", map[string]any{
		"id":        string(s.ID),
		"project":   b.Project,
		"directory": s.Worktree,
	})
	if err != nil {
		return nil // degrade
	}
	return nil
}

// PublishDecision records one worker DECISION line as a project-scoped
// decision observation, immediately: fleet memory grows while the worker
// runs, not only at exit.
func (b *EngramBridge) PublishDecision(ctx context.Context, s Session, decision string) error {
	if b.BaseURL == "" || strings.TrimSpace(decision) == "" {
		return nil
	}
	err := b.post(ctx, "/observations", map[string]any{
		"session_id": string(s.ID),
		"type":       "decision",
		"title":      fmt.Sprintf("worker %s decides: %.60s", s.ID, decision),
		"content":    decision,
		"project":    b.Project,
		"scope":      "project",
	})
	if err != nil {
		return nil // degrade
	}
	return nil
}

// PublishOutcome records what a finished worker did as a project-scoped
// decision observation. An empty summary is a local no-op (Engram rejects
// untitled content).
func (b *EngramBridge) PublishOutcome(ctx context.Context, id SessionID, task, summary string) error {
	if b.BaseURL == "" || strings.TrimSpace(summary) == "" {
		return nil
	}
	err := b.post(ctx, "/observations", map[string]any{
		"session_id": string(id),
		"type":       "decision",
		"title":      fmt.Sprintf("worker %s: %s", id, task),
		"content":    summary,
		"project":    b.Project,
		"scope":      "project",
	})
	if err != nil {
		return nil // degrade
	}
	return nil
}
