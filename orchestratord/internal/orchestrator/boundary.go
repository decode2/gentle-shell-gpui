package orchestrator

import (
	"net"
	"net/http"
	"strings"
)

// hostAllowed reports whether the request's Host header names this daemon's
// loopback surface. It is the DNS-rebinding defense: a page that rebound
// some domain to 127.0.0.1 arrives with that domain as Host and is refused;
// browsers and tools talking to 127.0.0.1/localhost (optionally with its
// port) pass.
func hostAllowed(host string) bool {
	name := host
	if h, _, err := net.SplitHostPort(host); err == nil {
		name = h
	}
	name = strings.Trim(name, "[]")
	switch name {
	case "127.0.0.1", "::1", "localhost":
		return true
	default:
		return false
	}
}

// originAllowed reports whether the request's Origin may open a websocket
// or stream: the desktop shell's origins, or no Origin at all (non-browser
// clients). Everything else — a malicious page's cross-site websocket — is
// refused.
func originAllowed(origin string) bool {
	if origin == "" {
		return true
	}
	for _, candidate := range corsAllowedOrigins {
		if origin == candidate {
			return true
		}
	}
	return false
}

// withBoundary enforces the daemon's local trust boundary on every request:
// loopback Host only, and browser origins only for streamed upgrades.
func withBoundary(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !hostAllowed(r.Host) {
			http.Error(w, "forbidden host", http.StatusForbidden)
			return
		}
		if !originAllowed(r.Header.Get("Origin")) {
			http.Error(w, "forbidden origin", http.StatusForbidden)
			return
		}
		next.ServeHTTP(w, r)
	})
}

// wsOriginPatterns lists the origin hosts coder/websocket may authorize for
// the terminal upgrade: the desktop shell's hosts (it matches the origin's
// host part, scheme included when the pattern carries one).
func wsOriginPatterns() []string {
	patterns := make([]string, 0, len(corsAllowedOrigins))
	for _, origin := range corsAllowedOrigins {
		rest := origin
		if idx := strings.Index(origin, "://"); idx >= 0 {
			rest = origin[idx+3:]
		}
		patterns = append(patterns, rest)
	}
	return patterns
}
