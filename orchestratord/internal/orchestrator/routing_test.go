package orchestrator

import (
	"errors"
	"testing"
	"time"
)

func receive(t *testing.T, inbox <-chan Event, timeout time.Duration) Event {
	t.Helper()
	select {
	case ev := <-inbox:
		return ev
	case <-time.After(timeout):
		t.Fatal("timed out waiting for routed event")
		return Event{}
	}
}

func assertNoEvent(t *testing.T, inbox <-chan Event) {
	t.Helper()
	select {
	case ev := <-inbox:
		t.Fatalf("unexpected event %q delivered", ev.Kind)
	case <-time.After(50 * time.Millisecond):
	}
}

func TestRouterRoutesFailureToOwningSessionOnly(t *testing.T) {
	router := NewRouter(2)
	inboxA := router.Register("metering")
	router.Register("chart")

	ev := Event{
		Kind:    EventCheckFailed,
		Session: "metering",
		PR:      322,
		Check:   Check{Name: "e2e (chrome)", Status: CheckFailed, LogURL: "https://ci/log/1"},
	}
	if err := router.Route(ev); err != nil {
		t.Fatalf("Route() error = %v, want nil", err)
	}

	got := receive(t, inboxA, time.Second)
	if got.Kind != EventCheckFailed || got.Session != "metering" || got.Check.Name != "e2e (chrome)" {
		t.Fatalf("routed event = %+v, want the failed check for metering", got)
	}
	assertNoEvent(t, router.Register("chart"))
}

func TestRouterUnknownSession(t *testing.T) {
	router := NewRouter(2)

	err := router.Route(Event{Kind: EventCheckFailed, Session: "ghost", PR: 1})
	if !errors.Is(err, ErrUnknownSession) {
		t.Fatalf("Route() error = %v, want ErrUnknownSession", err)
	}
}

func TestRouterFixBudgetEscalates(t *testing.T) {
	router := NewRouter(2)
	inbox := router.Register("metering")

	for cycle := 1; cycle <= 2; cycle++ {
		err := router.Route(Event{Kind: EventCheckFailed, Session: "metering", PR: 322})
		if err != nil {
			t.Fatalf("cycle %d: Route() error = %v, want nil", cycle, err)
		}
	}

	err := router.Route(Event{Kind: EventCheckFailed, Session: "metering", PR: 322})
	if !errors.Is(err, ErrFixBudgetExhausted) {
		t.Fatalf("third Route() error = %v, want ErrFixBudgetExhausted", err)
	}
	if got := len(inbox); got != 2 {
		t.Fatalf("inbox has %d events, want exactly the 2 delivered cycles", got)
	}
}

func TestRouterSuccessResetsFixBudget(t *testing.T) {
	router := NewRouter(1)
	inbox := router.Register("metering")

	if err := router.Route(Event{Kind: EventCheckFailed, Session: "metering"}); err != nil {
		t.Fatalf("first failure: Route() error = %v, want nil", err)
	}
	if err := router.Route(Event{Kind: EventCheckPassed, Session: "metering"}); err != nil {
		t.Fatalf("success: Route() error = %v, want nil", err)
	}
	if err := router.Route(Event{Kind: EventCheckFailed, Session: "metering"}); err != nil {
		t.Fatalf("failure after a passing cycle: Route() error = %v, want budget reset", err)
	}
	if got := len(inbox); got != 3 {
		t.Fatalf("inbox has %d events, want 3", got)
	}
}

func TestRouterQuestionDoesNotConsumeFixBudget(t *testing.T) {
	router := NewRouter(1)
	inbox := router.Register("invoice")

	if err := router.Route(Event{Kind: EventQuestion, Session: "invoice", Note: "Postmark or SES?"}); err != nil {
		t.Fatalf("question: Route() error = %v, want nil", err)
	}
	if err := router.Route(Event{Kind: EventCheckFailed, Session: "invoice"}); err != nil {
		t.Fatalf("failure after question: Route() error = %v, want nil", err)
	}
	if got := len(inbox); got != 2 {
		t.Fatalf("inbox has %d events, want 2", got)
	}
}
