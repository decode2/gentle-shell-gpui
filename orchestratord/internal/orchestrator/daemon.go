package orchestrator

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"time"
)

// DaemonHTTP serves the fleet over local HTTP. It is the read/write surface
// for desktop, web, and mobile clients: one protocol, localhost bound.
type DaemonHTTP struct {
	fleet    *Fleet
	journal  *Journal
	launcher *Launcher
	agents   *AgentDetector
	terminal *TerminalHub
	config   *configStore
	projects *Projects
}

// corsAllowedOrigins are the desktop shell origins allowed to call the
// daemon from a webview. Unknown origins (arbitrary websites) get no CORS
// headers, so a drive-by page cannot create sessions on the local daemon.
// Non-browser clients send no Origin and are never blocked.
var corsAllowedOrigins = []string{
	"tauri://localhost",
	"http://tauri.localhost",
	"https://tauri.localhost",
	"http://localhost:5173", // vite dev server
}

// withCORS answers preflights and echoes the allow-listed origin. It is
// localhost confinement by allow-list, not by wildcard.
func withCORS(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		origin := r.Header.Get("Origin")
		allowed := false
		for _, candidate := range corsAllowedOrigins {
			if origin == candidate {
				allowed = true
				break
			}
		}
		if allowed {
			w.Header().Set("Access-Control-Allow-Origin", origin)
			w.Header().Add("Vary", "Origin")
		}
		if r.Method == http.MethodOptions {
			w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
			w.Header().Set("Access-Control-Allow-Headers", "Content-Type")
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

// NewDaemonHTTP returns the fleet's HTTP handler.
func NewDaemonHTTP(fleet *Fleet) http.Handler {
	return NewDaemonServer(fleet, nil, nil)
}

// NewDaemonServer returns the handler with journal and launch support: the
// surface the desktop app and the CLI use. A nil journal or launcher keeps
// their endpoints inert (events read empty, launch/stop report 400).
func NewDaemonServer(fleet *Fleet, journal *Journal, launcher *Launcher) http.Handler {
	return NewDaemonServerWithDetector(fleet, journal, launcher, nil)
}

// NewDaemonServerWithDetector also wires agent detection for GET /agents; a
// nil detector serves an empty roster.
func NewDaemonServerWithDetector(fleet *Fleet, journal *Journal, launcher *Launcher, agents *AgentDetector) http.Handler {
	return NewDaemonServerFull(fleet, journal, launcher, agents, NewTerminalHub())
}

// NewDaemonServerFull wires every service; terminal may be nil to disable
// the embedded terminal route (tests, headless).
func NewDaemonServerFull(fleet *Fleet, journal *Journal, launcher *Launcher, agents *AgentDetector, terminal *TerminalHub) http.Handler {
	return newDaemonServer(fleet, journal, launcher, agents, terminal, nil)
}

func newDaemonServer(fleet *Fleet, journal *Journal, launcher *Launcher, agents *AgentDetector, terminal *TerminalHub, config *configStore) http.Handler {
	return newDaemonServerProjects(fleet, journal, launcher, agents, terminal, config, NewProjects())
}

func newDaemonServerProjects(fleet *Fleet, journal *Journal, launcher *Launcher, agents *AgentDetector, terminal *TerminalHub, config *configStore, projects *Projects) http.Handler {
	d := &DaemonHTTP{fleet: fleet, journal: journal, launcher: launcher, agents: agents, terminal: terminal, config: config, projects: projects}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /board", d.board)
	mux.HandleFunc("GET /board/live", d.liveBoard)
	mux.HandleFunc("GET /agents", d.listAgents)
	mux.HandleFunc("GET /version", d.daemonVersion)
	mux.HandleFunc("GET /config", d.getConfig)
	mux.HandleFunc("POST /config", d.saveConfig)
	mux.HandleFunc("GET /projects", d.listProjects)
	mux.HandleFunc("POST /projects", d.addProject)
	mux.HandleFunc("GET /sessions/{id}/terminal", d.sessionTerminalWS)
	mux.HandleFunc("GET /sessions/{id}/evidence", d.sessionEvidence)
	mux.HandleFunc("POST /sessions", d.startSession)
	mux.HandleFunc("GET /sessions/{id}/events", d.sessionEvents)
	mux.HandleFunc("GET /sessions/{id}/diff", d.sessionDiff)
	mux.HandleFunc("POST /sessions/{id}/stop", d.stopSession)
	mux.HandleFunc("POST /sessions/{id}/chat", d.sendChat)
	mux.HandleFunc("POST /sessions/{id}/terminal", d.openSessionTerminal)
	return withBoundary(withCORS(mux))
}

// sessionTerminalWS bridges the embedded terminal websocket for the session.
func (d *DaemonHTTP) sessionTerminalWS(w http.ResponseWriter, r *http.Request) {
	if d.terminal == nil {
		http.Error(w, "embedded terminal disabled", http.StatusNotImplemented)
		return
	}
	s, err := d.fleet.Get(SessionID(r.PathValue("id")))
	if err != nil {
		http.Error(w, "unknown session", http.StatusNotFound)
		return
	}
	_ = d.terminal.Open(w, r, s.ID, s.Worktree)
}

// sessionDiff returns the session's worktree diff against its base.
func (d *DaemonHTTP) sessionDiff(w http.ResponseWriter, r *http.Request) {
	s, err := d.fleet.Get(SessionID(r.PathValue("id")))
	if err != nil {
		http.Error(w, "unknown session", http.StatusNotFound)
		return
	}
	info, err := worktreeDiff(s.Worktree, s.Base)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, info)
}

// openSessionTerminal opens the machine's terminal at the worktree.
func (d *DaemonHTTP) openSessionTerminal(w http.ResponseWriter, r *http.Request) {
	s, err := d.fleet.Get(SessionID(r.PathValue("id")))
	if err != nil {
		http.Error(w, "unknown session", http.StatusNotFound)
		return
	}
	if s.Worktree == "" {
		http.Error(w, "session has no worktree", http.StatusBadRequest)
		return
	}
	cmd, ok := terminalCommandForOS(s.Worktree)
	if !ok {
		http.Error(w, "no known terminal emulator on this machine", http.StatusBadRequest)
		return
	}
	if err := cmd.Start(); err != nil {
		http.Error(w, "terminal failed to start", http.StatusInternalServerError)
		return
	}
	go func() { _ = cmd.Wait() }()
	w.WriteHeader(http.StatusAccepted)
}

// listAgents reports which coding agent CLIs are installed on this machine.
func (d *DaemonHTTP) listAgents(w http.ResponseWriter, _ *http.Request) {
	roster := []DetectedAgent{}
	if d.agents != nil {
		roster = d.agents.Detect(false)
	}
	writeJSON(w, http.StatusOK, roster)
}

// liveBoard streams the board as Server-Sent Events: one snapshotted board
// per tick until the client disconnects or the request context ends. It is
// the zero-dependency live channel for the spike; a WebSocket transport can
// replace it in the Tauri phase without changing the payload.
func (d *DaemonHTTP) liveBoard(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.WriteHeader(http.StatusOK)

	// A 5s cadence keeps the board near-live on reconnect (a client that
	// re-opens its stream waits at most one tick); a comment heartbeat
	// keeps proxies from reaping an idle stream.
	boardTicker := time.NewTicker(5 * time.Second)
	defer boardTicker.Stop()
	heartbeatTicker := time.NewTicker(15 * time.Second)
	defer heartbeatTicker.Stop()

	writeBoard := func() {
		payload, err := json.Marshal(d.fleet.Board())
		if err != nil {
			return
		}
		fmt.Fprintf(w, "event: board\ndata: %s\n\n", payload)
		flusher.Flush()
	}
	// Heartbeat: an SSE comment every tick between board pushes; browsers
	// keep the stream alive and proxies do not reap idle connections.
	heartbeat := func() {
		fmt.Fprint(w, ": keep-alive\n\n")
		flusher.Flush()
	}

	writeBoard()
	for {
		select {
		case <-r.Context().Done():
			return
		case <-heartbeatTicker.C:
			heartbeat()
		case <-boardTicker.C:
			writeBoard()
		}
	}
}

func (d *DaemonHTTP) board(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, d.fleet.Board())
}

