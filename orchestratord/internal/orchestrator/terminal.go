package orchestrator

import (
	"context"
	"net/http"
	"sync"

	"github.com/coder/websocket"
)

// terminalIO is the platform-neutral handle of a worker's pseudo terminal:
// an os.File PTY on Unix, a ConPTY on Windows.
type terminalIO interface {
	Read([]byte) (int, error)
	Write([]byte) (int, error)
	Close() error
}

// TerminalHub owns the PTY-backed terminal for one session: at most one shell
// at a time, shared by every attached websocket client (the same session
// viewed from two windows shares one terminal), torn down with the daemon.
type TerminalHub struct {
	mu        sync.Mutex
	terminals map[SessionID]*sessionTerminal
}

// sessionTerminal is one live interactive shell anchored in a worktree.
type sessionTerminal struct {
	hub    *TerminalHub
	id     SessionID
	dir    string
	io     terminalIO
	cancel context.CancelFunc
	exited chan struct{} // closed when the shell dies

	mu      sync.Mutex
	clients map[chan []byte]struct{}
}

// NewTerminalHub returns an empty hub.
func NewTerminalHub() *TerminalHub {
	return &TerminalHub{terminals: make(map[SessionID]*sessionTerminal)}
}

// Open upgrades the websocket request and bridges it to the session's PTY:
// the first client starts the shell inside the worktree; further clients
// attach to the same terminal.
func (h *TerminalHub) Open(w http.ResponseWriter, r *http.Request, id SessionID, dir string) error {
	// The boundary middleware already filtered browser origins
	// (originAllowed); coder/websocket's own cross-origin check compares
	// against the request host, which would reject the desktop shell's
	// tauri origin — authorize exactly the allowed origin hosts instead.
	opts := &websocket.AcceptOptions{
		OriginPatterns: wsOriginPatterns(),
	}
	conn, err := websocket.Accept(w, r, opts)
	if err != nil {
		return err
	}
	// The terminal's closure status belongs to the connection, not to any
	// goroutine: every exit path closes explicitly and this deferred default
	// only fires on unexpected paths (never after a normal closure, which
	// already closed the connection with its own status).
	closeStatus := websocket.StatusInternalError
	closeReason := "terminal closed"
	var closeMu sync.Mutex
	closeWith := func(status websocket.StatusCode, reason string) {
		closeMu.Lock()
		closeStatus, closeReason = status, reason
		closeMu.Unlock()
		_ = conn.Close(status, reason)
	}
	defer func() {
		closeMu.Lock()
		status, reason := closeStatus, closeReason
		closeMu.Unlock()
		_ = conn.Close(status, reason)
	}()

	term, err := h.acquire(id, dir)
	if err != nil {
		_ = conn.Close(websocket.StatusUnsupportedData, err.Error())
		return err
	}

	// Register this client's mailbox and start its keystroke pump.
	mailbox := term.subscribe()
	defer term.unsubscribe(mailbox)

	go func() {
		for {
			_, data, err := conn.Read(r.Context())
			if err != nil {
				closeWith(websocket.StatusNormalClosure, "client left")
				return
			}
			if _, err := term.io.Write(data); err != nil {
				return
			}
		}
	}()

	// Drain the mailbox into this client's websocket until the shell or the
	// client goes away.
	for {
		select {
		case burst, ok := <-mailbox:
			if !ok {
				closeWith(websocket.StatusNormalClosure, "shell exited")
				return nil
			}
			if err := conn.Write(r.Context(), websocket.MessageBinary, burst); err != nil {
				return nil
			}
		case <-r.Context().Done():
			closeWith(websocket.StatusNormalClosure, "client left")
			return nil
		case <-term.exited:
			closeWith(websocket.StatusNormalClosure, "shell exited")
			return nil
		}
	}
}

// subscribe registers a mailbox; the PTY broadcast delivers every burst.
func (t *sessionTerminal) subscribe() chan []byte {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.clients == nil {
		t.clients = make(map[chan []byte]struct{})
	}
	mailbox := make(chan []byte, 64)
	t.clients[mailbox] = struct{}{}
	return mailbox
}

func (t *sessionTerminal) unsubscribe(mailbox chan []byte) {
	t.mu.Lock()
	defer t.mu.Unlock()
	delete(t.clients, mailbox)
}

// broadcast delivers one PTY burst to every mailbox; a slow or gone client
// drops its burst instead of stalling the shell.
func (t *sessionTerminal) broadcast(burst []byte) {
	t.mu.Lock()
	defer t.mu.Unlock()
	for mailbox := range t.clients {
		select {
		case mailbox <- burst:
		default: // mailbox full: the client is too slow, drop the burst
		}
	}
}

// acquire returns the session's terminal, starting it when this is the first
// attach. The worktree anchors the shell; the daemon's lifecycle bounds it.
func (h *TerminalHub) acquire(id SessionID, dir string) (*sessionTerminal, error) {
	h.mu.Lock()
	if term, ok := h.terminals[id]; ok {
		h.mu.Unlock()
		return term, nil
	}
	h.mu.Unlock()

	if dir == "" {
		return nil, errNoWorktree
	}
	if statErr := statDir(dir); statErr != nil {
		return nil, errNoWorktree
	}

	ctx, cancel := context.WithCancel(context.Background())
	term, err := newSessionTerminal(ctx, dir)
	if err != nil {
		cancel()
		return nil, err
	}
	term.hub, term.id, term.dir, term.cancel = h, id, dir, cancel

	// The single PTY reader: every burst broadcasts to all clients.
	go func() {
		buf := make([]byte, 4096)
		for {
			n, readErr := term.io.Read(buf)
			if n > 0 {
				burst := make([]byte, n)
				copy(burst, buf[:n])
				term.broadcast(burst)
			}
			if readErr != nil {
				close(term.exited)
				return
			}
		}
	}()

	h.mu.Lock()
	h.terminals[id] = term
	h.mu.Unlock()

	// When the shell exits — or the daemon dies — drop the terminal so the
	// next attach starts a fresh one.
	go func() {
		select {
		case <-term.exited:
		case <-ctx.Done():
			_ = term.io.Close()
		}
		h.mu.Lock()
		if h.terminals[id] == term {
			delete(h.terminals, id)
		}
		h.mu.Unlock()
		_ = term.io.Close()
		cancel()
	}()
	return term, nil
}

// Stop kills the session's terminal shell, if any.
func (h *TerminalHub) Stop(id SessionID) {
	h.mu.Lock()
	term, ok := h.terminals[id]
	if ok {
		delete(h.terminals, id)
	}
	h.mu.Unlock()
	if ok {
		_ = term.io.Close()
	}
}
