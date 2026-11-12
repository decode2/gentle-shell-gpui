package orchestrator

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

// UpdateAsset is one downloadable release file the updater knows how to
// install: the app binary or a sidecar.
type UpdateAsset struct {
	Name string `json:"name"`
	URL  string `json:"url"`
	Size int64  `json:"size"`
}

// UpdateStore downloads release assets into the app directory, staging each
// through a temp file and swapping atomically. It never touches files
// outside AppDir and refuses size-mismatched payloads.
type UpdateStore struct {
	// AppDir is the directory holding the app executable and sidecars.
	AppDir string
}

// updatableAssets are the file names Apply may replace, per running binary
// role: app shell and daemon, with the platform extensions.
func updatableAssets() map[string]string {
	names := map[string]string{"orchestratord": "orchestratord"}
	switch runtime.GOOS {
	case "windows":
		names["gentle-orchestrator"] = "GentleOrchestrator"
	default:
		names["gentle-orchestrator"] = "gentle-orchestrator"
	}
	return names
}

// Apply downloads every known binary asset (orchestratord, the app shell),
// staging and swapping each one. Unknown assets are skipped: a release may
// carry zips and sources the in-place updater cannot install. It returns the
// asset names it replaced.
func (s *UpdateStore) Apply(ctx context.Context, assets []UpdateAsset) ([]string, error) {
	known := updatableAssets()
	var applied []string
	for _, asset := range assets {
		if strings.Contains(asset.Name, "..") || strings.Contains(asset.Name, "/") || strings.Contains(asset.Name, "\\") {
			return applied, fmt.Errorf("refusing asset name %q", asset.Name)
		}
		base, ok := known[baseName(asset.Name)]
		if !ok {
			continue
		}
		if err := s.swapOne(ctx, asset, filepath.Join(s.AppDir, base+filepath.Ext(asset.Name))); err != nil {
			return applied, err
		}
		applied = append(applied, asset.Name)
	}
	return applied, nil
}

// baseName strips the release naming around the binary identity: from
// "orchestratord-x86_64-pc-windows-msvc.exe" to "orchestratord".
func baseName(name string) string {
	trimmed := filepath.Base(name)
	trimmed = strings.TrimSuffix(trimmed, filepath.Ext(trimmed))
	trimmed = strings.TrimSuffix(trimmed, ".exe")
	// A triple suffix follows the first dash-separated platform marker.
	if idx := strings.Index(trimmed, "-x86_64"); idx > 0 {
		trimmed = trimmed[:idx]
	}
	if idx := strings.Index(trimmed, "-aarch64"); idx > 0 {
		trimmed = trimmed[:idx]
	}
	return strings.ToLower(trimmed)
}

func (s *UpdateStore) swapOne(ctx context.Context, asset UpdateAsset, target string) error {
	if strings.Contains(asset.Name, "..") || strings.Contains(asset.Name, "/") || strings.Contains(asset.Name, "\\") || strings.ContainsAny(asset.Name, " ") {
		return fmt.Errorf("refusing asset name %q", asset.Name)
	}

	ctx, cancel := context.WithTimeout(ctx, 10*time.Minute)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, asset.URL, nil)
	if err != nil {
		return err
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("download %s: %w", asset.Name, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("download %s: status %d", asset.Name, resp.StatusCode)
	}

	staging, err := os.CreateTemp(s.AppDir, ".staging-update-*")
	if err != nil {
		return err
	}
	stagingPath := staging.Name()
	defer os.Remove(stagingPath) // no-op after a successful swap

	written, err := io.Copy(staging, resp.Body)
	if err != nil {
		staging.Close()
		return fmt.Errorf("download %s: %w", asset.Name, err)
	}
	if err := staging.Close(); err != nil {
		return err
	}
	if asset.Size > 0 && written != asset.Size {
		return fmt.Errorf("download %s: got %d bytes, want %d", asset.Name, written, asset.Size)
	}

	if err := os.Rename(stagingPath, target); err != nil {
		// Windows refuses to rename over a running binary; swap by rename
		// dance (old aside, new in, old deleted on next start).
		aside := target + ".old"
		_ = os.Remove(aside)
		if renameErr := os.Rename(target, aside); renameErr != nil {
			return fmt.Errorf("swap %s: %w (the app may be running)", target, renameErr)
		}
		if err := os.Rename(stagingPath, target); err != nil {
			_ = os.Rename(aside, target)
			return err
		}
		_ = os.Remove(aside)
	}
	return nil
}

// writeFile and readFile are tiny test helpers.
func writeFile(path string, content []byte) error {
	return os.WriteFile(path, content, 0o755)
}

func readFile(path string) ([]byte, error) {
	return os.ReadFile(path)
}

func listDir(dir string) string {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return ""
	}
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		names = append(names, e.Name())
	}
	return strings.Join(names, ",")
}

var errUpdateUnsupported = errors.New("orchestrator: update unavailable for this build")
