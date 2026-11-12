package orchestrator

import (
	"errors"
	"sort"
	"sync"
	"time"
)

var (
	// ErrUnknownSession is returned when an id is not registered in the fleet.
	ErrUnknownSession = errors.New("orchestrator: unknown session")
	// ErrDuplicateSession is returned when starting a session id twice.
	ErrDuplicateSession = errors.New("orchestrator: session already registered")
	// ErrDuplicateBranch is returned when two sessions would own one branch:
	// a pull request has exactly one owner, and ambiguous routing is worse
	// than a refused launch.
	ErrDuplicateBranch = errors.New("orchestrator: branch already owned by another session")
)

// Fleet is the in-memory registry of worker sessions. All methods are safe
// for concurrent use; lane membership is always re-derived from facts.
type Fleet struct {
	mu       sync.RWMutex
	sessions map[SessionID]*Session
}

// NewFleet returns an empty fleet.
func NewFleet() *Fleet {
	return &Fleet{sessions: make(map[SessionID]*Session)}
}

// Start registers a new worker session. Its facts start empty, so the
// session lands in the working lane until reality says otherwise.
func (f *Fleet) Start(s Session) error {
	f.mu.Lock()
	defer f.mu.Unlock()

	if _, ok := f.sessions[s.ID]; ok {
		return ErrDuplicateSession
	}
	for id, existing := range f.sessions {
		if existing.Branch == s.Branch && id != s.ID {
			return ErrDuplicateBranch
		}
	}
	s.Facts.UpdatedAt = time.Now()
	stored := s
	f.sessions[s.ID] = &stored
	return nil
}

// Get returns a copy of the session. The copy's facts are a snapshot.
func (f *Fleet) Get(id SessionID) (Session, error) {
	f.mu.RLock()
	defer f.mu.RUnlock()

	s, ok := f.sessions[id]
	if !ok {
		return Session{}, ErrUnknownSession
	}
	return *s, nil
}

// Update applies mut to the session's facts and returns the lane the facts
// now project onto.
func (f *Fleet) Update(id SessionID, mut func(*Facts)) (Lane, error) {
	f.mu.Lock()
	defer f.mu.Unlock()

	s, ok := f.sessions[id]
	if !ok {
		return "", ErrUnknownSession
	}
	mut(&s.Facts)
	s.Facts.UpdatedAt = time.Now()
	return DeriveLane(s.Facts), nil
}

// Lane derives the session's current lane from its facts.
func (f *Fleet) Lane(id SessionID) (Lane, error) {
	f.mu.RLock()
	defer f.mu.RUnlock()

	s, ok := f.sessions[id]
	if !ok {
		return "", ErrUnknownSession
	}
	return DeriveLane(s.Facts), nil
}

// SessionByBranch returns the id of the session that owns branch.
func (f *Fleet) SessionByBranch(branch string) (SessionID, bool) {
	f.mu.RLock()
	defer f.mu.RUnlock()

	for id, s := range f.sessions {
		if s.Branch == branch {
			return id, true
		}
	}
	return "", false
}

// ByLane groups registered session ids by their current lane. Lanes without
// sessions are present as empty slices, so a board can render all four lanes.
func (f *Fleet) ByLane() map[Lane][]SessionID {
	f.mu.RLock()
	defer f.mu.RUnlock()

	groups := make(map[Lane][]SessionID, 4)
	for _, lane := range []Lane{LaneWorking, LaneNeedsYou, LaneInReview, LaneReadyToMerge} {
		groups[lane] = nil
	}
	for id, s := range f.sessions {
		lane := DeriveLane(s.Facts)
		groups[lane] = append(groups[lane], id)
	}
	for lane := range groups {
		sort.Slice(groups[lane], func(i, j int) bool { return groups[lane][i] < groups[lane][j] })
	}
	return groups
}

// Count returns the number of registered sessions.
func (f *Fleet) Count() int {
	f.mu.RLock()
	defer f.mu.RUnlock()

	return len(f.sessions)
}