// startSessionRequest is the JSON body of POST /sessions.
type startSessionRequest struct {
	ID       string `json:"id"`
	Task     string `json:"task"`
	Agent    string `json:"agent"`
	Branch   string `json:"branch"`
	Worktree string `json:"worktree"`
	// Launch asks the daemon to create the worktree and spawn the agent
	// CLI instead of registering an externally-managed session.
	Launch bool `json:"launch"`
}

func (d *DaemonHTTP) startSession(w http.ResponseWriter, r *http.Request) {
	var req startSessionRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}
	if req.ID == "" || req.Branch == "" {
		http.Error(w, "id and branch are required", http.StatusBadRequest)
		return
	}

	if req.Launch {
		if d.launcher == nil {
			http.Error(w, "launch requires a local repository path (daemon --repo-path)", http.StatusBadRequest)
			return
		}
		launchCtx := context.WithoutCancel(r.Context())
		err := d.launcher.StartTask(launchCtx, Session{
			ID:     SessionID(req.ID),
			Task:   req.Task,
			Agent:  req.Agent,
			Branch: req.Branch,
		})
		if err != nil {
			writeStartError(w, err)
			return
		}
		writeJSON(w, http.StatusCreated, fleetCard(d.fleet, SessionID(req.ID)))
		return
	}

	err := d.fleet.Start(Session{
		ID:       SessionID(req.ID),
		Task:     req.Task,
		Agent:    req.Agent,
		Branch:   req.Branch,
		Worktree: req.Worktree,
	})
	if err != nil {
		writeStartError(w, err)
		return
	}
	writeJSON(w, http.StatusCreated, fleetCard(d.fleet, SessionID(req.ID)))
}

