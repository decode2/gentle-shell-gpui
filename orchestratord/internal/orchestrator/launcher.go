package orchestrator

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"runtime"
	"strings"
	"sync"
	"time"
)

// workProcess wraps one running worker process. done closes exactly when
// cmd.Wait returned: every kill/monitor path checks it instead of touching
// cmd fields that Wait writes concurrently.
type workProcess struct {
	cmd    *exec.Cmd
	done   chan struct{}
	stdin  io.WriteCloser
	stdout io.ReadCloser
}

// launch wires the pipes and starts the process; on success the stdin is
// held for chat delivery until the worker exits. Platform traits (process
// group, daemon-death linkage) apply here so every builder benefits.
func (wp *workProcess) launch() error {
	applyProcessPlatformTraits(wp.cmd)
	stdin, stdinErr := wp.cmd.StdinPipe()
	stdout, stdoutErr := wp.cmd.StdoutPipe()
	if stdinErr != nil || stdoutErr != nil {
		return errors.New("worker pipes unavailable")
	}
	wp.stdin = stdin
	wp.stdout = stdout
	if err := wp.cmd.Start(); err != nil {
		return err
	}
	return nil
}

// finished reports whether the worker already exited.
func (wp *workProcess) finished() bool {
	select {
	case <-wp.done:
		return true
	default:
		return false
	}
}

// Launcher turns a session request into a running worker: it creates the
// task's worktree, registers the session, spawns the agent CLI there, and
// records everything in the journal. Worker stdout lines that start with
// "QUESTION:" route to the session's inbox so the human sees them.
type Launcher struct {
	// Root is the launcher's lifecycle context: workers derive their process
	// context from it, so canceling Root (daemon shutdown, app exit) kills
	// every running worker. When nil, StartTask's ctx is used.
	Root      context.Context
	Fleet     *Fleet
	Router    *Router
	Journal   *Journal
	Worktrees *WorktreeManager
	// Base is the branch workers start from (never a protected branch as
	// the worker's own branch; the manager refuses that).
	Base string
	// Command builds the worker process; tests replace it. The default
	// spawns the agent CLI with the task text.
	Command func(ctx context.Context, s Session, dir string) *workProcess
	// Engram publishes fleet memory when set (PRD F5); failures degrade.
	Engram *EngramBridge
	// ReceiptGate requires the frozen evidence receipt before a launched
	// session may graduate to ready-to-merge (F4). Default on.
	ReceiptGate bool
	// Projects resolves per-project worktree managers for multi-repo fleets;
	// nil or an unknown project falls back to the fixed Worktrees.
	Projects *Projects

	mu       sync.Mutex
	running  map[SessionID]*workProcess
	stopping map[SessionID]bool
}

// agentArgTemplates maps an agent id to its headless invocation prefix; the
// task text is always the last argument. Per-agent argv refinements belong
// to the adapters phase; these cover the common CLIs honestly.
var agentArgTemplates = map[string][]string{
	"pi":     {"--print", "--mode", "text"},
	"claude": {"-p"},
	"codex":  {"exec"},
	"gemini": {},
	"aider":  {"--message"},
}

func defaultAgentCommand(ctx context.Context, s Session, dir string) *workProcess {
	args := append([]string(nil), agentArgTemplates[s.Agent]...)
	args = append(args, workerPrompt(s))
	invocation := agentInvocation(resolveAgentExecutable(s.Agent), runtime.GOOS)
	cmd := exec.CommandContext(ctx, invocation.exe, append(invocation.prefix, args...)...)
	cmd.Dir = dir
	applyProcessPlatformTraits(cmd)
	return &workProcess{cmd: cmd}
}

// agentInvocation turns a resolved executable path into the process to run:
// npm shims (.cmd/.bat) cannot exec directly on Windows, they need
// "cmd /c"; everything else (and every unix path) runs as-is.
func agentInvocation(resolved, goos string) struct {
	exe    string
	prefix []string
} {
	lower := strings.ToLower(resolved)
	if goos == "windows" && (strings.HasSuffix(lower, ".cmd") || strings.HasSuffix(lower, ".bat")) {
		return struct {
			exe    string
			prefix []string
		}{exe: "cmd", prefix: []string{"/c", resolved}}
	}
	return struct {
		exe    string
		prefix []string
	}{exe: resolved}
}

// resolveAgentExecutable is the seam for tests: LookPath by default.
var resolveAgentExecutable = func(command string) string {
	path, err := exec.LookPath(command)
	if err != nil {
		return command
	}
	return path
}

