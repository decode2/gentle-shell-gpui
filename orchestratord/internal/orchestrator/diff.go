package orchestrator

import (
	"fmt"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
)

// DiffFile is one changed file in a session's worktree.
type DiffFile struct {
	Path      string `json:"path"`
	Additions int    `json:"additions"`
	Deletions int    `json:"deletions"`
}

// DiffInfo is the session's worktree diff against its base branch:
// committed and working-tree changes, with a capped unified patch.
type DiffInfo struct {
	Files []DiffFile `json:"files"`
	Patch string     `json:"patch"`
}

// maxPatchBytes caps the inline patch so a huge worker change cannot drown
// the UI; the full patch lives in the worktree itself.
const maxPatchBytes = 256 * 1024

// worktreeDiff runs git inside the worktree: committed changes against base
// plus unstaged working-tree changes, merged per file.
func worktreeDiff(worktree, base string) (DiffInfo, error) {
	if worktree == "" {
		return DiffInfo{}, fmt.Errorf("session has no worktree")
	}
	if base == "" {
		base = "main"
	}

	committed, err := git(worktree, "diff", "--numstat", base+"...HEAD")
	if err != nil {
		return DiffInfo{}, fmt.Errorf("git diff (committed): %s", committed)
	}
	working, err := git(worktree, "diff", "--numstat")
	if err != nil {
		return DiffInfo{}, fmt.Errorf("git diff (working): %s", working)
	}

	files := mergeNumstat(parseNumstat(committed), parseNumstat(working))

	var patch strings.Builder
	if out, err := git(worktree, "diff", base+"...HEAD"); err == nil {
		patch.WriteString(out)
	}
	if out, err := git(worktree, "diff"); err == nil {
		patch.WriteString(out)
	}
	patchText := patch.String()
	if len(patchText) > maxPatchBytes {
		patchText = patchText[:maxPatchBytes] + "\n… (patch truncated; full diff in the worktree)"
	}

	return DiffInfo{Files: files, Patch: patchText}, nil
}

// parseNumstat reads git's "added<TAB>deleted<TAB>path" lines; binary and
// rename entries carry "-" counts and are skipped.
func parseNumstat(out string) []DiffFile {
	var files []DiffFile
	for _, line := range strings.Split(out, "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		parts := strings.Split(line, "\t")
		if len(parts) != 3 {
			continue
		}
		added, err1 := strconv.Atoi(parts[0])
		deleted, err2 := strconv.Atoi(parts[1])
		if err1 != nil || err2 != nil {
			continue
		}
		files = append(files, DiffFile{Path: parts[2], Additions: added, Deletions: deleted})
	}
	return files
}

// mergeNumstat sums per-path counts, committed first, working on top.
func mergeNumstat(lists ...[]DiffFile) []DiffFile {
	byPath := make(map[string]*DiffFile)
	var order []string
	for _, list := range lists {
		for _, f := range list {
			existing, ok := byPath[f.Path]
			if !ok {
				copy := f
				byPath[f.Path] = &copy
				order = append(order, f.Path)
				continue
			}
			existing.Additions += f.Additions
			existing.Deletions += f.Deletions
		}
	}
	files := make([]DiffFile, 0, len(order))
	for _, path := range order {
		files = append(files, *byPath[path])
	}
	return files
}

// terminalCommandForOS builds the command that opens the machine's terminal
// at dir; ok is false when no known terminal exists. Tests swap it.
var terminalCommandForOS = func(dir string) (*exec.Cmd, bool) {
	switch runtime.GOOS {
	case "windows":
		// Windows Terminal when present; cmd always exists and /K keeps it open.
		if _, err := exec.LookPath("wt.exe"); err == nil {
			return exec.Command("wt.exe", "-d", dir), true
		}
		return exec.Command("cmd", "/K", "cd", "/d", dir), true
	case "darwin":
		return exec.Command("open", "-a", "Terminal", dir), true
	default:
		for _, candidate := range []struct {
			binary string
			args   []string
		}{
			{"x-terminal-emulator", []string{"-e", "sh", "-c", "cd '" + dir + "' && exec $SHELL"}},
			{"gnome-terminal", []string{"--working-directory=" + dir}},
			{"konsole", []string{"--workdir", dir}},
			{"xterm", []string{"-e", "sh", "-c", "cd '" + dir + "' && exec $SHELL"}},
		} {
			if _, err := exec.LookPath(candidate.binary); err == nil {
				return exec.Command(candidate.binary, candidate.args...), true
			}
		}
		return nil, false
	}
}