func writeStartError(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, ErrDuplicateSession):
		http.Error(w, "session already registered", http.StatusConflict)
	case errors.Is(err, ErrProtectedBranch), errors.Is(err, ErrInvalidBranch):
		http.Error(w, err.Error(), http.StatusBadRequest)
	default:
		http.Error(w, "internal error", http.StatusInternalServerError)
	}
}

// sessionEvents returns the session journal: the worker's recorded life.
func (d *DaemonHTTP) sessionEvents(w http.ResponseWriter, r *http.Request) {
	id := SessionID(r.PathValue("id"))
	var entries []JournalEntry
	if d.journal != nil {
		entries, _ = d.journal.ForEvidence(id, maxEvidenceJournalEntries)
	}
	if entries == nil {
		entries = []JournalEntry{}
	}
	writeJSON(w, http.StatusOK, entries)
}

// sendChat delivers one message to the running worker's stdin.
func (d *DaemonHTTP) sendChat(w http.ResponseWriter, r *http.Request) {
	if d.launcher == nil {
		http.Error(w, "chat requires the daemon launcher", http.StatusBadRequest)
		return
	}
	var body struct {
		Message string `json:"message"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}
	if err := d.launcher.SendToWorker(SessionID(r.PathValue("id")), body.Message); err != nil {
		if errors.Is(err, ErrUnknownSession) {
			http.Error(w, "no running worker for that session", http.StatusNotFound)
			return
		}
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	w.WriteHeader(http.StatusAccepted)
}

// stopSession kills a running worker spawned by the daemon.
func (d *DaemonHTTP) stopSession(w http.ResponseWriter, r *http.Request) {
	id := SessionID(r.PathValue("id"))
	if d.launcher == nil {
		http.Error(w, "stop requires the daemon launcher", http.StatusBadRequest)
		return
	}
	if err := d.launcher.Stop(id); err != nil {
		if errors.Is(err, ErrUnknownSession) {
			http.Error(w, "no running worker for that session", http.StatusNotFound)
			return
		}
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	w.WriteHeader(http.StatusAccepted)
}

// fleetCard snapshots one session as its card; used for the 201 response.
func fleetCard(f *Fleet, id SessionID) SessionCard {
	card := SessionCard{ID: string(id)}
	if s, err := f.Get(id); err == nil {
		card.Task, card.Agent, card.Branch = s.Task, s.Agent, s.Branch
	}
	return card
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