// StartTask validates, creates the worktree, registers the session, and
// launches the worker in the background. It returns after launch, not after
// the worker finishes; the journal carries the run.
func (l *Launcher) StartTask(ctx context.Context, session Session) error {
	if err := validateBranch(session.Branch); err != nil {
		return err
	}
	base := l.Base
	if base == "" {
		base = "main"
	}
	manager := l.Worktrees
	if l.Projects != nil && session.Project != "" {
		resolved, ok := l.Projects.ManagerFor(session.Project)
		if !ok {
			return fmt.Errorf("orchestrator: unknown project %q", session.Project)
		}
		manager = resolved
	}
	if _, err := l.Fleet.Get(session.ID); err == nil {
		return ErrDuplicateSession
	}

	path, err := manager.Create(session.Branch, base)
	if err != nil {
		return err
	}
	session.Worktree = path
	session.Base = base
	if err := l.Fleet.Start(session); err != nil {
		_ = l.Worktrees.Remove(session.Branch)
		return err
	}
	l.Journal.Append(session.ID, "launched", path)
	if l.Engram != nil {
		_ = l.Engram.StartSession(ctx, session)
	}

	l.mu.Lock()
	if l.running == nil {
		l.running = make(map[SessionID]*workProcess)
		l.stopping = make(map[SessionID]bool)
	}
	workerCtx := l.Root
	if workerCtx == nil {
		workerCtx = ctx
	}
	command := l.Command
	if command == nil {
		command = defaultAgentCommand
	}
	wp := command(workerCtx, session, session.Worktree)
	wp.done = make(chan struct{})
	l.running[session.ID] = wp
	l.mu.Unlock()

	// Pipes are created and the process started synchronously: by the time
	// StartTask returns, SendToWorker can write and Stop can kill — no
	// launch race.
	if startErr := wp.launch(); startErr != nil {
		l.Journal.Append(session.ID, "exited", fmt.Sprintf("launch failed: %v", startErr))
		l.finish(session, fmt.Sprintf("worker launch failed: %v", startErr))
		l.mu.Lock()
		delete(l.running, session.ID)
		l.mu.Unlock()
		return startErr
	}
	l.Journal.Append(session.ID, "started", session.Worktree)
	l.Fleet.Update(session.ID, func(f *Facts) {
		f.Activity.Running = true
		if l.ReceiptGate {
			f.ReceiptRequired = true
		}
	})
	go l.run(workerCtx, session, wp)
	return nil
}

// run records the worker's output until exit. The process was already
// registered by StartTask; a stop that arrived first wins the journal.
func (l *Launcher) run(ctx context.Context, session Session, wp *workProcess) {
	l.mu.Lock()
	stopping := l.stopping[session.ID]
	l.mu.Unlock()

	// Lifecycle watchdog: when the daemon's context cancels (app close),
	// kill the worker's whole process tree — grandchildren included — so
	// nothing outlives the app.
	go func() {
		<-ctx.Done()
		l.mu.Lock()
		current := l.running[session.ID]
		live := current == wp && wp.cmd.Process != nil
		l.mu.Unlock()
		if live && !wp.finished() {
			_ = killProcessTree(wp.cmd, wp.cmd.Process)
		}
	}()

	if stopping {
		l.Journal.Append(session.ID, "stopped", "stopped before launch")
		l.freezeReceipt(session, "Detenido sin iniciar")
		l.finish(session, "worker stopped before launch")
		return
	}

	// A stop that raced the launch wins here: kill the just-started process
	// so the wait below observes it.
	l.mu.Lock()
	stopping = l.stopping[session.ID]
	l.mu.Unlock()
	if stopping {
		_ = wp.cmd.Process.Kill()
	}

	var wg sync.WaitGroup
	scan := func(reader interface{ Read([]byte) (int, error) }) {
		defer wg.Done()
		scanner := bufio.NewScanner(reader)
		for scanner.Scan() {
			line := scanner.Text()
			if says, ok := strings.CutPrefix(line, "SAY:"); ok {
				l.Journal.Append(session.ID, "chat-in", strings.TrimSpace(says))
				continue
			}
			l.Journal.Append(session.ID, "output", line)
			if question, ok := strings.CutPrefix(line, "QUESTION:"); ok {
				_ = l.Router.Route(Event{Kind: EventQuestion, Session: session.ID, Note: strings.TrimSpace(question)})
				continue
			}
			if decision, ok := strings.CutPrefix(line, "DECISION:"); ok {
				text := strings.TrimSpace(decision)
				l.Journal.Append(session.ID, "decision", text)
				if l.Engram != nil {
					_ = l.Engram.PublishDecision(ctx, session, text)
				}
				continue
			}
			if summary, ok := strings.CutPrefix(line, "SUMMARY:"); ok {
				l.Journal.Append(session.ID, "summary", strings.TrimSpace(summary))
				continue
			}
		}
	}
	wg.Add(2)
	go scan(wp.stdout)
	stderr, _ := wp.cmd.StderrPipe()
	if stderr != nil {
		go scan(stderr)
	} else {
		wg.Done()
	}
	wg.Wait()

	waitErr := wp.cmd.Wait()
	close(wp.done)
	text := "ok"
	if waitErr != nil {
		text = waitErr.Error()
	}
	l.mu.Lock()
	wasStopping := l.stopping[session.ID]
	delete(l.running, session.ID)
	l.mu.Unlock()
	if wasStopping {
		l.Journal.Append(session.ID, "stopped", text)
		l.freezeReceipt(session, "Detenido")
		l.finish(session, "worker stopped: "+text)
	} else {
		l.Journal.Append(session.ID, "exited", text)
		l.freezeReceipt(session, "Exitado")
		l.finish(session, "worker exited ("+text+"); task: "+session.Task+"; branch: "+session.Branch)
	}
}

