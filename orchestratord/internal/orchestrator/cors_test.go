package orchestrator

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestDaemonCORSEchoesAllowedTauriOrigins(t *testing.T) {
	fleet := NewFleet()
	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	for _, origin := range corsAllowedOrigins {
		req, err := http.NewRequest(http.MethodOptions, srv.URL+"/sessions", nil)
		if err != nil {
			t.Fatalf("new request: %v", err)
		}
		req.Header.Set("Origin", origin)
		req.Header.Set("Access-Control-Request-Method", http.MethodPost)

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("preflight %s: %v", origin, err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusNoContent {
			t.Fatalf("preflight %s status = %d, want 204", origin, resp.StatusCode)
		}
		if got := resp.Header.Get("Access-Control-Allow-Origin"); got != origin {
			t.Fatalf("preflight %s ACAO = %q, want the echoed origin", origin, got)
		}
	}
}

func TestDaemonCORSWithholdsHeaderFromUnknownOrigins(t *testing.T) {
	fleet := NewFleet()
	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	req, err := http.NewRequest(http.MethodGet, srv.URL+"/board", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", "https://evil.example")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("GET /board: %v", err)
	}
	resp.Body.Close()
	if got := resp.Header.Get("Access-Control-Allow-Origin"); got != "" {
		t.Fatalf("ACAO = %q, want empty for an unknown origin", got)
	}
}

func TestDaemonCORSNonBrowserClientsNeedNoOrigin(t *testing.T) {
	fleet := NewFleet()
	if err := fleet.Start(Session{ID: "metering", Task: "t", Agent: "pi", Branch: "orch/metering"}); err != nil {
		t.Fatalf("Start(): %v", err)
	}
	srv := httptest.NewServer(NewDaemonHTTP(fleet))
	t.Cleanup(srv.Close)

	// No Origin header at all (curl, sidecar-local UI fetches).
	resp, err := http.Get(srv.URL + "/board")
	if err != nil {
		t.Fatalf("GET /board: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", resp.StatusCode)
	}
}
