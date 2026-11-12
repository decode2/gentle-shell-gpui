package orchestrator

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"path/filepath"
	"time"
)

// DefaultFixCycles bounds how many automatic fix attempts a session gets
// before the router escalates to the human.
const DefaultFixCycles = 3

// DefaultPollInterval is the watcher cadence when none is configured.
const DefaultPollInterval = 30 * time.Second

// ServeOptions configures Serve.
type ServeOptions struct {
	// Repo is the OWNER/REPOSITORY handle the watcher polls via gh.
	Repo string
	// ListenAddr is the host:port for the local daemon, e.g. 127.0.0.1:8080.
	ListenAddr string
	// PollInterval is the watcher cadence; zero falls back to the default.
	PollInterval time.Duration
	// Source replaces the default GhPullSource; used by tests.
	Source PullSource
	// RepoPath is the local checkout used to create worker worktrees. When
	// set, POST /sessions accepts launch:true and spawns agent CLIs.
	RepoPath string
	// BaseBranch is what worker worktrees branch from (default main).
	BaseBranch string
	// PrintBoard renders the text board to stdout on every poll.
	PrintBoard bool
	// ConfigPath overrides where orchestrator.json lives (tests).
	ConfigPath string
	// SessionsPath overrides where sessions.json lives (tests).
	SessionsPath string
	// Command replaces the worker command builder (tests inject fakes).
	Command func(ctx context.Context, s Session, dir string) *workProcess
}

// ServeHandle is the running daemon: Addr is live for the whole run and Done
// receives the drain result once the context is canceled.
type ServeHandle struct {
	Addr string
	Done chan error
	// Rehydrated counts the sessions restored from the last run's persisted
	// history; the UI shows it so a restart is not a silent reset.
	Rehydrated int
}

// Serve wires the Phase 0 fleet — fleet, router, watcher, HTTP daemon with
// its live board stream — binds opts.ListenAddr (":0" picks a free port) and
// returns immediately with the handle. Cancel ctx to stop; Done reports
// ctx.Err() after the server drained.
func Serve(ctx context.Context, opts ServeOptions) (*ServeHandle, error) {
	fleet := NewFleet()
	rehydrated := 0
	router := NewRouter(DefaultFixCycles)

	source := opts.Source
	if source == nil {
		source = GhPullSource{Repo: opts.Repo}
	}
	interval := opts.PollInterval
	if interval <= 0 {
		interval = DefaultPollInterval
	}
	watcher := NewWatcher(fleet, router, source, interval)
	detector := NewAgentDetector(nil, 0)
	projects := NewProjects()

	// Session history survives daemon restarts: load what the last run
	// persisted and keep saving as the fleet changes.
	journal := NewJournal()

	// The default store anchors sessions.json beside the config; an explicit
	// SessionsPath is the file itself (tests and packaging overrides).
	sessionStore := NewSessionStore(filepath.Dir(configPath(opts)))
	if opts.SessionsPath != "" {
		sessionStore = &SessionStore{path: opts.SessionsPath}
	}
	sessions, err := sessionStore.Load()
	if err != nil {
		sessions = nil
	}
	for _, persisted := range sessions {
		if persisted.Session.ID == "" {
			continue
		}
		s := persisted.Session
		s.Facts.Activity.Detail = "De la corrida anterior"
		if startErr := fleet.Start(s); startErr != nil {
			continue
		}
		rehydrated++
		if persisted.Summary != "" {
			journal.Append(s.ID, "summary", persisted.Summary)
		}
	}
	go func() {
		ticker := time.NewTicker(persistEvery)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				rows := make([]PersistedSession, 0, fleet.Count())
				for _, card := range fleet.AllCards() {
					s, err := fleet.Get(SessionID(card.ID))
					if err != nil {
						continue
					}
					rows = append(rows, PersistedSession{Session: s})
				}
				_ = sessionStore.Save(rows)
			}
		}
	}()

	var launcher *Launcher
	if opts.RepoPath != "" {
		base := opts.BaseBranch
		if base == "" {
			base = "main"
		}
		lifecycle, cancelLifecycle := context.WithCancel(ctx)
		launcher = &Launcher{
			Root:        lifecycle,
			Fleet:       fleet,
			Router:      router,
			Journal:     journal,
			Worktrees:   &WorktreeManager{Repo: opts.RepoPath, Parent: filepath.Join(opts.RepoPath, ".orchestrator-worktrees")},
			Base:        base,
			ReceiptGate: true,
			Projects:    projects,
			Command:     opts.Command,
		}
		// Workers die with the daemon: canceling the lifecycle context kills
		// every running worker process (see Launcher.Root).
		go func() {
			<-ctx.Done()
			cancelLifecycle()
		}()
	}

	ln, err := net.Listen("tcp", opts.ListenAddr)
	if err != nil {
		return nil, fmt.Errorf("orchestrator: listen %s: %w", opts.ListenAddr, err)
	}

	handle := &ServeHandle{Addr: ln.Addr().String(), Done: make(chan error, 1), Rehydrated: rehydrated}
	server := &http.Server{Handler: newDaemonServer(fleet, journal, launcher, detector, nil, &configStore{path: configPath(opts)})}
	go func() {
		serveErr := server.Serve(ln)
		if serveErr != nil && serveErr != http.ErrServerClosed {
			handle.Done <- serveErr
			return
		}
		handle.Done <- nil
	}()

	go func() {
		for {
			pollErr := watcher.Poll(ctx)
			if opts.PrintBoard {
				if pollErr != nil {
					fmt.Printf("watcher poll: %v\n", pollErr)
				}
				fmt.Print(RenderBoard(fleet.Board()))
			}
			select {
			case <-ctx.Done():
				return
			case <-time.After(interval):
			}
		}
	}()

	go func() {
		<-ctx.Done()
		if launcher != nil {
			launcher.KillAll()
		}
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdownCtx)
	}()
	return handle, nil
}

// configPath anchors orchestrator.json beside the daemon binary so the
// shell re-reads it on restart; tests can override it.
func configPath(opts ServeOptions) string {
	if opts.ConfigPath != "" {
		return opts.ConfigPath
	}
	return defaultConfigPath()
}