// freezeReceipt stamps the session's evidence receipt at worker exit: the
// package's facts at this moment are what the receipt attests to. Freezing
// releases the evidence gate (F4).
func (l *Launcher) freezeReceipt(session Session, status string) {
	ev := Evidence{
		SessionID: string(session.ID),
		Task:      session.Task,
		Agent:     session.Agent,
		Branch:    session.Branch,
		Status:    status,
	}
	receipt := evidenceReceipt(ev)
	l.Journal.Append(session.ID, "receipt", receipt)
	_, _ = l.Fleet.Update(session.ID, func(f *Facts) { f.ReceiptValid = true })
	if l.Engram != nil {
		_ = l.Engram.PublishOutcome(context.Background(), session.ID, session.Task, "receipt frozen "+receipt)
	}
}

// KillAll kills every running worker's process tree; the shutdown path
// sweeps stragglers after the lifecycle context canceled. ProcessState is
// only read under l.mu — Wait writes it concurrently.
func (l *Launcher) KillAll() {
	l.mu.Lock()
	live := make([]*workProcess, 0, len(l.running))
	for id, wp := range l.running {
		l.stopping[id] = true
		if wp.cmd.Process != nil && !wp.finished() {
			live = append(live, wp)
		}
	}
	l.mu.Unlock()
	for _, wp := range live {
		_ = killProcessTree(wp.cmd, wp.cmd.Process)
	}
}

// SendToWorker delivers one chat message to the running worker's stdin:
// the agent reads it as a line next to its task stream.
func (l *Launcher) SendToWorker(id SessionID, message string) error {
	l.mu.Lock()
	wp, ok := l.running[id]
	l.mu.Unlock()

	if !ok || wp.stdin == nil || wp.finished() {
		return ErrUnknownSession
	}
	if strings.TrimSpace(message) == "" {
		return errors.New("orchestrator: empty chat message")
	}
	if _, err := io.WriteString(wp.stdin, strings.TrimRight(message, "\n")+"\n"); err != nil {
		return fmt.Errorf("orchestrator: chat delivery failed: %w", err)
	}
	l.Journal.Append(id, "chat-out", strings.TrimSpace(message))
	return nil
}

// finish marks the worker idle in the fleet and records the outcome in
// fleet memory when the bridge is configured.
func (l *Launcher) finish(session Session, summary string) {
	_, _ = l.Fleet.Update(session.ID, func(f *Facts) {
		f.Activity.Running = false
		f.Activity.Detail = ""
	})
	if l.Engram != nil {
		_ = l.Engram.PublishOutcome(context.Background(), session.ID, session.Task, summary)
	}
}

// Stop kills a running worker and its whole process tree. If the process
// has not started yet, the stopping flag makes the launch path kill it
// right after start. The journal records the stop; the worktree and branch
// survive for inspection.
func (l *Launcher) Stop(id SessionID) error {
	l.mu.Lock()
	wp, ok := l.running[id]
	if ok {
		l.stopping[id] = true
	}
	var process *os.Process
	if ok && wp.cmd.Process != nil && !wp.finished() {
		process = wp.cmd.Process
	}
	l.mu.Unlock()

	if !ok {
		return ErrUnknownSession
	}
	if process == nil {
		return nil
	}
	if err := killProcessTree(wp.cmd, process); err != nil {
		return err
	}
	// Unblock the output scanners: with the tree gone the pipes close on
	// their own, but a grandchild holding stdout would otherwise stall the
	// journal and the receipt freeze.
	go func() {
		select {
		case <-wp.done:
		case <-time.After(2 * time.Second):
		}
		closeWorkerPipes(wp.cmd)
	}()
	return nil
}
