package orchestrator

import (
	"net/http"
	"strconv"
	"strings"
)

// Version is the daemon build identity; the update banner compares it with
// the latest published release tag.
const Version = "0.1.0-alpha.1"

// daemonVersion reports the running daemon's build.
func (d *DaemonHTTP) daemonVersion(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"version": Version})
}

// updateAvailable compares dotted numeric versions with optional pre-release
// suffixes: true when latest is strictly newer than current. Any unparsable
// side answers false — a broken tag never triggers an update; an empty
// current version updates whenever latest parses.
func updateAvailable(current, latest string) bool {
	cur, ok1 := parseVersion(current)
	upd, ok2 := parseVersion(latest)
	if !ok2 {
		return false
	}
	if !ok1 {
		return true
	}
	return upd.greater(cur)
}

type version struct {
	nums [3]int
	pre  int // -1: final release; otherwise the pre-release's numeric tail
}

// parseVersion reads "vMAJOR.MINOR.PATCH[-NAME.N]" — only the numeric tail
// of the pre-release orders alpha/beta tags against each other.
func parseVersion(tag string) (version, bool) {
	var v version
	tag = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(tag), "v"))
	if tag == "" {
		return v, false
	}

	v.pre = -1
	if idx := strings.Index(tag, "-"); idx >= 0 {
		pre := tag[idx+1:]
		tag = tag[:idx]
		digits, ok := trailingNumber(pre)
		if !ok {
			return v, false
		}
		v.pre = digits
	}
	if idx := strings.Index(tag, "+"); idx >= 0 {
		tag = tag[:idx]
	}

	parts := strings.Split(tag, ".")
	if len(parts) == 0 || len(parts) > 3 {
		return v, false
	}
	for i, part := range parts {
		n, err := strconv.Atoi(part)
		if err != nil || n < 0 {
			return v, false
		}
		v.nums[i] = n
	}
	return v, true
}

// trailingNumber extracts the number at the end of a pre-release tag:
// "alpha.2" → 2; a pre-release without a numeric tail sorts before any
// numbered sibling (returns 0, true).
func trailingNumber(pre string) (int, bool) {
	if pre == "" {
		return 0, false
	}
	end := len(pre)
	for end > 0 && pre[end-1] >= '0' && pre[end-1] <= '9' {
		end--
	}
	if end == len(pre) {
		return 0, true
	}
	n, err := strconv.Atoi(pre[end:])
	if err != nil {
		return 0, false
	}
	return n, true
}

func (v version) greater(other version) bool {
	for i := 2; i >= 0; i-- {
		if v.nums[i] != other.nums[i] {
			return v.nums[i] > other.nums[i]
		}
	}
	switch {
	case other.pre < 0 && v.pre >= 0:
		return false // a pre-release never outranks its release
	case v.pre < 0 && other.pre >= 0:
		return true
	default:
		return v.pre > other.pre
	}
}
