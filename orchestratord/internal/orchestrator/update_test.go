package orchestrator

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestUpdateCheckComparesVersions(t *testing.T) {
	tests := []struct {
		name    string
		current string
		latest  string
		want    bool
	}{
		{"same version", "0.1.0", "0.1.0", false},
		{"older current", "0.1.0", "0.2.0", true},
		{"newer current", "0.2.0", "0.1.0", false},
		{"alpha tags", "0.1.0-alpha.1", "0.1.0-alpha.2", true},
		{"garbage latest", "0.1.0", "", false},
		{"garbage current", "", "0.1.0", true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := updateAvailable(tt.current, tt.latest); got != tt.want {
				t.Fatalf("updateAvailable(%q, %q) = %v, want %v", tt.current, tt.latest, got, tt.want)
			}
		})
	}
}

func TestApplyUpdateDownloadsAndSwapsSidecarAndApp(t *testing.T) {
	dir := t.TempDir()
	payload := "PE-fake-binary-content" // 22 bytes
	assetURL := "/orchestratord.exe"

	mux := http.NewServeMux()
	mux.HandleFunc(assetURL, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Length", "22")
		_, _ = w.Write([]byte(payload))
	})
	srv := httptest.NewServer(mux)

	store := &UpdateStore{AppDir: dir}
	current := dir + "/orchestratord.exe"
	_ = writeFile(current, []byte("old"))

	asset := UpdateAsset{Name: "orchestratord.exe", URL: srv.URL + assetURL, Size: 22}
	result, err := store.Apply(context.Background(), []UpdateAsset{asset})
	if err != nil {
		t.Fatalf("Apply(): %v", err)
	}

	// The new bytes are in place, staged through a temp file.
	got, err := readFile(current)
	if err != nil {
		t.Fatalf("read updated binary: %v", err)
	}
	if string(got) != payload {
		t.Fatalf("binary content = %q, want %q", got, payload)
	}
	if len(result) != 1 || result[0] != asset.Name {
		t.Fatalf("result = %+v, want the applied asset", result)
	}
	// No temp staging left behind.
	if strings.Contains(listDir(dir), ".staging-") {
		t.Fatalf("staging file left behind: %s", listDir(dir))
	}
}

func TestApplyUpdateRejectsSizeMismatch(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("short"))
	}))
	store := &UpdateStore{AppDir: t.TempDir()}

	asset := UpdateAsset{Name: "orchestratord.exe", URL: srv.URL, Size: 1000}
	if _, err := store.Apply(context.Background(), []UpdateAsset{asset}); err == nil {
		t.Fatal("Apply() succeeded with a size mismatch, want refusal")
	}
}

func TestApplyUpdateRefusesPathEscapes(t *testing.T) {
	store := &UpdateStore{AppDir: t.TempDir()}
	asset := UpdateAsset{Name: "../escape.exe", URL: "http://127.0.0.1:1/x", Size: 1}
	_, err := store.Apply(context.Background(), []UpdateAsset{asset})
	if err == nil || !strings.Contains(err.Error(), "asset name") {
		t.Fatalf("Apply() error = %v, want an asset-name refusal", err)
	}
}

func TestApplyUpdateSkipsUnknownAssets(t *testing.T) {
	store := &UpdateStore{AppDir: t.TempDir()}
	asset := UpdateAsset{Name: "source.tar.gz", URL: "http://127.0.0.1:1/x", Size: 1}
	result, err := store.Apply(context.Background(), []UpdateAsset{asset})
	if err != nil {
		t.Fatalf("Apply(): %v", err)
	}
	if len(result) != 0 {
		t.Fatalf("result = %+v, want nothing applied for a non-binary asset", result)
	}
}
