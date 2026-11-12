// Command orchestratord runs the Gentle-AI orchestrator daemon: it serves the
// fleet over local HTTP (GET /board, GET /board/live via SSE, POST /sessions)
// and polls the repository's pull requests so workers get their CI and review
// feedback routed back to their own session.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"

	"github.com/decode2/gentle-shell-gpui/orchestratord/internal/orchestrator"
)

// openLogFile owns the daemon's log file beside its binary: the daemon also
// runs double-clicked (GUI subsystem, no console, no shell to capture
// stdout), so it never depends on stdout for diagnosability. Logs over 5 MB
// are truncated at start instead of growing forever.
func openLogFile() *os.File {
	exe, err := os.Executable()
	if err != nil {
		return nil
	}
	path := filepath.Join(filepath.Dir(exe), "orchestratord.log")
	flags := os.O_CREATE | os.O_WRONLY | os.O_APPEND
	if info, err := os.Stat(path); err == nil && info.Size() > 5<<20 {
		flags = os.O_CREATE | os.O_WRONLY | os.O_TRUNC
	}
	log, err := os.OpenFile(path, flags, 0o644)
	if err != nil {
		return nil
	}
	return log
}

func main() {
	repo := flag.String("repo", "", "OWNER/REPOSITORY the watcher polls via gh (required)")
	repoPath := flag.String("repo-path", "", "local checkout path; enables launching workers in their own worktrees")
	listen := flag.String("listen", "127.0.0.1:8094", "host:port for the local daemon")
	poll := flag.Duration("poll", orchestrator.DefaultPollInterval, "watcher poll cadence")
	printBoard := flag.Bool("print-board", false, "print the text board to stdout on every poll")
	flag.Parse()

	// The repo is optional: without it the board still serves and sessions
	// can be registered; PR polling stays disabled until a repo is given.
	logFile := openLogFile()
	if logFile != nil {
		defer logFile.Close()
		fmt.Fprintf(logFile, "orchestratord: starting (pid %d)\n", os.Getpid())
	}
	if *repo == "" {
		fmt.Println("orchestratord: no --repo given; serving the board without PR polling")
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	handle, err := orchestrator.Serve(ctx, orchestrator.ServeOptions{
		Repo:         *repo,
		RepoPath:     *repoPath,
		ListenAddr:   *listen,
		PollInterval: *poll,
		PrintBoard:   *printBoard,
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "orchestratord: %v\n", err)
		os.Exit(1)
	}

	fmt.Printf("orchestratord: fleet board on http://%s/board (live stream at /board/live)\n", handle.Addr)

	if err := <-handle.Done; err != nil && err != ctx.Err() {
		fmt.Fprintf(os.Stderr, "orchestratord: %v\n", err)
		if logFile != nil {
			fmt.Fprintf(logFile, "orchestratord: %v\n", err)
		}
		os.Exit(1)
	}
	fmt.Println("orchestratord: stopped")
}
