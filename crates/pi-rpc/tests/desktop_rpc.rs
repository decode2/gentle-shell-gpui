//! Wire-format and correlation tests for the Gentle Desktop RPC client
//! adapter (`gentle-shell.desktop-rpc/v1`). No process, network or UI: the
//! adapter only builds the outer prompt and decodes the widget records.

use pi_rpc::desktop_rpc::{
    ApplyProfileOutcome, ApplyProfileResult, ConfigChanged, ConfigErrorCode, ConfigLayer,
    ConfigLayerSource, ConfigScope, ConfigState, ConfigStatus, ConfigStore, ConfigWrite,
    ConfigWriteTarget, ConfigWriteTargetState, DesignCanvas, DesignCanvasEvent, DesignEntry,
    DesignTransition, DesktopAgent, DesktopAgentStep, DesktopAgentUsage, DesktopEvent, DesktopJob,
    DesktopMethod, DesktopRecord, DesktopRequest, DesktopResponse, DesktopRpcError, DesktopSession,
    Handoff, JobExitCode, MAX_REQUEST_BYTES, PROMPT_COMMAND, PermissionMode, PermissionModeChanged,
    PermissionModeState, PinSource, ProfileApplied, ProfileApplyErrorCode, ProfileApplyMode,
    ProfileConfirmation, ProfileFallbacksData, ProfileNote, ProfileNoteLevel, ProfilesData,
    ProviderUsage, RateLimitEvent, RateLimitSource, SCHEMA, SetConfigResult,
    SetPermissionModeResult, UsageLimit, UsageLimitScope, UsageRefresh, UsageResetAt, UsageStatus,
    UsageWindow, WIDGET_KEY, decode_payload, decode_widget,
};
use pi_rpc::{Response, UiMethod, UiRequest};
use serde_json::{Value, json};

/// One `extension_ui_request` carrying a single Desktop widget line.
fn widget(line: &str) -> UiRequest {
    widget_with_lines(Some(vec![line.to_string()]))
}

fn widget_with_lines(lines: Option<Vec<String>>) -> UiRequest {
    UiRequest {
        id: "ui-1".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: WIDGET_KEY.into(),
            lines,
            placement: None,
        },
    }
}

fn response_line(request_id: &str, data: Value) -> String {
    json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": request_id,
        "ok": true,
        "data": data,
    })
    .to_string()
}

fn error_line(request_id: &str, code: &str, message: &str) -> String {
    json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": request_id,
        "ok": false,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

/// A `capabilities` payload as the extension describes itself.
fn capabilities_data(methods: &[&str], events: &[&str]) -> Value {
    json!({
        "schema": SCHEMA,
        "command": PROMPT_COMMAND,
        "widget": WIDGET_KEY,
        "methods": methods,
        "events": events,
    })
}

fn prompt_response(disposition: &str) -> Response {
    Response {
        id: Some("gs-1".into()),
        command: "prompt".into(),
        success: true,
        data: json!({ "disposition": disposition }),
        error: None,
    }
}

fn event_line(seq: u64, event: &str, data: Value) -> String {
    json!({
        "schema": SCHEMA,
        "kind": "event",
        "seq": seq,
        "event": event,
        "data": data,
    })
    .to_string()
}

/// The `DesktopResponse` inside a decoded record.
fn record_response(record: DesktopRecord) -> DesktopResponse {
    match record {
        DesktopRecord::Response(response) => response,
        other => panic!("expected a response record, got {other:?}"),
    }
}

fn record_event(record: DesktopRecord) -> DesktopEvent {
    match record {
        DesktopRecord::Event(event) => event,
        other => panic!("expected an event record, got {other:?}"),
    }
}

// --- inner request wire format -------------------------------------------

#[test]
fn capabilities_request_has_the_exact_inner_and_outer_shape() {
    let request = DesktopRequest::capabilities("desktop-1").unwrap();

    let payload = format!(
        r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"capabilities","params":{{}}}}"#
    );
    assert_eq!(request.payload(), payload);
    assert_eq!(
        request.prompt_message(),
        format!("{PROMPT_COMMAND} {payload}")
    );

    // The outer command carries no `id`: the JSONL transport assigns the Pi
    // request id, and it is never the Desktop `requestId`.
    let command = request.prompt_command();
    assert_eq!(
        command,
        json!({ "type": "prompt", "message": format!("{PROMPT_COMMAND} {payload}") })
    );
    assert!(command.get("id").is_none());
}

#[test]
fn snapshot_request_uses_the_wire_method_name() {
    let request = DesktopRequest::get_snapshot("desktop-7").unwrap();
    assert_eq!(request.method(), DesktopMethod::GetSnapshot);
    assert_eq!(
        request.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-7","method":"get_snapshot","params":{{}}}}"#
        )
    );
    assert_eq!(
        DesktopMethod::parse("get_snapshot").unwrap(),
        DesktopMethod::GetSnapshot
    );
    assert_eq!(
        DesktopMethod::parse("capabilities").unwrap(),
        DesktopMethod::Capabilities
    );
}

#[test]
fn request_id_bounds_are_enforced() {
    assert!(matches!(
        DesktopRequest::capabilities(""),
        Err(DesktopRpcError::InvalidRequestId { .. })
    ));
    let longest = "r".repeat(128);
    assert!(DesktopRequest::capabilities(longest).is_ok());
    let too_long = "r".repeat(129);
    assert!(matches!(
        DesktopRequest::capabilities(too_long),
        Err(DesktopRpcError::InvalidRequestId { .. })
    ));
}

#[test]
fn oversized_requests_are_rejected_before_they_reach_the_transport() {
    let params = json!({ "blob": "x".repeat(MAX_REQUEST_BYTES) });
    assert!(matches!(
        DesktopRequest::new("desktop-1", DesktopMethod::Capabilities, params),
        Err(DesktopRpcError::RequestTooLarge { limit, .. }) if limit == MAX_REQUEST_BYTES
    ));
}

#[test]
fn methods_outside_the_supported_set_are_rejected() {
    assert!(matches!(
        DesktopMethod::parse("abort"),
        Err(DesktopRpcError::UnsupportedMethod(_))
    ));
}

// --- outer Pi response ----------------------------------------------------

#[test]
fn handled_means_accepted_and_never_a_domain_result() {
    let accepted = Response {
        id: Some("gs-3".into()),
        command: "prompt".into(),
        success: true,
        data: json!({ "disposition": "handled" }),
        error: None,
    };
    let handoff = Handoff::from_pi_response(&accepted).unwrap();
    assert_eq!(handoff.disposition, "handled");
    assert!(handoff.is_handled());

    // Only `handled` is acceptance: an intermediate disposition means the
    // domain answer is still coming, or never will.
    for disposition in ["started", "queued", "unknown"] {
        match Handoff::from_pi_response(&prompt_response(disposition)) {
            Err(DesktopRpcError::UnexpectedDisposition {
                disposition: Some(got),
            }) => assert_eq!(got, disposition),
            other => panic!("expected {disposition} to be refused, got {other:?}"),
        }
    }

    // A success without the disposition says nothing about delivery.
    let shapeless = Response {
        success: true,
        data: json!({}),
        ..accepted.clone()
    };
    assert!(matches!(
        Handoff::from_pi_response(&shapeless),
        Err(DesktopRpcError::UnexpectedDisposition { .. })
    ));

    let rejected = Response {
        success: false,
        error: Some("no such command".into()),
        ..accepted
    };
    assert!(matches!(
        Handoff::from_pi_response(&rejected),
        Err(DesktopRpcError::OuterFailed { .. })
    ));
}

// --- widget decoding -----------------------------------------------------

#[test]
fn a_successful_response_decodes_from_one_widget_line() {
    let ui = widget(&response_line(
        "desktop-1",
        capabilities_data(&["capabilities", "get_snapshot"], &["snapshot_changed"]),
    ));
    let response = record_response(decode_widget(&ui).unwrap().unwrap());
    assert_eq!(response.request_id(), Some("desktop-1"));
    assert!(response.is_ok());

    let capabilities = response.capabilities().unwrap();
    assert!(capabilities.supports(DesktopMethod::GetSnapshot));
    assert!(capabilities.supports(DesktopMethod::Capabilities));
}

#[test]
fn an_error_response_keeps_its_code_and_message() {
    let ui = widget(&error_line(
        "desktop-2",
        "unsupported_method",
        "no such method",
    ));
    let response = record_response(decode_widget(&ui).unwrap().unwrap());
    assert!(!response.is_ok());
    assert_eq!(response.error().unwrap().code, "unsupported_method");

    match response.snapshot() {
        Err(DesktopRpcError::RemoteError(info)) => {
            assert_eq!(info.code, "unsupported_method");
            assert_eq!(info.message, "no such method");
        }
        other => panic!("expected the remote error, got {other:?}"),
    }
}

#[test]
fn events_decode_with_their_sequence_and_data() {
    let ui = widget(&event_line(1, "snapshot_changed", json!({ "sequence": 1 })));
    let event = record_event(decode_widget(&ui).unwrap().unwrap());
    assert_eq!(event.seq, 1);
    assert_eq!(event.event, "snapshot_changed");
    assert_eq!(event.data["sequence"], 1);
}

#[test]
fn unrelated_ui_requests_are_not_desktop_records() {
    let other_widget = UiRequest {
        id: "ui-2".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: "gentle-agents".into(),
            lines: Some(vec!["[]".into()]),
            placement: None,
        },
    };
    assert!(decode_widget(&other_widget).unwrap().is_none());

    let notify = UiRequest {
        id: "ui-3".into(),
        timeout: None,
        method: UiMethod::Notify {
            message: "hi".into(),
            notify_type: None,
        },
    };
    assert!(decode_widget(&notify).unwrap().is_none());

    // The extension clearing its widget carries no record.
    assert!(decode_widget(&widget_with_lines(None)).unwrap().is_none());
    assert!(
        decode_widget(&widget_with_lines(Some(vec![])))
            .unwrap()
            .is_none()
    );

    // More than one line is not the documented shape.
    let two_lines = widget_with_lines(Some(vec![
        response_line("desktop-1", json!({})),
        event_line(1, "snapshot_changed", json!({})),
    ]));
    assert!(matches!(
        decode_widget(&two_lines),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

#[test]
fn foreign_schemas_kinds_and_malformed_payloads_are_rejected() {
    assert!(matches!(
        decode_payload(
            r#"{"schema":"other/v1","kind":"response","requestId":"a","ok":true,"data":{}}"#
        ),
        Err(DesktopRpcError::ForeignSchema(_))
    ));
    assert!(matches!(
        decode_payload(
            &json!({
                "schema": SCHEMA, "kind": "chunk", "requestId": "a", "ok": true, "data": {}
            })
            .to_string()
        ),
        Err(DesktopRpcError::UnexpectedKind(_))
    ));
    assert!(matches!(
        decode_payload("not json"),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
    assert!(matches!(
        decode_payload("[]"),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
    // `ok: true` promises data; `ok: false` promises an error.
    assert!(matches!(
        decode_payload(
            &json!({
                "schema": SCHEMA, "kind": "response", "requestId": "a", "ok": true
            })
            .to_string()
        ),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
    assert!(matches!(
        decode_payload(
            &json!({
                "schema": SCHEMA, "kind": "response", "requestId": "a", "ok": false
            })
            .to_string()
        ),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
    // An event needs a sequence and a name.
    assert!(matches!(
        decode_payload(
            &json!({
                "schema": SCHEMA, "kind": "event", "seq": 1, "data": {}
            })
            .to_string()
        ),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
    assert!(matches!(
        decode_payload(
            &json!({
                "schema": SCHEMA, "kind": "event", "event": "x", "data": {}
            })
            .to_string()
        ),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

#[test]
fn typed_decoding_rejects_data_that_does_not_fit() {
    let capabilities = record_response(
        decode_widget(&widget(&response_line("desktop-1", json!({}))))
            .unwrap()
            .unwrap(),
    );
    assert!(matches!(
        capabilities.capabilities(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));

    let snapshot = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "schema": SCHEMA, "sequence": 3, "state": {} }),
        )))
        .unwrap()
        .unwrap(),
    );
    let snapshot = snapshot.snapshot().unwrap();
    assert_eq!(snapshot.sequence, 3);
    assert_eq!(snapshot.state, json!({}));

    let bad_state = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "schema": SCHEMA, "sequence": 3, "state": [] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        bad_state.snapshot(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));

    let no_sequence = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "schema": SCHEMA, "state": {} }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        no_sequence.snapshot(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));

    // Asking for the wrong domain shape is a client mistake, not silence.
    let event_shape = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "methods": ["capabilities"] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        event_shape.snapshot(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

// --- session correlation -------------------------------------------------

#[test]
fn the_session_issues_one_request_at_a_time_with_its_own_ids() {
    let mut session = DesktopSession::new();
    let request = session.issue(DesktopMethod::Capabilities).unwrap();
    assert_eq!(request.request_id(), "desktop-1");
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::Capabilities))
    );

    // A second request before the first answer is ambiguous: refuse it.
    assert!(matches!(
        session.issue(DesktopMethod::GetSnapshot),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));

    let ui = widget(&response_line(
        "desktop-1",
        capabilities_data(&["capabilities", "get_snapshot"], &[]),
    ));
    session.accept(&ui).unwrap().unwrap();
    assert!(session.pending().is_none());

    let next = session.issue(DesktopMethod::GetSnapshot).unwrap();
    assert_eq!(next.request_id(), "desktop-2");
}

#[test]
fn responses_must_match_the_pending_request_id() {
    let mut session = DesktopSession::new();
    session.issue(DesktopMethod::Capabilities).unwrap();

    let foreign = widget(&response_line("desktop-99", capabilities_data(&[], &[])));
    match session.accept(&foreign) {
        Err(DesktopRpcError::UncorrelatedResponse { expected, got }) => {
            assert_eq!(expected.as_deref(), Some("desktop-1"));
            assert_eq!(got.as_deref(), Some("desktop-99"));
        }
        other => panic!("expected an uncorrelated response, got {other:?}"),
    }
    // The real answer still correlates.
    let real = widget(&response_line(
        "desktop-1",
        capabilities_data(&["capabilities"], &[]),
    ));
    assert!(session.accept(&real).unwrap().is_some());

    // Nothing is pending now.
    let late = widget(&response_line(
        "desktop-1",
        capabilities_data(&["capabilities"], &[]),
    ));
    assert!(matches!(
        session.accept(&late),
        Err(DesktopRpcError::UncorrelatedResponse { expected: None, .. })
    ));
}

#[test]
fn event_sequences_advance_one_at_a_time_without_gaps_or_repeats() {
    let mut session = DesktopSession::new();

    let first = widget(&event_line(1, "snapshot_changed", json!({ "sequence": 1 })));
    session.accept(&first).unwrap().unwrap();
    assert_eq!(session.last_sequence(), Some(1));

    let second = widget(&event_line(2, "snapshot_changed", json!({ "sequence": 2 })));
    session.accept(&second).unwrap().unwrap();

    // A duplicate is not a new event.
    match session.accept(&second) {
        Err(DesktopRpcError::DuplicateSequence { last, got }) => {
            assert_eq!((last, got), (2, 2));
        }
        other => panic!("expected a duplicate sequence, got {other:?}"),
    }

    // A gap means events were lost: refuse and keep the last good sequence.
    let skipped = widget(&event_line(5, "snapshot_changed", json!({})));
    match session.accept(&skipped) {
        Err(DesktopRpcError::OutOfOrderSequence { expected, got }) => {
            assert_eq!((expected, got), (3, 5));
        }
        other => panic!("expected an out-of-order sequence, got {other:?}"),
    }
    assert_eq!(session.last_sequence(), Some(2));

    let third = widget(&event_line(3, "snapshot_changed", json!({})));
    session.accept(&third).unwrap().unwrap();
    assert_eq!(session.last_sequence(), Some(3));
}

#[test]
fn the_session_ignores_unrelated_widgets_and_keeps_its_state() {
    let mut session = DesktopSession::new();
    session.issue(DesktopMethod::Capabilities).unwrap();

    let other = UiRequest {
        id: "ui-9".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: "gentle-agents".into(),
            lines: Some(vec!["[]".into()]),
            placement: None,
        },
    };
    assert!(session.accept(&other).unwrap().is_none());
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::Capabilities))
    );

    // A malformed desktop record is reported, and the pending request stays.
    let broken = widget("{not json");
    assert!(matches!(
        session.accept(&broken),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::Capabilities))
    );
}

#[test]
fn an_error_response_still_completes_the_pending_request() {
    let mut session = DesktopSession::new();
    session.issue(DesktopMethod::GetSnapshot).unwrap();
    let ui = widget(&error_line("desktop-1", "unavailable", "not ready"));
    let response = record_response(session.accept(&ui).unwrap().unwrap());
    assert_eq!(response.error().unwrap().code, "unavailable");
    assert!(session.pending().is_none());
    assert!(session.issue(DesktopMethod::GetSnapshot).is_ok());
}

// --- corrections: server-side correlation and recovery --------------------

#[test]
fn request_ids_are_measured_in_utf16_code_units() {
    // 64 non-BMP characters are 64 chars but the documented 128 code units.
    let at_limit = "\u{1f600}".repeat(64);
    assert_eq!(at_limit.chars().count(), 64);
    assert!(DesktopRequest::capabilities(at_limit).is_ok());

    assert!(matches!(
        DesktopRequest::capabilities("\u{1f600}".repeat(65)),
        Err(DesktopRpcError::InvalidRequestId { units: 130 })
    ));
}

#[test]
fn params_must_be_a_json_object() {
    for params in [json!([]), json!(""), json!(1), Value::Null] {
        assert!(matches!(
            DesktopRequest::new("desktop-1", DesktopMethod::Capabilities, params),
            Err(DesktopRpcError::ParamsNotObject)
        ));
    }
    assert!(DesktopRequest::new("desktop-1", DesktopMethod::GetSnapshot, json!({})).is_ok());
}

#[test]
fn the_request_size_limit_is_exact() {
    let prefix = format!(
        r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"capabilities","params":{{"blob":""#
    );
    let suffix = r#""}}"#;
    let room = MAX_REQUEST_BYTES - prefix.len() - suffix.len();

    let fits = DesktopRequest::new(
        "desktop-1",
        DesktopMethod::Capabilities,
        json!({ "blob": "x".repeat(room) }),
    )
    .unwrap();
    assert_eq!(fits.payload().len(), MAX_REQUEST_BYTES);

    assert!(matches!(
        DesktopRequest::new(
            "desktop-1",
            DesktopMethod::Capabilities,
            json!({ "blob": "x".repeat(room + 1) }),
        ),
        Err(DesktopRpcError::RequestTooLarge { bytes, limit })
            if bytes == MAX_REQUEST_BYTES + 1 && limit == MAX_REQUEST_BYTES
    ));
}

#[test]
fn a_null_request_id_error_keeps_the_code_and_releases_pending() {
    let mut session = DesktopSession::new();
    session.issue(DesktopMethod::Capabilities).unwrap();

    // The server documents `requestId: null` for requests it could not parse.
    let line = json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": null,
        "ok": false,
        "error": { "code": "invalid_request", "message": "missing method" },
    })
    .to_string();
    let response = record_response(session.accept(&widget(&line)).unwrap().unwrap());
    assert_eq!(response.request_id(), None);
    assert_eq!(response.error().unwrap().code, "invalid_request");
    assert_eq!(response.error().unwrap().message, "missing method");
    assert!(session.pending().is_none());

    // Without an id a success cannot be correlated, so it is refused.
    let orphan = json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": null,
        "ok": true,
        "data": {},
    })
    .to_string();
    assert!(matches!(
        session.accept(&widget(&orphan)),
        Err(DesktopRpcError::UncorrelatedResponse {
            expected: None,
            got: None,
        })
    ));

    // An idle session cannot own a null-id error either.
    let mut idle = DesktopSession::new();
    assert!(matches!(
        idle.accept(&widget(&line)),
        Err(DesktopRpcError::UncorrelatedResponse {
            expected: None,
            got: None,
        })
    ));

    assert!(session.issue(DesktopMethod::Capabilities).is_ok());
}

#[test]
fn abandoning_a_timed_out_request_frees_the_session() {
    let mut session = DesktopSession::new();
    session.issue(DesktopMethod::GetSnapshot).unwrap();

    assert_eq!(session.abandon().as_deref(), Some("desktop-1"));
    assert!(session.pending().is_none());
    assert!(session.abandon().is_none());

    // A late answer to the abandoned request is refused, not misapplied.
    let late = widget(&response_line(
        "desktop-1",
        json!({ "schema": SCHEMA, "sequence": 1, "state": {} }),
    ));
    assert!(matches!(
        session.accept(&late),
        Err(DesktopRpcError::UncorrelatedResponse { expected: None, got: Some(got) })
            if got == "desktop-1"
    ));

    assert_eq!(
        session
            .issue(DesktopMethod::GetSnapshot)
            .unwrap()
            .request_id(),
        "desktop-2"
    );
}

#[test]
fn reset_drops_the_in_flight_request_and_the_event_baseline() {
    let mut session = DesktopSession::new();
    session.issue(DesktopMethod::Capabilities).unwrap();
    session
        .accept(&widget(&event_line(1, "snapshot_changed", json!({}))))
        .unwrap();

    session.reset();
    assert!(session.pending().is_none());
    assert_eq!(session.last_sequence(), None);

    // A restarted backend starts a new sequence: anything is the new baseline.
    session
        .accept(&widget(&event_line(42, "snapshot_changed", json!({}))))
        .unwrap();
    assert_eq!(session.last_sequence(), Some(42));

    // Ids stay monotonic, so a late answer from the old backend cannot match.
    assert_eq!(
        session
            .issue(DesktopMethod::GetSnapshot)
            .unwrap()
            .request_id(),
        "desktop-2"
    );
}

#[test]
fn a_snapshot_resyncs_the_event_sequence_after_a_gap() {
    let mut session = DesktopSession::new();
    session
        .accept(&widget(&event_line(1, "snapshot_changed", json!({}))))
        .unwrap();

    let gap = widget(&event_line(4, "snapshot_changed", json!({})));
    assert!(matches!(
        session.accept(&gap),
        Err(DesktopRpcError::OutOfOrderSequence {
            expected: 2,
            got: 4
        })
    ));

    // The documented recovery: ask for a snapshot and adopt its sequence.
    session.issue(DesktopMethod::GetSnapshot).unwrap();
    let snapshot = record_response(
        session
            .accept(&widget(&response_line(
                "desktop-1",
                json!({ "schema": SCHEMA, "sequence": 3, "state": {} }),
            )))
            .unwrap()
            .unwrap(),
    )
    .snapshot()
    .unwrap();
    session.resync_from_snapshot(&snapshot).unwrap();
    assert_eq!(session.last_sequence(), Some(3));

    session
        .accept(&widget(&event_line(4, "snapshot_changed", json!({}))))
        .unwrap();
    assert_eq!(session.last_sequence(), Some(4));

    // A stale snapshot must not rewind the baseline into replays.
    assert!(matches!(
        session.resync_from_snapshot(&snapshot),
        Err(DesktopRpcError::StaleSnapshot {
            last: 4,
            sequence: 3,
        })
    ));
    assert_eq!(session.last_sequence(), Some(4));
}

#[test]
fn capabilities_retain_events_and_check_the_described_wire_values() {
    fn capabilities(data: Value) -> Result<pi_rpc::desktop_rpc::Capabilities, DesktopRpcError> {
        record_response(
            decode_widget(&widget(&response_line("desktop-1", data)))
                .unwrap()
                .unwrap(),
        )
        .capabilities()
    }

    let described = capabilities(capabilities_data(
        &["capabilities", "get_snapshot"],
        &["snapshot_changed", "status_changed"],
    ))
    .unwrap();
    assert_eq!(
        described.methods,
        vec!["capabilities".to_string(), "get_snapshot".to_string()]
    );
    assert_eq!(
        described.events,
        vec!["snapshot_changed".to_string(), "status_changed".to_string()]
    );
    assert_eq!(described.schema.as_deref(), Some(SCHEMA));
    assert_eq!(described.command.as_deref(), Some(PROMPT_COMMAND));
    assert_eq!(described.widget.as_deref(), Some(WIDGET_KEY));
    assert!(described.emits("snapshot_changed"));
    assert!(!described.emits("other"));

    let actual_server = capabilities(json!({
        "schema": SCHEMA,
        "command": "gentle:desktop",
        "widget": WIDGET_KEY,
        "methods": ["capabilities", "get_snapshot"],
        "events": [],
    }))
    .unwrap();
    assert_eq!(actual_server.command.as_deref(), Some("gentle:desktop"));

    for (data, field) in [
        (
            json!({ "methods": ["capabilities"], "events": [], "widget": "other" }),
            "widget",
        ),
        (
            json!({ "methods": ["capabilities"], "events": [], "command": "/other" }),
            "command",
        ),
        (
            json!({ "methods": ["capabilities"], "events": [], "schema": "other/v1" }),
            "schema",
        ),
    ] {
        match capabilities(data) {
            Err(DesktopRpcError::UnexpectedValue {
                field: got,
                expected,
                ..
            }) => {
                assert_eq!(got, field);
                assert!(!expected.is_empty());
            }
            other => panic!("expected {field} to be refused, got {other:?}"),
        }
    }

    // `events` is part of the contract, not an optional extra.
    assert!(matches!(
        capabilities(json!({ "methods": ["capabilities"] })),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

// --- permission mode additions --------------------------------------------

#[test]
fn get_permission_mode_request_uses_the_wire_method_name_and_empty_params() {
    let request = DesktopRequest::get_permission_mode("desktop-1").unwrap();
    assert_eq!(request.method(), DesktopMethod::GetPermissionMode);
    assert_eq!(
        request.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"get_permission_mode","params":{{}}}}"#
        )
    );
    assert_eq!(
        DesktopMethod::parse("get_permission_mode").unwrap(),
        DesktopMethod::GetPermissionMode
    );
    assert_eq!(
        "get_permission_mode".parse::<DesktopMethod>().unwrap(),
        DesktopMethod::GetPermissionMode
    );
    assert_eq!(
        "set_permission_mode".parse::<DesktopMethod>().unwrap(),
        DesktopMethod::SetPermissionMode
    );
}

#[test]
fn set_permission_mode_request_has_the_exact_payload_for_every_mode_and_persist() {
    for (mode, wire) in [
        (PermissionMode::Normal, "normal"),
        (PermissionMode::Auto, "auto"),
        (PermissionMode::Yolo, "yolo"),
        (PermissionMode::SuperYolo, "super-yolo"),
    ] {
        // `persist: None` omits the key, so the server's documented default
        // (`false`) applies.
        let omitted = DesktopRequest::set_permission_mode("desktop-1", mode, None).unwrap();
        assert_eq!(omitted.method(), DesktopMethod::SetPermissionMode);
        assert_eq!(
            omitted.payload(),
            format!(
                r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"set_permission_mode","params":{{"mode":"{wire}"}}}}"#
            )
        );

        let keep = DesktopRequest::set_permission_mode("desktop-2", mode, Some(false)).unwrap();
        assert_eq!(
            keep.payload(),
            format!(
                r#"{{"schema":"{SCHEMA}","requestId":"desktop-2","method":"set_permission_mode","params":{{"mode":"{wire}","persist":false}}}}"#
            )
        );

        let persist = DesktopRequest::set_permission_mode("desktop-3", mode, Some(true)).unwrap();
        assert_eq!(
            persist.payload(),
            format!(
                r#"{{"schema":"{SCHEMA}","requestId":"desktop-3","method":"set_permission_mode","params":{{"mode":"{wire}","persist":true}}}}"#
            )
        );
    }
}

#[test]
fn permission_mode_wire_values_round_trip_and_reject_unknowns() {
    for (mode, wire) in [
        (PermissionMode::Normal, "normal"),
        (PermissionMode::Auto, "auto"),
        (PermissionMode::Yolo, "yolo"),
        (PermissionMode::SuperYolo, "super-yolo"),
    ] {
        assert_eq!(mode.as_str(), wire);
        assert_eq!(mode.to_string(), wire);
        assert_eq!(wire.parse::<PermissionMode>().unwrap(), mode);
    }
    for bogus in ["turbo", "NORMAL", "super_yolo", ""] {
        assert!(matches!(
            bogus.parse::<PermissionMode>(),
            Err(DesktopRpcError::InvalidPermissionMode(_))
        ));
    }
}

#[test]
fn permission_mode_responses_decode_their_typed_state() {
    // `get_permission_mode` returns the live state: only mode and default_mode.
    let get = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "mode": "yolo", "default_mode": "normal" }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert_eq!(
        get.permission_mode().unwrap(),
        PermissionModeState {
            mode: PermissionMode::Yolo,
            default_mode: PermissionMode::Normal,
        }
    );

    // `set_permission_mode` returns the actual resulting state plus the two
    // attempt flags.
    let set = record_response(
        decode_widget(&widget(&response_line(
            "desktop-2",
            json!({ "mode": "auto", "default_mode": "auto", "persisted": true, "changed": true }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert_eq!(
        set.set_permission_mode().unwrap(),
        SetPermissionModeResult {
            mode: PermissionMode::Auto,
            default_mode: PermissionMode::Auto,
            persisted: true,
            changed: true,
            persist_error: None,
        }
    );

    // A failed save keeps the live change and reports the persist error.
    let failed_save = record_response(
        decode_widget(&widget(&response_line(
            "desktop-3",
            json!({
                "mode": "auto",
                "default_mode": "normal",
                "persisted": false,
                "changed": true,
                "persist_error": { "code": "default_unchanged", "message": "Default permission mode unchanged" },
            }),
        )))
        .unwrap()
        .unwrap(),
    );
    let result = failed_save.set_permission_mode().unwrap();
    assert!(!result.persisted);
    assert!(result.changed);
    let persist_error = result.persist_error.unwrap();
    assert_eq!(persist_error.code, "default_unchanged");
    assert_eq!(persist_error.message, "Default permission mode unchanged");
}

#[test]
fn permission_mode_changed_events_decode_their_typed_data() {
    let ui = widget(&event_line(
        1,
        "permission_mode_changed",
        json!({
            "mode": "auto",
            "previous_mode": "normal",
            "default_mode": "auto",
            "persisted": true,
            "source": "desktop",
        }),
    ));
    let event = record_event(decode_widget(&ui).unwrap().unwrap());
    assert_eq!(event.event, "permission_mode_changed");
    assert_eq!(
        event.permission_mode_changed().unwrap(),
        PermissionModeChanged {
            mode: PermissionMode::Auto,
            previous_mode: PermissionMode::Normal,
            default_mode: PermissionMode::Auto,
            persisted: true,
            source: "desktop".to_string(),
        }
    );
}

#[test]
fn permission_mode_decoding_rejects_unknown_modes_and_malformed_shapes() {
    let response_cases: &[Value] = &[
        json!({ "mode": "turbo", "default_mode": "normal" }),
        json!({ "mode": "auto", "default_mode": "turbo" }),
        json!({ "mode": 7, "default_mode": "normal" }),
        json!({ "mode": "auto" }),
        json!({ "mode": "auto", "default_mode": "normal", "persisted": false }),
        json!({ "mode": "auto", "default_mode": "normal", "persisted": false, "changed": "yes" }),
        json!({
            "mode": "auto", "default_mode": "normal", "persisted": false, "changed": false,
            "persist_error": "nope",
        }),
    ];
    for data in response_cases {
        let response = record_response(
            decode_widget(&widget(&response_line("desktop-1", data.clone())))
                .unwrap()
                .unwrap(),
        );
        assert!(
            response.set_permission_mode().is_err(),
            "{data} should not decode as a set_permission_mode result"
        );
    }

    // The read accessor applies the same mode and shape validation.
    let bad_read = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "mode": "auto" }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(bad_read.permission_mode().is_err());

    let event_cases: &[Value] = &[
        json!({ "mode": "turbo", "previous_mode": "normal", "default_mode": "normal", "persisted": false, "source": "desktop" }),
        json!({ "mode": "auto", "previous_mode": "normal", "default_mode": "auto", "persisted": false }),
        json!({ "mode": "auto", "previous_mode": "normal", "default_mode": "auto", "persisted": false, "source": 7 }),
    ];
    for data in event_cases {
        let event = record_event(
            decode_widget(&widget(&event_line(
                1,
                "permission_mode_changed",
                data.clone(),
            )))
            .unwrap()
            .unwrap(),
        );
        assert!(
            event.permission_mode_changed().is_err(),
            "{data} should not decode as a permission_mode_changed event"
        );
    }
}

#[test]
fn permission_mode_decoding_ignores_unknown_extra_fields() {
    // The existing decoders read the fields they know and ignore the rest, so
    // an evolving server stays compatible with this client.
    let set = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({
                "mode": "auto",
                "default_mode": "normal",
                "persisted": false,
                "changed": true,
                "extra": { "anything": true },
            }),
        )))
        .unwrap()
        .unwrap(),
    );
    let result = set.set_permission_mode().unwrap();
    assert_eq!(result.mode, PermissionMode::Auto);
    assert!(result.changed);
}

#[test]
fn capabilities_expose_the_permission_methods_and_event() {
    let response = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(
                &[
                    "capabilities",
                    "get_snapshot",
                    "get_permission_mode",
                    "set_permission_mode",
                ],
                &["permission_mode_changed"],
            ),
        )))
        .unwrap()
        .unwrap(),
    );
    let capabilities = response.capabilities().unwrap();
    assert!(capabilities.supports(DesktopMethod::GetPermissionMode));
    assert!(capabilities.supports(DesktopMethod::SetPermissionMode));
    assert!(capabilities.emits("permission_mode_changed"));
    assert!(!capabilities.emits("config_changed"));
}

#[test]
fn the_session_issues_permission_requests_with_typed_params() {
    let mut session = DesktopSession::new();

    let read = session.issue(DesktopMethod::GetPermissionMode).unwrap();
    assert_eq!(read.request_id(), "desktop-1");
    assert_eq!(
        read.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"get_permission_mode","params":{{}}}}"#
        )
    );

    // Only one request may be in flight.
    assert!(matches!(
        session.issue_set_permission_mode(PermissionMode::Auto, Some(true)),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));

    // Complete the read, then the typed set goes out with the exact params.
    session
        .accept(&widget(&response_line(
            "desktop-1",
            json!({ "mode": "normal", "default_mode": "normal" }),
        )))
        .unwrap();

    let set = session
        .issue_set_permission_mode(PermissionMode::SuperYolo, Some(true))
        .unwrap();
    assert_eq!(set.request_id(), "desktop-2");
    assert_eq!(set.method(), DesktopMethod::SetPermissionMode);
    assert_eq!(
        set.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-2","method":"set_permission_mode","params":{{"mode":"super-yolo","persist":true}}}}"#
        )
    );
    assert_eq!(
        session.pending(),
        Some(("desktop-2", DesktopMethod::SetPermissionMode))
    );
}

// --- subagent and job methods --------------------------------------------

fn agent_data() -> Value {
    json!({
        "id": "m3a1-worker",
        "agent": "worker",
        "status": "completed",
        "model": "anthropic/claude-sonnet-4",
        "modelSource": "binding",
        "fallbackFrom": "anthropic/claude-opus-4",
        "thinking": "high",
        "lastStep": "answered",
        "usage": {
            "total": 15234,
            "cost": 0.0421,
            "input": 12000,
            "output": 3234,
            "cache": 5000,
            "reasoning": 4000,
        },
    })
}

fn full_agent() -> DesktopAgent {
    DesktopAgent {
        id: "m3a1-worker".into(),
        agent: "worker".into(),
        status: "completed".into(),
        model: Some("anthropic/claude-sonnet-4".into()),
        model_source: Some("binding".into()),
        fallback_from: Some("anthropic/claude-opus-4".into()),
        thinking: Some("high".into()),
        last_step: DesktopAgentStep::Answered,
        usage: DesktopAgentUsage {
            total: 15234,
            cost: 0.0421,
            input: Some(12000),
            output: Some(3234),
            cache: Some(5000),
            reasoning: Some(4000),
        },
    }
}

fn job_data() -> Value {
    json!({
        "id": "job-2",
        "label": "Deploy staging",
        "status": "exited",
        "startedAt": 1788599000000u64,
        "endedAt": 1788599500000u64,
        "exitCode": 0,
    })
}

fn single_agent(data: Value) -> DesktopAgent {
    let list = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "agents": [data] }),
        )))
        .unwrap()
        .unwrap(),
    )
    .list_subagents()
    .unwrap();
    assert_eq!(list.agents.len(), 1);
    list.agents.into_iter().next().unwrap()
}

fn single_job(data: Value) -> DesktopJob {
    let list = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "jobs": [data] }),
        )))
        .unwrap()
        .unwrap(),
    )
    .list_jobs()
    .unwrap();
    assert_eq!(list.jobs.len(), 1);
    list.jobs.into_iter().next().unwrap()
}

#[test]
fn subagent_and_job_methods_use_the_exact_wire_names_and_shapes() {
    let list_agents = DesktopRequest::list_subagents("desktop-1").unwrap();
    assert_eq!(list_agents.method(), DesktopMethod::ListSubagents);
    assert_eq!(
        list_agents.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"list_subagents","params":{{}}}}"#
        )
    );

    let list_jobs = DesktopRequest::list_jobs("desktop-2").unwrap();
    assert_eq!(list_jobs.method(), DesktopMethod::ListJobs);
    assert_eq!(
        list_jobs.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-2","method":"list_jobs","params":{{}}}}"#
        )
    );

    let cancel_agent = DesktopRequest::cancel_subagent("desktop-3", "m3a1-worker").unwrap();
    assert_eq!(cancel_agent.method(), DesktopMethod::CancelSubagent);
    assert_eq!(
        cancel_agent.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-3","method":"cancel_subagent","params":{{"id":"m3a1-worker"}}}}"#
        )
    );

    let cancel_job = DesktopRequest::cancel_job("desktop-4", "job-1").unwrap();
    assert_eq!(cancel_job.method(), DesktopMethod::CancelJob);
    assert_eq!(
        cancel_job.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-4","method":"cancel_job","params":{{"id":"job-1"}}}}"#
        )
    );

    for (wire, method) in [
        ("list_subagents", DesktopMethod::ListSubagents),
        ("cancel_subagent", DesktopMethod::CancelSubagent),
        ("list_jobs", DesktopMethod::ListJobs),
        ("cancel_job", DesktopMethod::CancelJob),
    ] {
        assert_eq!(DesktopMethod::parse(wire).unwrap(), method);
        assert_eq!(wire.parse::<DesktopMethod>().unwrap(), method);
        assert_eq!(method.as_str(), wire);
    }
}

#[test]
fn subagent_and_job_methods_are_advertised_and_checked_by_capabilities() {
    let response = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(
                &[
                    "capabilities",
                    "list_subagents",
                    "cancel_subagent",
                    "list_jobs",
                    "cancel_job",
                ],
                &["agents_update", "jobs_update"],
            ),
        )))
        .unwrap()
        .unwrap(),
    );
    let capabilities = response.capabilities().unwrap();
    assert!(capabilities.supports(DesktopMethod::ListSubagents));
    assert!(capabilities.supports(DesktopMethod::CancelSubagent));
    assert!(capabilities.supports(DesktopMethod::ListJobs));
    assert!(capabilities.supports(DesktopMethod::CancelJob));
    assert!(capabilities.emits("agents_update"));
    assert!(capabilities.emits("jobs_update"));
}

#[test]
fn cancel_params_reject_an_empty_id_before_anything_is_sent() {
    assert!(matches!(
        DesktopRequest::cancel_subagent("desktop-1", ""),
        Err(DesktopRpcError::InvalidCancelId)
    ));
    assert!(matches!(
        DesktopRequest::cancel_job("desktop-1", ""),
        Err(DesktopRpcError::InvalidCancelId)
    ));

    let mut session = DesktopSession::new();
    assert!(matches!(
        session.issue_cancel_subagent(""),
        Err(DesktopRpcError::InvalidCancelId)
    ));
    assert!(matches!(
        session.issue_cancel_job(""),
        Err(DesktopRpcError::InvalidCancelId)
    ));
    // A rejected request never consumes an id or leaves one pending.
    assert!(session.pending().is_none());
    assert_eq!(
        session.issue_list_subagents().unwrap().request_id(),
        "desktop-1"
    );
}

#[test]
fn agent_entries_decode_model_usage_cost_and_breakdown() {
    let agent = single_agent(agent_data());
    assert_eq!(agent, full_agent());

    // The optional launch-context and breakdown fields are absent rather than
    // invented when the owner did not record them.
    let minimal = single_agent(json!({
        "id": "a1",
        "agent": "explore",
        "status": "running",
        "model": "anthropic/claude-sonnet-4",
        "lastStep": "writing",
        "usage": { "total": 100, "cost": 0.0 },
    }));
    assert_eq!(minimal.model_source, None);
    assert_eq!(minimal.fallback_from, None);
    assert_eq!(minimal.thinking, None);
    assert_eq!(minimal.usage.input, None);
    assert_eq!(minimal.usage.output, None);
    assert_eq!(minimal.usage.cache, None);
    assert_eq!(minimal.usage.reasoning, None);

    // `model` is optional on this projection: an entry without one decodes.
    let no_model = single_agent(json!({
        "id": "a2",
        "agent": "explore",
        "status": "queued",
        "lastStep": "queued",
        "usage": { "total": 0, "cost": 0.0 },
    }));
    assert_eq!(no_model.model, None);
}

#[test]
fn agent_last_step_is_a_closed_vocabulary() {
    for step in DesktopAgentStep::ALL {
        assert_eq!(step.as_str().parse::<DesktopAgentStep>().unwrap(), step);
        assert_eq!(step.to_string(), step.as_str());

        let agent = single_agent(json!({
            "id": "a1",
            "agent": "explore",
            "status": "running",
            "model": "anthropic/claude-sonnet-4",
            "lastStep": step.as_str(),
            "usage": { "total": 1, "cost": 0.0 },
        }));
        assert_eq!(agent.last_step, step);
    }

    // Raw owner step text (an error, a path, a provider body) never crosses:
    // it is not in the vocabulary, so it is refused, not forwarded.
    for bogus in ["responded", "error: timeout", "reading lib/auth.ts", ""] {
        let response = record_response(
            decode_widget(&widget(&response_line(
                "desktop-1",
                json!({ "agents": [{
                    "id": "a1",
                    "agent": "explore",
                    "status": "running",
                    "model": "anthropic/claude-sonnet-4",
                    "lastStep": bogus,
                    "usage": { "total": 1, "cost": 0.0 },
                }] }),
            )))
            .unwrap()
            .unwrap(),
        );
        assert!(matches!(
            response.list_subagents(),
            Err(DesktopRpcError::InvalidLastStep(_))
        ));
    }
}

#[test]
fn job_entries_decode_timestamps_and_preserve_null_vs_absent_exit_code() {
    assert_eq!(
        single_job(job_data()),
        DesktopJob {
            id: "job-2".into(),
            label: "Deploy staging".into(),
            status: "exited".into(),
            started_at: 1788599000000,
            ended_at: Some(1788599500000),
            exit_code: Some(JobExitCode::Code(0)),
        }
    );

    // A running job has no end and no recorded exit code: both are absent.
    let running = single_job(json!({
        "id": "job-1",
        "label": "CI for PR 42",
        "status": "running",
        "startedAt": 1788600000000u64,
    }));
    assert_eq!(running.ended_at, None);
    assert_eq!(running.exit_code, None);

    // A job that ended without a code records `null`, which is not absence.
    let no_code = single_job(json!({
        "id": "job-3",
        "label": "killed",
        "status": "failed",
        "startedAt": 1788600000000u64,
        "endedAt": 1788601000000u64,
        "exitCode": null,
    }));
    assert_eq!(no_code.exit_code, Some(JobExitCode::Null));
}

#[test]
fn list_responses_decode_their_typed_lists() {
    let agents = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "agents": [agent_data(), agent_data()] }),
        )))
        .unwrap()
        .unwrap(),
    )
    .list_subagents()
    .unwrap();
    assert_eq!(agents.agents, vec![full_agent(), full_agent()]);

    let jobs = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "jobs": [job_data()] }),
        )))
        .unwrap()
        .unwrap(),
    )
    .list_jobs()
    .unwrap();
    assert_eq!(jobs.jobs.len(), 1);
    assert_eq!(jobs.jobs[0].id, "job-2");
}

#[test]
fn cancel_responses_carry_requested_and_changed_only_when_supplied() {
    // The current server answers the post-action projection alone; the flags
    // are future-facing and must not be invented as defaults.
    let plain = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "agent": agent_data() }),
        )))
        .unwrap()
        .unwrap(),
    )
    .cancel_subagent()
    .unwrap();
    assert_eq!(plain.agent, full_agent());
    assert_eq!(plain.requested, None);
    assert_eq!(plain.changed, None);

    // A synchronous cancel (queued -> cancelled) reports both flags.
    let queued = json!({
        "id": "m3a1-worker",
        "agent": "worker",
        "status": "cancelled",
        "model": "anthropic/claude-sonnet-4",
        "lastStep": "cancelled",
        "usage": { "total": 15234, "cost": 0.0421 },
    });
    let sync = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "agent": queued, "requested": true, "changed": true }),
        )))
        .unwrap()
        .unwrap(),
    )
    .cancel_subagent()
    .unwrap();
    assert_eq!(sync.requested, Some(true));
    assert_eq!(sync.changed, Some(true));
    assert_eq!(sync.agent.status, "cancelled");

    // A running task is cancelled asynchronously: requested but not changed.
    // The projection must keep both, never default `changed` to false.
    let async_cancel = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "agent": agent_data(), "requested": true }),
        )))
        .unwrap()
        .unwrap(),
    )
    .cancel_subagent()
    .unwrap();
    assert_eq!(async_cancel.requested, Some(true));
    assert_eq!(async_cancel.changed, None);

    // Jobs report only `changed`.
    let job = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "job": job_data() }),
        )))
        .unwrap()
        .unwrap(),
    )
    .cancel_job()
    .unwrap();
    assert_eq!(job.job.id, "job-2");
    assert_eq!(job.changed, None);

    let changed_job = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "job": job_data(), "changed": true }),
        )))
        .unwrap()
        .unwrap(),
    )
    .cancel_job()
    .unwrap();
    assert_eq!(changed_job.changed, Some(true));
}

#[test]
fn agents_update_and_jobs_update_verify_the_exact_event_name() {
    let agents = record_event(
        decode_widget(&widget(&event_line(
            1,
            "agents_update",
            json!({ "agents": [agent_data()] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert_eq!(agents.agents_update().unwrap().agents, vec![full_agent()]);

    let jobs = record_event(
        decode_widget(&widget(&event_line(
            2,
            "jobs_update",
            json!({ "jobs": [job_data()] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert_eq!(jobs.jobs_update().unwrap().jobs.len(), 1);

    // The exact event name is checked first: a foreign event that happens to
    // fit the shape is refused, not misread.
    let foreign_name = record_event(
        decode_widget(&widget(&event_line(
            3,
            "permission_mode_changed",
            json!({ "agents": [agent_data()] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        foreign_name.agents_update(),
        Err(DesktopRpcError::UnexpectedEvent { expected: "agents_update", got })
            if got == "permission_mode_changed"
    ));

    // The right name with the wrong shape is a shape error, and the other
    // domain's shape is refused too.
    let wrong_shape = record_event(
        decode_widget(&widget(&event_line(
            4,
            "agents_update",
            json!({ "jobs": [job_data()] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        wrong_shape.agents_update(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));

    let wrong_shape_jobs = record_event(
        decode_widget(&widget(&event_line(
            5,
            "jobs_update",
            json!({ "agents": [agent_data()] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        wrong_shape_jobs.jobs_update(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

#[test]
fn malformed_agent_and_job_payloads_are_rejected() {
    let agent_cases: &[Value] = &[
        json!({ "agent": "explore", "status": "running", "model": "m", "lastStep": "running", "usage": { "total": 1, "cost": 0.0 } }),
        json!({ "id": "", "agent": "explore", "status": "running", "model": "m", "lastStep": "running", "usage": { "total": 1, "cost": 0.0 } }),
        json!({ "id": "a1", "agent": "explore", "status": "running", "model": "m", "usage": { "total": 1, "cost": 0.0 } }),
        json!({ "id": "a1", "agent": "explore", "status": "running", "model": "m", "lastStep": "running" }),
        json!({ "id": "a1", "agent": "explore", "status": "running", "model": "m", "lastStep": "running", "usage": { "cost": 0.0 } }),
        json!({ "id": "a1", "agent": "explore", "status": "running", "model": "m", "lastStep": "running", "usage": { "total": -1, "cost": 0.0 } }),
        json!({ "id": "a1", "agent": "explore", "status": "running", "model": "m", "lastStep": "running", "usage": { "total": 1, "cost": "cheap" } }),
        json!({ "id": "a1", "agent": "explore", "status": "running", "model": 7, "lastStep": "running", "usage": { "total": 1, "cost": 0.0 } }),
    ];
    for data in agent_cases {
        let response = record_response(
            decode_widget(&widget(&response_line(
                "desktop-1",
                json!({ "agents": [data.clone()] }),
            )))
            .unwrap()
            .unwrap(),
        );
        assert!(
            response.list_subagents().is_err(),
            "{data} should not decode as a subagent list"
        );
    }

    let job_cases: &[Value] = &[
        json!({ "label": "x", "status": "running", "startedAt": 1 }),
        json!({ "id": "j1", "status": "running", "startedAt": 1 }),
        json!({ "id": "j1", "label": "x", "startedAt": 1 }),
        json!({ "id": "j1", "label": "x", "status": "running" }),
        json!({ "id": "j1", "label": "x", "status": "running", "startedAt": -1 }),
        json!({ "id": "j1", "label": "x", "status": "running", "startedAt": 1, "endedAt": "soon" }),
        json!({ "id": "j1", "label": "x", "status": "exited", "startedAt": 1, "exitCode": 1.5 }),
        json!({ "id": "j1", "label": "x", "status": "exited", "startedAt": 1, "exitCode": "0" }),
    ];
    for data in job_cases {
        let response = record_response(
            decode_widget(&widget(&response_line(
                "desktop-1",
                json!({ "jobs": [data.clone()] }),
            )))
            .unwrap()
            .unwrap(),
        );
        assert!(
            response.list_jobs().is_err(),
            "{data} should not decode as a job list"
        );
    }
}

#[test]
fn malformed_cancel_payloads_are_rejected() {
    for data in [
        json!({}),
        json!({ "requested": true }),
        json!({ "agent": "nope" }),
        json!({ "agent": agent_data(), "requested": "yes" }),
        json!({ "agent": agent_data(), "changed": 1 }),
    ] {
        let response = record_response(
            decode_widget(&widget(&response_line("desktop-1", data.clone())))
                .unwrap()
                .unwrap(),
        );
        assert!(
            response.cancel_subagent().is_err(),
            "{data} should not decode as a cancel_subagent result"
        );
    }

    for data in [
        json!({}),
        json!({ "job": "nope" }),
        json!({ "job": job_data(), "changed": "yes" }),
    ] {
        let response = record_response(
            decode_widget(&widget(&response_line("desktop-1", data.clone())))
                .unwrap()
                .unwrap(),
        );
        assert!(
            response.cancel_job().is_err(),
            "{data} should not decode as a cancel_job result"
        );
    }
}

#[test]
fn subagent_and_job_requests_flow_through_session_correlation() {
    let mut session = DesktopSession::new();

    let list = session.issue_list_subagents().unwrap();
    assert_eq!(list.request_id(), "desktop-1");
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::ListSubagents))
    );

    // One request in flight, whatever the method.
    assert!(matches!(
        session.issue_list_jobs(),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));
    assert!(matches!(
        session.issue_cancel_job("job-1"),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));

    session
        .accept(&widget(&response_line(
            "desktop-1",
            json!({ "agents": [agent_data()] }),
        )))
        .unwrap();
    assert!(session.pending().is_none());

    let cancel = session.issue_cancel_job("job-1").unwrap();
    assert_eq!(cancel.request_id(), "desktop-2");
    assert_eq!(cancel.method(), DesktopMethod::CancelJob);
    assert_eq!(
        session.pending(),
        Some(("desktop-2", DesktopMethod::CancelJob))
    );
}

#[test]
fn subagent_and_job_events_keep_the_sequence_rules() {
    let mut session = DesktopSession::new();

    // The first event of the new names sets the baseline like any other.
    session
        .accept(&widget(&event_line(
            7,
            "jobs_update",
            json!({ "jobs": [job_data()] }),
        )))
        .unwrap();
    assert_eq!(session.last_sequence(), Some(7));

    session
        .accept(&widget(&event_line(
            8,
            "agents_update",
            json!({ "agents": [agent_data()] }),
        )))
        .unwrap();
    assert_eq!(session.last_sequence(), Some(8));

    // Gaps are still gaps, and the last good sequence is preserved.
    let skipped = widget(&event_line(12, "jobs_update", json!({ "jobs": [] })));
    assert!(matches!(
        session.accept(&skipped),
        Err(DesktopRpcError::OutOfOrderSequence {
            expected: 9,
            got: 12
        })
    ));
    assert_eq!(session.last_sequence(), Some(8));
}

// --- subscription usage methods and event -------------------------------

fn usage_provider_data() -> Value {
    json!({
        "provider": "codex",
        "status": "ok",
        "fetchedAt": 1788600000000u64,
        "plan": "pro",
        "refresh": "unsupported",
        "limits": [
            {
                "name": "prompts",
                "limitReached": false,
                "scope": "provider",
                "windows": [
                    {
                        "label": "5h/week",
                        "usedPercent": 42.5,
                        "windowSeconds": 18000,
                        "resetAt": 1789200000000u64,
                        "used": 76.5,
                        "budget": 180.0,
                    },
                    {
                        "label": "48h/month",
                        "usedPercent": 0.0,
                        "windowSeconds": 172800,
                        "resetAt": null,
                    },
                ],
            }
        ],
    })
}

fn full_provider() -> ProviderUsage {
    ProviderUsage {
        provider: "codex".into(),
        status: UsageStatus::Ok,
        fetched_at: Some(1788600000000.0),
        plan: Some("pro".into()),
        refresh: Some(UsageRefresh::Unsupported),
        limits: vec![UsageLimit {
            name: "prompts".into(),
            limit_reached: false,
            scope: Some(UsageLimitScope::Provider),
            windows: vec![
                UsageWindow {
                    label: "5h/week".into(),
                    used_percent: 42.5,
                    window_seconds: 18000.0,
                    reset_at: UsageResetAt::At(1789200000000.0),
                    used: Some(76.5),
                    budget: Some(180.0),
                },
                UsageWindow {
                    label: "48h/month".into(),
                    used_percent: 0.0,
                    window_seconds: 172800.0,
                    reset_at: UsageResetAt::Null,
                    used: None,
                    budget: None,
                },
            ],
        }],
    }
}

fn minimal_usage_provider(status: &str) -> Value {
    json!({ "provider": "codex", "status": status, "limits": [] })
}

fn single_usage_provider(data: Value) -> ProviderUsage {
    let response = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "providers": [data] }),
        )))
        .unwrap()
        .unwrap(),
    );
    let usage = response.subscription_usage().unwrap();
    assert_eq!(usage.providers.len(), 1);
    usage.providers.into_iter().next().unwrap()
}

#[test]
fn subscription_usage_methods_use_the_exact_wire_names_and_shapes() {
    for (method, wire) in [
        (
            DesktopMethod::GetSubscriptionUsage,
            "get_subscription_usage",
        ),
        (
            DesktopMethod::RefreshSubscriptionUsage,
            "refresh_subscription_usage",
        ),
    ] {
        assert_eq!(method.as_str(), wire);
        assert_eq!(DesktopMethod::parse(wire).unwrap(), method);
        assert_eq!(wire.parse::<DesktopMethod>().unwrap(), method);
    }

    let get = DesktopRequest::get_subscription_usage("desktop-1", None).unwrap();
    assert_eq!(get.method(), DesktopMethod::GetSubscriptionUsage);
    assert_eq!(
        get.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"get_subscription_usage","params":{{}}}}"#
        )
    );

    let get_for = DesktopRequest::get_subscription_usage("desktop-2", Some("nan")).unwrap();
    assert_eq!(
        get_for.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-2","method":"get_subscription_usage","params":{{"provider":"nan"}}}}"#
        )
    );

    let refresh = DesktopRequest::refresh_subscription_usage("desktop-3", None).unwrap();
    assert_eq!(
        refresh.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-3","method":"refresh_subscription_usage","params":{{}}}}"#
        )
    );

    let refresh_for =
        DesktopRequest::refresh_subscription_usage("desktop-4", Some("claude")).unwrap();
    assert_eq!(
        refresh_for.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-4","method":"refresh_subscription_usage","params":{{"provider":"claude"}}}}"#
        )
    );
}

#[test]
fn subscription_usage_rejects_an_empty_provider_before_anything_is_sent() {
    assert!(matches!(
        DesktopRequest::get_subscription_usage("desktop-1", Some("")),
        Err(DesktopRpcError::InvalidProvider)
    ));
    assert!(matches!(
        DesktopRequest::refresh_subscription_usage("desktop-1", Some("")),
        Err(DesktopRpcError::InvalidProvider)
    ));

    let mut session = DesktopSession::new();
    assert!(matches!(
        session.issue_get_subscription_usage(Some("")),
        Err(DesktopRpcError::InvalidProvider)
    ));
    assert!(matches!(
        session.issue_refresh_subscription_usage(Some("")),
        Err(DesktopRpcError::InvalidProvider)
    ));
    // A rejected request never consumes an id or leaves one pending.
    assert!(session.pending().is_none());
    assert_eq!(
        session
            .issue_get_subscription_usage(None)
            .unwrap()
            .request_id(),
        "desktop-1"
    );
}

#[test]
fn subscription_usage_decodes_providers_limits_and_windows() {
    assert_eq!(
        single_usage_provider(usage_provider_data()),
        full_provider()
    );

    // `fetch_at`, `plan`, `refresh`, `scope`, `used` and `budget` are optional
    // and decode to None rather than being invented.
    let minimal = single_usage_provider(minimal_usage_provider("pending"));
    assert_eq!(minimal.provider, "codex");
    assert_eq!(minimal.status, UsageStatus::Pending);
    assert_eq!(minimal.fetched_at, None);
    assert_eq!(minimal.plan, None);
    assert_eq!(minimal.refresh, None);
    assert!(minimal.limits.is_empty());
}

#[test]
fn subscription_usage_limits_default_to_empty_when_absent() {
    // A pending, failed or unavailable provider may omit `limits` entirely:
    // the typed accessor projects an empty list, never an error or invented
    // usage. Only a present non-array `limits` stays malformed.
    for status in ["pending", "failed", "unavailable"] {
        let provider = single_usage_provider(json!({ "provider": "codex", "status": status }));
        assert_eq!(provider.status.as_str(), status);
        assert!(provider.limits.is_empty());
    }
}

#[test]
fn subscription_usage_decodes_every_status_and_refresh_disposition() {
    for status in UsageStatus::ALL {
        assert_eq!(status.as_str(), status.to_string());
        assert_eq!(status.as_str().parse::<UsageStatus>().unwrap(), status);
        let provider = single_usage_provider(minimal_usage_provider(status.as_str()));
        assert_eq!(provider.status, status);
    }

    for refresh in UsageRefresh::ALL {
        assert_eq!(refresh.as_str(), refresh.to_string());
        assert_eq!(refresh.as_str().parse::<UsageRefresh>().unwrap(), refresh);
        let data = json!({
            "provider": "codex",
            "status": "ok",
            "refresh": refresh.as_str(),
            "limits": [],
        });
        assert_eq!(single_usage_provider(data).refresh, Some(refresh));
    }

    // `refresh` is optional: its absence is not a default disposition.
    assert_eq!(
        single_usage_provider(minimal_usage_provider("ok")).refresh,
        None
    );

    assert!(matches!(
        "turbo".parse::<UsageStatus>(),
        Err(DesktopRpcError::InvalidUsageStatus(_))
    ));
    assert!(matches!(
        "later".parse::<UsageRefresh>(),
        Err(DesktopRpcError::InvalidUsageRefresh(_))
    ));
    assert!(matches!(
        "account".parse::<UsageLimitScope>(),
        Err(DesktopRpcError::InvalidUsageScope(_))
    ));
    assert!(matches!(
        "push".parse::<RateLimitSource>(),
        Err(DesktopRpcError::InvalidRateLimitSource(_))
    ));
}

#[test]
fn usage_reset_at_is_distinct_from_absent() {
    let window = |data: Value| {
        let provider = single_usage_provider(json!({
            "provider": "x",
            "status": "ok",
            "limits": [{
                "name": "n",
                "limitReached": false,
                "windows": [data],
            }],
        }));
        provider.limits[0].windows[0].clone()
    };

    assert_eq!(
        window(json!({
            "label": "w", "usedPercent": 1.0, "windowSeconds": 60, "resetAt": null
        }))
        .reset_at,
        UsageResetAt::Null
    );
    assert_eq!(
        window(json!({
            "label": "w", "usedPercent": 1.0, "windowSeconds": 60, "resetAt": 1789200000000u64
        }))
        .reset_at,
        UsageResetAt::At(1789200000000.0)
    );

    // `resetAt` is required: absent is malformed, never conflated with null.
    let absent = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "providers": [{
                "provider": "x",
                "status": "ok",
                "limits": [{
                    "name": "n",
                    "limitReached": false,
                    "windows": [{ "label": "w", "usedPercent": 1.0, "windowSeconds": 60 }],
                }],
            }] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        absent.subscription_usage(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

#[test]
fn rate_limit_event_decodes_source_and_snapshots() {
    let ui = widget(&event_line(
        1,
        "rate_limit",
        json!({
            "source": "refresh",
            "snapshots": [{ "provider": "claude", "status": "failed", "limits": [] }],
        }),
    ));
    let event = record_event(decode_widget(&ui).unwrap().unwrap());
    assert_eq!(event.event, "rate_limit");
    assert_eq!(
        event.rate_limit().unwrap(),
        RateLimitEvent {
            source: RateLimitSource::Refresh,
            snapshots: vec![ProviderUsage {
                provider: "claude".into(),
                status: UsageStatus::Failed,
                fetched_at: None,
                plan: None,
                refresh: None,
                limits: vec![],
            }],
        }
    );

    let response = record_event(
        decode_widget(&widget(&event_line(
            2,
            "rate_limit",
            json!({
                "source": "response",
                "snapshots": [usage_provider_data()],
            }),
        )))
        .unwrap()
        .unwrap(),
    )
    .rate_limit()
    .unwrap();
    assert_eq!(response.source, RateLimitSource::Response);
    assert_eq!(response.snapshots, vec![full_provider()]);
}

#[test]
fn rate_limit_verifies_the_exact_event_name_and_rejects_malformed_data() {
    // The exact event name is checked first: a foreign event that happens to
    // fit the shape is refused, not misread.
    let foreign = record_event(
        decode_widget(&widget(&event_line(
            1,
            "agents_update",
            json!({ "source": "refresh", "snapshots": [] }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        foreign.rate_limit(),
        Err(DesktopRpcError::UnexpectedEvent { expected: "rate_limit", got })
            if got == "agents_update"
    ));

    let cases: &[Value] = &[
        json!({ "snapshots": [] }),                          // missing source
        json!({ "source": "refresh" }),                      // missing snapshots
        json!({ "source": "push", "snapshots": [] }),        // unknown source
        json!({ "source": 7, "snapshots": [] }),             // source not a string
        json!({ "source": "refresh", "snapshots": "nope" }), // snapshots not an array
        // Snapshot entries use the provider shape; a non-array `limits` stays
        // malformed even though an absent one defaults to empty.
        json!({ "source": "refresh", "snapshots": [{ "provider": "x", "status": "ok", "limits": "nope" }] }),
        json!({ "source": "refresh", "snapshots": [{
            "provider": "x", "status": "ok",
            "limits": [{ "name": "n", "limitReached": false }],
        }] }),
    ];
    for data in cases {
        let event = record_event(
            decode_widget(&widget(&event_line(1, "rate_limit", data.clone())))
                .unwrap()
                .unwrap(),
        );
        assert!(
            event.rate_limit().is_err(),
            "{data} should not decode as a rate_limit event"
        );
    }
}

#[test]
fn malformed_subscription_usage_payloads_are_rejected() {
    let provider_cases: &[Value] = &[
        json!({ "status": "ok", "limits": [] }),
        json!({ "provider": "", "status": "ok", "limits": [] }),
        json!({ "provider": "x" }),
        json!({ "provider": "x", "status": "turbo", "limits": [] }),
        json!({ "provider": "x", "status": "ok", "limits": "nope" }),
        json!({ "provider": "x", "status": "ok", "limits": [{}] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "", "limitReached": false, "windows": [] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n" }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": "yes", "windows": [] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": "nope" }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{}] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{ "label": "", "usedPercent": 1.0, "windowSeconds": 60, "resetAt": null }] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{ "label": "w", "usedPercent": "full", "windowSeconds": 60, "resetAt": null }] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{ "label": "w", "usedPercent": 1.0, "windowSeconds": "an hour", "resetAt": null }] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{ "label": "w", "usedPercent": 1.0, "windowSeconds": 60 }] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{ "label": "w", "usedPercent": 1.0, "windowSeconds": 60, "resetAt": "soon" }] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{ "label": "w", "usedPercent": 1.0, "windowSeconds": 60, "resetAt": null, "used": "many" }] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "windows": [{ "label": "w", "usedPercent": 1.0, "windowSeconds": 60, "resetAt": null, "budget": [] }] }] }),
        json!({ "provider": "x", "status": "ok", "limits": [{ "name": "n", "limitReached": false, "scope": "account", "windows": [] }] }),
        json!({ "provider": "x", "status": "ok", "refresh": "later", "limits": [] }),
        json!({ "provider": "x", "status": "ok", "fetchedAt": "yesterday", "limits": [] }),
        json!({ "provider": "x", "status": "ok", "plan": 7, "limits": [] }),
    ];
    for data in provider_cases {
        let response = record_response(
            decode_widget(&widget(&response_line(
                "desktop-1",
                json!({ "providers": [data.clone()] }),
            )))
            .unwrap()
            .unwrap(),
        );
        assert!(
            response.subscription_usage().is_err(),
            "{data} should not decode as subscription usage"
        );
    }

    // The response shape requires the `providers` array.
    for data in [
        json!({}),
        json!({ "providers": "nope" }),
        json!({ "providers": [7] }),
    ] {
        let response = record_response(
            decode_widget(&widget(&response_line("desktop-1", data.clone())))
                .unwrap()
                .unwrap(),
        );
        assert!(
            response.subscription_usage().is_err(),
            "{data} should not decode as subscription usage"
        );
    }
}

#[test]
fn subscription_usage_rejects_non_finite_numbers() {
    // serde_json refuses a number that overflows to infinity, so a non-finite
    // `usedPercent` fails the whole record closed rather than decoding usage.
    let payload = format!(
        r#"{{"schema":"{SCHEMA}","kind":"response","requestId":"desktop-1","ok":true,"data":{{"providers":[{{"provider":"codex","status":"ok","limits":[{{"name":"prompts","limitReached":false,"windows":[{{"label":"5h","usedPercent":1e400,"windowSeconds":18000,"resetAt":null}}]}}]}}]}}}}"#
    );
    assert!(matches!(
        decode_payload(&payload),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

#[test]
fn subscription_usage_ignores_unknown_extra_fields() {
    // The projection carries no credential, account or URL fields; an evolving
    // server may add them and this client stays compatible by ignoring them.
    let provider = single_usage_provider(json!({
        "provider": "codex",
        "status": "ok",
        "limits": [],
        "account": "acct_123",
        "url": "https://example.invalid",
        "apiKey": "secret",
    }));
    assert_eq!(provider.provider, "codex");
    assert_eq!(provider.status, UsageStatus::Ok);
    assert!(provider.limits.is_empty());
}

#[test]
fn subscription_usage_requests_flow_through_session_correlation() {
    let mut session = DesktopSession::new();

    let get = session.issue_get_subscription_usage(None).unwrap();
    assert_eq!(get.request_id(), "desktop-1");
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::GetSubscriptionUsage))
    );

    // One request in flight, whatever the method.
    assert!(matches!(
        session.issue_refresh_subscription_usage(None),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));

    session
        .accept(&widget(&response_line(
            "desktop-1",
            json!({ "providers": [] }),
        )))
        .unwrap();
    assert!(session.pending().is_none());

    let refresh = session
        .issue_refresh_subscription_usage(Some("claude"))
        .unwrap();
    assert_eq!(refresh.request_id(), "desktop-2");
    assert_eq!(refresh.method(), DesktopMethod::RefreshSubscriptionUsage);
    assert_eq!(
        refresh.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-2","method":"refresh_subscription_usage","params":{{"provider":"claude"}}}}"#
        )
    );
}

#[test]
fn capabilities_expose_subscription_usage_methods_and_event() {
    let response = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(
                &[
                    "capabilities",
                    "get_subscription_usage",
                    "refresh_subscription_usage",
                ],
                &["rate_limit"],
            ),
        )))
        .unwrap()
        .unwrap(),
    );
    let capabilities = response.capabilities().unwrap();
    assert!(capabilities.supports(DesktopMethod::GetSubscriptionUsage));
    assert!(capabilities.supports(DesktopMethod::RefreshSubscriptionUsage));
    assert!(capabilities.emits("rate_limit"));
    assert!(!capabilities.emits("usage_changed"));
}

// --- design canvas method and event -------------------------------------

fn design_entry() -> DesignEntry {
    DesignEntry {
        slug: "hero-onboarding".into(),
        title: "Hero onboarding".into(),
        mtime: 1788600000000.0,
    }
}

fn design_entry_data() -> Value {
    json!({
        "slug": "hero-onboarding",
        "title": "Hero onboarding",
        "mtime": 1788600000000u64,
    })
}

fn design_canvas_event(
    seq: u64,
    transition: &str,
    mut state: Value,
    url: Option<&str>,
) -> DesktopEvent {
    state["transition"] = json!(transition);
    if let Some(url) = url {
        state["url"] = json!(url);
    }
    record_event(
        decode_widget(&widget(&event_line(seq, "design_canvas", state)))
            .unwrap()
            .unwrap(),
    )
}

#[test]
fn get_design_canvas_uses_the_exact_wire_name_and_params() {
    assert_eq!(DesktopMethod::GetDesignCanvas.as_str(), "get_design_canvas");
    assert_eq!(
        DesktopMethod::parse("get_design_canvas").unwrap(),
        DesktopMethod::GetDesignCanvas
    );
    assert_eq!(
        "get_design_canvas".parse::<DesktopMethod>().unwrap(),
        DesktopMethod::GetDesignCanvas
    );

    // `includeUrl: None` omits the key, so the server's documented default
    // (no URL) applies.
    let omitted = DesktopRequest::get_design_canvas("desktop-1", None).unwrap();
    assert_eq!(omitted.method(), DesktopMethod::GetDesignCanvas);
    assert_eq!(
        omitted.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"get_design_canvas","params":{{}}}}"#
        )
    );

    let with_url = DesktopRequest::get_design_canvas("desktop-2", Some(true)).unwrap();
    assert_eq!(
        with_url.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-2","method":"get_design_canvas","params":{{"includeUrl":true}}}}"#
        )
    );

    // `false` is accepted and sent explicitly, never defaulted to omission.
    let without_url = DesktopRequest::get_design_canvas("desktop-3", Some(false)).unwrap();
    assert_eq!(
        without_url.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-3","method":"get_design_canvas","params":{{"includeUrl":false}}}}"#
        )
    );

    // The shared zero-param issue path also fits this method with `{}`.
    let mut session = DesktopSession::new();
    assert_eq!(
        session
            .issue(DesktopMethod::GetDesignCanvas)
            .unwrap()
            .payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-1","method":"get_design_canvas","params":{{}}}}"#
        )
    );
}

#[test]
fn design_canvas_response_decodes_running_and_stopped_states() {
    let running = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({
                "running": true,
                "slug": "hero-onboarding",
                "connectedClients": 3,
                "designs": [design_entry_data()],
                "url": "http://127.0.0.1:3210/?token=abc",
            }),
        )))
        .unwrap()
        .unwrap(),
    )
    .design_canvas()
    .unwrap();
    assert_eq!(
        running,
        DesignCanvas {
            running: true,
            slug: Some("hero-onboarding".into()),
            connected_clients: 3,
            designs: vec![design_entry()],
            url: Some("http://127.0.0.1:3210/?token=abc".into()),
        }
    );

    // A stopped canvas reports no slug, no clients, no designs and no URL.
    let stopped = record_response(
        decode_widget(&widget(&response_line(
            "desktop-2",
            json!({
                "running": false,
                "slug": null,
                "connectedClients": 0,
                "designs": [],
            }),
        )))
        .unwrap()
        .unwrap(),
    )
    .design_canvas()
    .unwrap();
    assert_eq!(
        stopped,
        DesignCanvas {
            running: false,
            slug: None,
            connected_clients: 0,
            designs: vec![],
            url: None,
        }
    );
}

#[test]
fn design_entries_decode_slug_title_and_fractional_mtime() {
    // `mtime` is a finite non-negative number, not required to be an integer.
    let response = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({
                "running": true,
                "slug": null,
                "connectedClients": 0,
                "designs": [
                    { "slug": "a", "title": "A", "mtime": 1788600000000.5 },
                    { "slug": "b", "title": "", "mtime": 0 },
                ],
            }),
        )))
        .unwrap()
        .unwrap(),
    )
    .design_canvas()
    .unwrap();
    assert_eq!(response.designs[0].mtime, 1788600000000.5);
    assert_eq!(
        response.designs[1],
        DesignEntry {
            slug: "b".into(),
            title: "".into(),
            mtime: 0.0,
        }
    );
}

#[test]
fn design_canvas_events_decode_open_refresh_and_stop_transitions() {
    for transition in DesignTransition::ALL {
        assert_eq!(transition.to_string(), transition.as_str());
        assert_eq!(
            transition.as_str().parse::<DesignTransition>().unwrap(),
            transition
        );
    }
    assert!(matches!(
        "pause".parse::<DesignTransition>(),
        Err(DesktopRpcError::InvalidDesignTransition(_))
    ));

    let running = json!({
        "running": true,
        "slug": "hero-onboarding",
        "connectedClients": 1,
        "designs": [design_entry_data()],
    });

    let open = design_canvas_event(
        1,
        "open",
        running.clone(),
        Some("http://127.0.0.1:3210/?token=abc"),
    );
    assert_eq!(
        open.design_canvas().unwrap(),
        DesignCanvasEvent {
            transition: DesignTransition::Open,
            state: DesignCanvas {
                running: true,
                slug: Some("hero-onboarding".into()),
                connected_clients: 1,
                designs: vec![design_entry()],
                url: Some("http://127.0.0.1:3210/?token=abc".into()),
            },
        }
    );

    let refresh = design_canvas_event(2, "refresh", running.clone(), None);
    let refreshed = refresh.design_canvas().unwrap();
    assert_eq!(refreshed.transition, DesignTransition::Refresh);
    assert_eq!(refreshed.state.url, None);

    // `stop` carries the stopped state and no URL.
    let stopped = json!({
        "running": false,
        "slug": null,
        "connectedClients": 0,
        "designs": [],
    });
    let stop = design_canvas_event(3, "stop", stopped, None);
    assert_eq!(
        stop.design_canvas().unwrap(),
        DesignCanvasEvent {
            transition: DesignTransition::Stop,
            state: DesignCanvas {
                running: false,
                slug: None,
                connected_clients: 0,
                designs: vec![],
                url: None,
            },
        }
    );
}

#[test]
fn design_canvas_url_is_optional_and_never_a_separate_token() {
    // No `url` field -> None, matching the default and stopped states.
    let without = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({ "running": false, "slug": null, "connectedClients": 0, "designs": [] }),
        )))
        .unwrap()
        .unwrap(),
    )
    .design_canvas()
    .unwrap();
    assert_eq!(without.url, None);

    // A token lives inside the URL string; there is no separate `token` field
    // and this projection never reads one.
    let with_url = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({
                "running": true,
                "slug": "a",
                "connectedClients": 1,
                "designs": [],
                "url": "http://127.0.0.1:3210/#t=abc",
                "token": "should-be-ignored",
            }),
        )))
        .unwrap()
        .unwrap(),
    )
    .design_canvas()
    .unwrap();
    assert_eq!(
        with_url.url.as_deref(),
        Some("http://127.0.0.1:3210/#t=abc")
    );

    // An explicit `url: null` decodes like absence.
    let null_url = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            json!({
                "running": true,
                "slug": "a",
                "connectedClients": 1,
                "designs": [],
                "url": null,
            }),
        )))
        .unwrap()
        .unwrap(),
    )
    .design_canvas()
    .unwrap();
    assert_eq!(null_url.url, None);
}

#[test]
fn design_canvas_rejects_malformed_shapes_and_non_finite_values() {
    let state_cases: &[Value] = &[
        json!({ "slug": null, "connectedClients": 0, "designs": [] }),
        json!({ "running": "yes", "slug": null, "connectedClients": 0, "designs": [] }),
        json!({ "running": true, "slug": 7, "connectedClients": 0, "designs": [] }),
        json!({ "running": true, "slug": null, "designs": [] }),
        json!({ "running": true, "slug": null, "connectedClients": -1, "designs": [] }),
        json!({ "running": true, "slug": null, "connectedClients": 1.5, "designs": [] }),
        json!({ "running": true, "slug": null, "connectedClients": "one", "designs": [] }),
        json!({ "running": true, "slug": null, "connectedClients": 0 }),
        json!({ "running": true, "slug": null, "connectedClients": 0, "designs": "nope" }),
        json!({ "running": true, "slug": null, "connectedClients": 0, "designs": [7] }),
    ];
    for data in state_cases {
        let response = record_response(
            decode_widget(&widget(&response_line("desktop-1", data.clone())))
                .unwrap()
                .unwrap(),
        );
        assert!(
            response.design_canvas().is_err(),
            "{data} should not decode as design canvas state"
        );
    }

    let entry_cases: &[Value] = &[
        json!({ "title": "x", "mtime": 1 }),
        json!({ "slug": "", "title": "x", "mtime": 1 }),
        json!({ "slug": "s", "title": "x" }),
        json!({ "slug": "s", "title": "x", "mtime": "soon" }),
        json!({ "slug": "s", "title": "x", "mtime": -1 }),
        json!({ "slug": "s", "title": 7, "mtime": 1 }),
        json!({ "slug": 7, "title": "x", "mtime": 1 }),
    ];
    for data in entry_cases {
        let response = record_response(
            decode_widget(&widget(&response_line(
                "desktop-1",
                json!({
                    "running": true,
                    "slug": null,
                    "connectedClients": 0,
                    "designs": [data.clone()],
                }),
            )))
            .unwrap()
            .unwrap(),
        );
        assert!(
            response.design_canvas().is_err(),
            "{data} should not decode as a design entry"
        );
    }

    // A non-finite `mtime` overflows JSON parsing and fails the whole record
    // closed rather than decoding an entry.
    let payload = format!(
        r#"{{"schema":"{SCHEMA}","kind":"response","requestId":"desktop-1","ok":true,"data":{{"running":true,"slug":null,"connectedClients":0,"designs":[{{"slug":"s","title":"x","mtime":1e400}}]}}}}"#
    );
    assert!(matches!(
        decode_payload(&payload),
        Err(DesktopRpcError::MalformedPayload(_))
    ));

    let event_cases: &[Value] = &[
        json!({ "running": true, "slug": null, "connectedClients": 0, "designs": [] }),
        json!({ "transition": "pause", "running": true, "slug": null, "connectedClients": 0, "designs": [] }),
        json!({ "transition": 7, "running": true, "slug": null, "connectedClients": 0, "designs": [] }),
        json!({ "transition": "open", "slug": null, "connectedClients": 0, "designs": [] }),
    ];
    for data in event_cases {
        let event = record_event(
            decode_widget(&widget(&event_line(1, "design_canvas", data.clone())))
                .unwrap()
                .unwrap(),
        );
        assert!(
            event.design_canvas().is_err(),
            "{data} should not decode as a design_canvas event"
        );
    }
}

#[test]
fn design_canvas_event_verifies_the_exact_event_name() {
    // A foreign event that happens to fit the shape is refused, not misread.
    let foreign = record_event(
        decode_widget(&widget(&event_line(
            1,
            "agents_update",
            json!({
                "transition": "open",
                "running": true,
                "slug": null,
                "connectedClients": 0,
                "designs": [],
            }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        foreign.design_canvas(),
        Err(DesktopRpcError::UnexpectedEvent { expected: "design_canvas", got })
            if got == "agents_update"
    ));

    // The right name with the wrong shape is still a shape error.
    let wrong_shape = record_event(
        decode_widget(&widget(&event_line(
            2,
            "design_canvas",
            json!({ "transition": "open", "running": true }),
        )))
        .unwrap()
        .unwrap(),
    );
    assert!(matches!(
        wrong_shape.design_canvas(),
        Err(DesktopRpcError::MalformedPayload(_))
    ));
}

#[test]
fn design_canvas_requests_flow_through_session_correlation() {
    let mut session = DesktopSession::new();

    let read = session.issue_get_design_canvas(None).unwrap();
    assert_eq!(read.request_id(), "desktop-1");
    assert_eq!(read.method(), DesktopMethod::GetDesignCanvas);
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::GetDesignCanvas))
    );

    // One request in flight, whatever the method.
    assert!(matches!(
        session.issue_get_design_canvas(Some(true)),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));

    session
        .accept(&widget(&response_line(
            "desktop-1",
            json!({ "running": true, "slug": null, "connectedClients": 0, "designs": [] }),
        )))
        .unwrap();
    assert!(session.pending().is_none());

    let with_url = session.issue_get_design_canvas(Some(true)).unwrap();
    assert_eq!(with_url.request_id(), "desktop-2");
    assert_eq!(with_url.method(), DesktopMethod::GetDesignCanvas);
    assert_eq!(
        with_url.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-2","method":"get_design_canvas","params":{{"includeUrl":true}}}}"#
        )
    );
}

#[test]
fn capabilities_expose_design_canvas_method_and_event() {
    let response = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(&["capabilities", "get_design_canvas"], &["design_canvas"]),
        )))
        .unwrap()
        .unwrap(),
    );
    let capabilities = response.capabilities().unwrap();
    assert!(capabilities.supports(DesktopMethod::GetDesignCanvas));
    assert!(capabilities.emits("design_canvas"));
    assert!(!capabilities.emits("design_canvas_token"));
}

// --- configuration stores: get_config, set_config and config_changed -----

fn profiles_value() -> Value {
    json!({
        "kind": "gentle-pi.agent_model_profiles",
        "version": 1,
        "active": "team",
        "profiles": { "team": { "explore": { "model": "anthropic/claude-sonnet-4" } } },
    })
}

fn fallbacks_value() -> Value {
    json!({
        "kind": "gentle-pi.profile_model_fallbacks",
        "version": 1,
        "profiles": {
            "team": {
                "*": [{ "model": "openai/gpt-shared" }],
                "worker": [{ "model": "openai/gpt-fallback", "thinking": "low" }],
            },
        },
    })
}

/// A documented `get_config` projection of `profiles` read from disk.
fn profiles_read() -> Value {
    json!({
        "store": "profiles",
        "scope": "global",
        "readOnly": false,
        "status": "ok",
        "revision": "sha256:1f0c",
        "value": profiles_value(),
        "layers": [{ "source": "global", "status": "ok" }],
        "writeTargets": [{ "target": "global", "status": "ok", "writeRevision": "sha256:8f2a" }],
    })
}

fn config_response(data: Value) -> DesktopResponse {
    record_response(
        decode_widget(&widget(&response_line("desktop-1", data)))
            .unwrap()
            .unwrap(),
    )
}

fn config_event(seq: u64, name: &str, data: Value) -> DesktopEvent {
    record_event(
        decode_widget(&widget(&event_line(seq, name, data)))
            .unwrap()
            .unwrap(),
    )
}

fn request_payload(request_id: &str, method: &str, params: Value) -> String {
    json!({
        "schema": SCHEMA,
        "requestId": request_id,
        "method": method,
        "params": params,
    })
    .to_string()
}

#[test]
fn config_methods_use_the_exact_wire_names() {
    for (method, name) in [
        (DesktopMethod::GetConfig, "get_config"),
        (DesktopMethod::SetConfig, "set_config"),
    ] {
        assert_eq!(method.as_str(), name);
        assert_eq!(method.to_string(), name);
        assert_eq!(DesktopMethod::parse(name).unwrap(), method);
        assert_eq!(name.parse::<DesktopMethod>().unwrap(), method);
    }
    assert!(matches!(
        DesktopMethod::parse("set_configs"),
        Err(DesktopRpcError::UnsupportedMethod(_))
    ));
}

#[test]
fn config_stores_are_a_closed_vocabulary_with_their_documented_shape() {
    let expected = [
        (
            ConfigStore::Profiles,
            "profiles",
            ConfigScope::Global,
            false,
        ),
        (
            ConfigStore::Models,
            "models",
            ConfigScope::GlobalProject,
            true,
        ),
        (
            ConfigStore::ProfileFallbacks,
            "profile_fallbacks",
            ConfigScope::Global,
            false,
        ),
        (
            ConfigStore::Subagents,
            "subagents",
            ConfigScope::GlobalProject,
            true,
        ),
        (ConfigStore::Pins, "pins", ConfigScope::LocalRepo, false),
        (
            ConfigStore::ManagedAssets,
            "managed_assets",
            ConfigScope::Global,
            true,
        ),
    ];
    assert_eq!(ConfigStore::ALL.len(), expected.len());
    for (store, (want, name, scope, read_only)) in ConfigStore::ALL.into_iter().zip(expected) {
        assert_eq!(store, want);
        assert_eq!(store.as_str(), name);
        assert_eq!(store.to_string(), name);
        assert_eq!(name.parse::<ConfigStore>().unwrap(), store);
        assert_eq!(store.scope(), scope);
        assert_eq!(store.is_read_only(), read_only);
    }
    for unknown in ["", "Profiles", "profile", "permission", "../profiles.json"] {
        assert!(
            matches!(
                unknown.parse::<ConfigStore>(),
                Err(DesktopRpcError::UnknownConfigStore(got)) if got == unknown
            ),
            "{unknown:?} must not be a store"
        );
    }

    for (scope, name) in [
        (ConfigScope::Global, "global"),
        (ConfigScope::GlobalProject, "global+project"),
        (ConfigScope::LocalRepo, "local+repo"),
    ] {
        assert_eq!(scope.as_str(), name);
        assert_eq!(name.parse::<ConfigScope>().unwrap(), scope);
    }
    assert!(matches!(
        "project".parse::<ConfigScope>(),
        Err(DesktopRpcError::InvalidConfigScope(_))
    ));

    for status in ConfigStatus::ALL {
        assert_eq!(status.as_str().parse::<ConfigStatus>().unwrap(), status);
    }
    assert_eq!(
        ConfigStatus::ALL.map(ConfigStatus::as_str),
        ["ok", "missing", "invalid"]
    );
    assert!(matches!(
        "stale".parse::<ConfigStatus>(),
        Err(DesktopRpcError::InvalidConfigStatus(_))
    ));

    assert_eq!(
        ConfigLayerSource::ALL.map(ConfigLayerSource::as_str),
        ["global", "project", "local", "repo"]
    );
    assert!(matches!(
        "cwd".parse::<ConfigLayerSource>(),
        Err(DesktopRpcError::InvalidConfigLayerSource(_))
    ));
    assert_eq!(
        ConfigWriteTarget::ALL.map(ConfigWriteTarget::as_str),
        ["global", "local", "repo"]
    );
    assert!(matches!(
        "project".parse::<ConfigWriteTarget>(),
        Err(DesktopRpcError::InvalidConfigWriteTarget(_))
    ));
    assert_eq!(PinSource::Local.as_str(), "local");
    assert_eq!(PinSource::Repo.as_str(), "repo");
    assert_eq!(PinSource::Local.write_target(), ConfigWriteTarget::Local);
    assert_eq!(PinSource::Repo.write_target(), ConfigWriteTarget::Repo);
}

#[test]
fn get_config_sends_exactly_the_store_param_for_every_store() {
    for store in ConfigStore::ALL {
        let request = DesktopRequest::get_config("desktop-9", store).unwrap();
        assert_eq!(request.method(), DesktopMethod::GetConfig);
        assert_eq!(
            request.payload(),
            format!(
                r#"{{"schema":"{SCHEMA}","requestId":"desktop-9","method":"get_config","params":{{"store":"{}"}}}}"#,
                store.as_str()
            )
        );
    }
}

#[test]
fn set_config_sends_exactly_store_data_and_expected_revision() {
    let profiles = DesktopRequest::set_config(
        "desktop-2",
        ConfigWrite::Profiles(ProfilesData::new(profiles_value()).unwrap()),
        "sha256:8f2a",
    )
    .unwrap();
    assert_eq!(profiles.method(), DesktopMethod::SetConfig);
    assert_eq!(
        profiles.payload(),
        request_payload(
            "desktop-2",
            "set_config",
            json!({
                "store": "profiles",
                "data": profiles_value(),
                "expectedRevision": "sha256:8f2a",
            }),
        )
    );

    let fallbacks = DesktopRequest::set_config(
        "desktop-3",
        ConfigWrite::ProfileFallbacks(ProfileFallbacksData::new(fallbacks_value()).unwrap()),
        "sha256:3c11",
    )
    .unwrap();
    assert_eq!(
        fallbacks.payload(),
        request_payload(
            "desktop-3",
            "set_config",
            json!({
                "store": "profile_fallbacks",
                "data": fallbacks_value(),
                "expectedRevision": "sha256:3c11",
            }),
        )
    );
}

#[test]
fn set_config_pins_select_one_layer_and_clear_with_an_explicit_null() {
    let set = ConfigWrite::pin(PinSource::Local, "team");
    assert_eq!(set.store(), ConfigStore::Pins);
    assert_eq!(set.write_target(), ConfigWriteTarget::Local);
    let request = DesktopRequest::set_config("desktop-4", set, "sha256:0d19").unwrap();
    assert_eq!(
        request.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-4","method":"set_config","params":{{"store":"pins","data":{{"source":"local","profile":"team"}},"expectedRevision":"sha256:0d19"}}}}"#
        )
    );

    // The clear names the layer and sends `profile: null`, never omits it.
    let clear = ConfigWrite::clear_pin(PinSource::Repo);
    assert_eq!(clear.write_target(), ConfigWriteTarget::Repo);
    let request = DesktopRequest::set_config("desktop-5", clear, "sha256:aa").unwrap();
    assert_eq!(
        request.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-5","method":"set_config","params":{{"store":"pins","data":{{"source":"repo","profile":null}},"expectedRevision":"sha256:aa"}}}}"#
        )
    );

    assert_eq!(
        ConfigWrite::Profiles(ProfilesData::new(profiles_value()).unwrap()).write_target(),
        ConfigWriteTarget::Global
    );
    assert_eq!(
        ConfigWrite::ProfileFallbacks(ProfileFallbacksData::new(fallbacks_value()).unwrap())
            .store(),
        ConfigStore::ProfileFallbacks
    );
}

#[test]
fn set_config_rejects_unusable_revisions_and_pin_names_before_sending() {
    assert!(matches!(
        DesktopRequest::set_config("desktop-1", ConfigWrite::pin(PinSource::Local, "team"), ""),
        Err(DesktopRpcError::InvalidExpectedRevision)
    ));
    assert!(matches!(
        DesktopRequest::set_config("desktop-1", ConfigWrite::pin(PinSource::Repo, ""), "rev"),
        Err(DesktopRpcError::InvalidPinProfile)
    ));
    let mut session = DesktopSession::new();
    assert!(matches!(
        session.issue_set_config(ConfigWrite::pin(PinSource::Local, "team"), ""),
        Err(DesktopRpcError::InvalidExpectedRevision)
    ));
    // A refused request never occupies the session.
    assert!(session.pending().is_none());
}

#[test]
fn store_data_is_any_object_and_the_server_validates_its_content() {
    assert_eq!(
        ProfilesData::new(profiles_value()).unwrap().as_value(),
        &profiles_value()
    );
    assert_eq!(
        ProfileFallbacksData::new(fallbacks_value())
            .unwrap()
            .as_value(),
        &fallbacks_value()
    );

    // Content the owner normalizer would refuse is still an object: the
    // client sends it and the server answers `invalid_data`.
    let server_validated: &[Value] = &[
        json!({}),
        json!({ "version": 2, "profiles": [] }),
        json!({ "kind": "gentle-pi.agent_model_profiles", "version": 1, "profiles": {}, "active": 7 }),
        json!({ "kind": "gentle-pi.agent_model_profiles", "version": 1, "profiles": {}, "workflow": [] }),
        json!({ "kind": "gentle-pi.profile_model_fallbacks", "version": 1, "profiles": {}, "path": "/etc/x" }),
    ];
    for data in server_validated {
        let profiles = ProfilesData::new(data.clone()).unwrap();
        assert_eq!(profiles.as_value(), data);
        let fallbacks = ProfileFallbacksData::new(data.clone()).unwrap();
        assert_eq!(fallbacks.as_value(), data);
    }

    // Whatever `data` holds, the envelope stays exactly `{ store, data,
    // expectedRevision }`: a member inside `data` never becomes a param.
    let request = DesktopRequest::set_config(
        "desktop-1",
        ConfigWrite::Profiles(
            ProfilesData::new(json!({ "path": "/etc/x", "store": "models" })).unwrap(),
        ),
        "sha256:8f2a",
    )
    .unwrap();
    assert_eq!(
        request.payload(),
        request_payload(
            "desktop-1",
            "set_config",
            json!({
                "store": "profiles",
                "data": { "path": "/etc/x", "store": "models" },
                "expectedRevision": "sha256:8f2a",
            }),
        )
    );

    for data in [json!(null), json!([]), json!("profiles.json"), json!(1)] {
        assert!(
            matches!(
                ProfilesData::new(data.clone()),
                Err(DesktopRpcError::InvalidConfigData(_))
            ),
            "{data} must not be profiles data"
        );
        assert!(
            matches!(
                ProfileFallbacksData::new(data.clone()),
                Err(DesktopRpcError::InvalidConfigData(_))
            ),
            "{data} must not be fallbacks data"
        );
    }
}

#[test]
fn config_methods_cannot_be_sent_with_untyped_params() {
    // The generic constructor and the zero-param issue path would let a path
    // or a read-only store through; only the typed API builds these methods.
    for method in [DesktopMethod::GetConfig, DesktopMethod::SetConfig] {
        assert!(matches!(
            DesktopRequest::new("desktop-1", method, json!({ "store": "models", "path": "/etc" })),
            Err(DesktopRpcError::TypedParamsRequired(got)) if got == method
        ));
        let mut session = DesktopSession::new();
        assert!(matches!(
            session.issue(method),
            Err(DesktopRpcError::TypedParamsRequired(got)) if got == method
        ));
        assert!(session.pending().is_none());
    }
    // Existing methods keep their generic path.
    assert!(DesktopRequest::new("desktop-1", DesktopMethod::GetSnapshot, json!({})).is_ok());
}

#[test]
fn get_config_decodes_a_writable_store_with_its_write_targets() {
    let state = config_response(profiles_read()).config().unwrap();
    assert_eq!(
        state,
        ConfigState {
            store: ConfigStore::Profiles,
            scope: ConfigScope::Global,
            read_only: false,
            status: ConfigStatus::Ok,
            revision: "sha256:1f0c".into(),
            value: Some(profiles_value()),
            layers: vec![ConfigLayer {
                source: ConfigLayerSource::Global,
                status: ConfigStatus::Ok,
            }],
            write_targets: Some(vec![ConfigWriteTargetState {
                target: ConfigWriteTarget::Global,
                status: ConfigStatus::Ok,
                write_revision: "sha256:8f2a".into(),
            }]),
        }
    );
    assert_eq!(
        state.write_revision(ConfigWriteTarget::Global),
        Some("sha256:8f2a")
    );
    assert_eq!(state.write_revision(ConfigWriteTarget::Local), None);
}

#[test]
fn get_config_decodes_every_store_and_status() {
    // A missing writable store: no value, and the missing target still has a
    // write revision of its own.
    let missing = config_response(json!({
        "store": "profile_fallbacks",
        "scope": "global",
        "readOnly": false,
        "status": "missing",
        "revision": "sha256:3c11",
        "layers": [{ "source": "global", "status": "missing" }],
        "writeTargets": [{ "target": "global", "status": "missing", "writeRevision": "sha256:77" }],
    }))
    .config()
    .unwrap();
    assert_eq!(missing.store, ConfigStore::ProfileFallbacks);
    assert_eq!(missing.status, ConfigStatus::Missing);
    assert_eq!(missing.value, None);
    assert_eq!(
        missing.write_targets.as_ref().unwrap()[0].status,
        ConfigStatus::Missing
    );

    // A read-only merged store reported invalid, with no write targets.
    let models = config_response(json!({
        "store": "models",
        "scope": "global+project",
        "readOnly": true,
        "status": "invalid",
        "revision": "sha256:5a61",
        "layers": [
            { "source": "global", "status": "invalid" },
            { "source": "project", "status": "ok" },
        ],
    }))
    .config()
    .unwrap();
    assert_eq!(models.store, ConfigStore::Models);
    assert_eq!(models.scope, ConfigScope::GlobalProject);
    assert!(models.read_only);
    assert_eq!(models.status, ConfigStatus::Invalid);
    assert_eq!(models.value, None);
    assert_eq!(models.write_targets, None);
    assert_eq!(
        models.layers,
        vec![
            ConfigLayer {
                source: ConfigLayerSource::Global,
                status: ConfigStatus::Invalid,
            },
            ConfigLayer {
                source: ConfigLayerSource::Project,
                status: ConfigStatus::Ok,
            },
        ]
    );

    // A merged store stays `ok` while one layer is broken.
    let subagents = config_response(json!({
        "store": "subagents",
        "scope": "global+project",
        "readOnly": true,
        "status": "ok",
        "revision": "sha256:77be",
        "value": { "defaultMode": "task" },
        "layers": [
            { "source": "global", "status": "ok" },
            { "source": "project", "status": "invalid" },
        ],
    }))
    .config()
    .unwrap();
    assert_eq!(subagents.store, ConfigStore::Subagents);
    assert_eq!(subagents.value, Some(json!({ "defaultMode": "task" })));

    let assets = config_response(json!({
        "store": "managed_assets",
        "scope": "global",
        "readOnly": true,
        "status": "ok",
        "revision": "sha256:be05",
        "value": { "schemaVersion": 1, "assets": {} },
        "layers": [{ "source": "global", "status": "ok" }],
    }))
    .config()
    .unwrap();
    assert_eq!(assets.store, ConfigStore::ManagedAssets);
    assert!(assets.read_only);

    // Pins: two layers, two write targets, and a value even with a stale layer.
    let pins = config_response(json!({
        "store": "pins",
        "scope": "local+repo",
        "readOnly": false,
        "status": "ok",
        "revision": "sha256:0d19",
        "value": {
            "resolved": true,
            "winner": { "source": "repo", "profile": "team" },
            "invalid": [],
            "stale": [{ "source": "local", "profile": "retired" }],
        },
        "layers": [
            { "source": "local", "status": "ok" },
            { "source": "repo", "status": "ok" },
        ],
        "writeTargets": [
            { "target": "local", "status": "ok", "writeRevision": "sha256:l1" },
            { "target": "repo", "status": "ok", "writeRevision": "sha256:r1" },
        ],
    }))
    .config()
    .unwrap();
    assert_eq!(pins.scope, ConfigScope::LocalRepo);
    assert_eq!(
        pins.write_revision(ConfigWriteTarget::Local),
        Some("sha256:l1")
    );
    assert_eq!(
        pins.write_revision(ConfigWriteTarget::Repo),
        Some("sha256:r1")
    );
    assert_eq!(pins.write_revision(ConfigWriteTarget::Global), None);

    // Outside a worktree: invalid, empty layers, and no target offered.
    let outside = config_response(json!({
        "store": "pins",
        "scope": "local+repo",
        "readOnly": false,
        "status": "invalid",
        "revision": "sha256:00",
        "value": { "reason": "worktree_unresolved" },
        "layers": [],
        "writeTargets": [],
    }))
    .config()
    .unwrap();
    assert_eq!(outside.status, ConfigStatus::Invalid);
    assert!(outside.layers.is_empty());
    assert_eq!(outside.write_targets, Some(vec![]));
}

#[test]
fn get_config_rejects_shapes_outside_the_contract() {
    let mut cases: Vec<(&str, Value)> = Vec::new();
    let with = |key: &str, value: Value| {
        let mut data = profiles_read();
        data[key] = value;
        data
    };
    let without = |key: &str| {
        let mut data = profiles_read();
        data.as_object_mut().unwrap().remove(key);
        data
    };
    cases.push(("unknown store", with("store", json!("permission"))));
    cases.push(("missing store", without("store")));
    cases.push(("unknown scope", with("scope", json!("project"))));
    cases.push(("scope of another store", with("scope", json!("local+repo"))));
    cases.push(("readOnly not a bool", with("readOnly", json!("false"))));
    cases.push(("readOnly disagrees", with("readOnly", json!(true))));
    cases.push(("unknown status", with("status", json!("stale"))));
    cases.push(("missing status", without("status")));
    cases.push(("missing revision", without("revision")));
    cases.push(("empty revision", with("revision", json!(""))));
    cases.push(("value not an object", with("value", json!("profiles.json"))));
    cases.push(("missing layers", without("layers")));
    cases.push((
        "layer source",
        with("layers", json!([{ "source": "cwd", "status": "ok" }])),
    ));
    cases.push((
        "layer status",
        with("layers", json!([{ "source": "global", "status": "gone" }])),
    ));
    cases.push(("writable without writeTargets", without("writeTargets")));
    cases.push(("writeTargets not an array", with("writeTargets", json!({}))));
    cases.push((
        "unknown target",
        with(
            "writeTargets",
            json!([{ "target": "project", "status": "ok", "writeRevision": "r" }]),
        ),
    ));
    cases.push((
        "target of another store",
        with(
            "writeTargets",
            json!([{ "target": "local", "status": "ok", "writeRevision": "r" }]),
        ),
    ));
    cases.push((
        "target status",
        with(
            "writeTargets",
            json!([{ "target": "global", "status": "fine", "writeRevision": "r" }]),
        ),
    ));
    cases.push((
        "missing writeRevision",
        with(
            "writeTargets",
            json!([{ "target": "global", "status": "ok" }]),
        ),
    ));
    cases.push((
        "empty writeRevision",
        with(
            "writeTargets",
            json!([{ "target": "global", "status": "ok", "writeRevision": "" }]),
        ),
    ));
    // A read-only store must not offer a write target.
    cases.push((
        "read-only with writeTargets",
        json!({
            "store": "models",
            "scope": "global+project",
            "readOnly": true,
            "status": "ok",
            "revision": "sha256:9a42",
            "value": {},
            "layers": [],
            "writeTargets": [{ "target": "global", "status": "ok", "writeRevision": "r" }],
        }),
    ));
    for (label, data) in cases {
        assert!(
            config_response(data.clone()).config().is_err(),
            "{label}: {data} must not decode as a config read"
        );
    }
    assert!(matches!(
        config_response(with("store", json!("permission"))).config(),
        Err(DesktopRpcError::UnknownConfigStore(_))
    ));
    assert!(matches!(
        config_response(with("status", json!("stale"))).config(),
        Err(DesktopRpcError::InvalidConfigStatus(_))
    ));
}

#[test]
fn get_config_keeps_only_the_documented_value_rules() {
    let with = |base: Value, key: &str, value: Value| {
        let mut data = base;
        data[key] = value;
        data
    };
    let without = |base: Value, key: &str| {
        let mut data = base;
        data.as_object_mut().unwrap().remove(key);
        data
    };
    let pins = json!({
        "store": "pins",
        "scope": "local+repo",
        "readOnly": false,
        "status": "missing",
        "revision": "sha256:p0",
        "value": { "resolved": false, "invalid": [], "stale": [] },
        "layers": [
            { "source": "local", "status": "missing" },
            { "source": "repo", "status": "missing" },
        ],
        "writeTargets": [
            { "target": "local", "status": "missing", "writeRevision": "sha256:lm" },
            { "target": "repo", "status": "missing", "writeRevision": "sha256:rm" },
        ],
    });
    // `pins` keeps its value whatever its status.
    assert_eq!(
        config_response(pins.clone()).config().unwrap().value,
        Some(json!({ "resolved": false, "invalid": [], "stale": [] }))
    );

    // Documented: `ok` means normalized content follows, and `pins` always
    // carries a value.
    let rejected: Vec<(&str, Value)> = vec![
        ("ok without value", without(profiles_read(), "value")),
        (
            "ok with null value",
            with(profiles_read(), "value", Value::Null),
        ),
        ("pins without value", without(pins.clone(), "value")),
    ];
    for (label, data) in rejected {
        assert!(
            matches!(
                config_response(data.clone()).config(),
                Err(DesktopRpcError::MalformedPayload(_))
            ),
            "{label}: {data} must not decode as a config read"
        );
    }

    // A stray value on a store that read `missing` or `invalid` is ignored.
    for status in ["missing", "invalid"] {
        let state = config_response(with(profiles_read(), "status", json!(status)))
            .config()
            .unwrap();
        assert_eq!(state.status.as_str(), status);
        assert_eq!(state.value, None);
    }
    let models = config_response(json!({
        "store": "models",
        "scope": "global+project",
        "readOnly": true,
        "status": "invalid",
        "revision": "sha256:5a61",
        "value": "not content",
        "layers": [{ "source": "global", "status": "invalid" }],
    }))
    .config()
    .unwrap();
    assert_eq!(models.value, None);
}

#[test]
fn get_config_leaves_layer_and_target_lists_to_the_server() {
    let with = |base: Value, key: &str, value: Value| {
        let mut data = base;
        data[key] = value;
        data
    };
    // Layer order, membership and repetition are reported as sent.
    let layers = json!([
        { "source": "project", "status": "ok" },
        { "source": "global", "status": "missing" },
        { "source": "global", "status": "ok" },
    ]);
    let state = config_response(with(profiles_read(), "layers", layers))
        .config()
        .unwrap();
    assert_eq!(
        state.layers,
        vec![
            ConfigLayer {
                source: ConfigLayerSource::Project,
                status: ConfigStatus::Ok,
            },
            ConfigLayer {
                source: ConfigLayerSource::Global,
                status: ConfigStatus::Missing,
            },
            ConfigLayer {
                source: ConfigLayerSource::Global,
                status: ConfigStatus::Ok,
            },
        ]
    );

    // A repeated write target decodes; the first entry answers the lookup.
    let targets = json!([
        { "target": "global", "status": "ok", "writeRevision": "sha256:a" },
        { "target": "global", "status": "ok", "writeRevision": "sha256:b" },
    ]);
    let state = config_response(with(profiles_read(), "writeTargets", targets))
        .config()
        .unwrap();
    assert_eq!(state.write_targets.as_ref().unwrap().len(), 2);
    assert_eq!(
        state.write_revision(ConfigWriteTarget::Global),
        Some("sha256:a")
    );
}

#[test]
fn set_config_response_decodes_the_reread_state_and_its_write() {
    let mut data = profiles_read();
    data["revision"] = json!("sha256:1c80");
    data["writeTargets"] =
        json!([{ "target": "global", "status": "ok", "writeRevision": "sha256:c4b0" }]);
    data["writeTarget"] = json!("global");
    data["writeRevision"] = json!("sha256:c4b0");
    data["changed"] = json!(true);
    let result = config_response(data.clone()).set_config().unwrap();
    assert_eq!(
        result,
        SetConfigResult {
            state: ConfigState {
                store: ConfigStore::Profiles,
                scope: ConfigScope::Global,
                read_only: false,
                status: ConfigStatus::Ok,
                revision: "sha256:1c80".into(),
                value: Some(profiles_value()),
                layers: vec![ConfigLayer {
                    source: ConfigLayerSource::Global,
                    status: ConfigStatus::Ok,
                }],
                write_targets: Some(vec![ConfigWriteTargetState {
                    target: ConfigWriteTarget::Global,
                    status: ConfigStatus::Ok,
                    write_revision: "sha256:c4b0".into(),
                }]),
            },
            write_target: ConfigWriteTarget::Global,
            write_revision: "sha256:c4b0".into(),
            changed: true,
        }
    );

    // A pin clear of an absent layer is a no-op: `changed: false`.
    let clear = config_response(json!({
        "store": "pins",
        "scope": "local+repo",
        "readOnly": false,
        "status": "missing",
        "revision": "sha256:p0",
        "value": { "resolved": false, "invalid": [], "stale": [] },
        "layers": [
            { "source": "local", "status": "missing" },
            { "source": "repo", "status": "missing" },
        ],
        "writeTargets": [
            { "target": "local", "status": "missing", "writeRevision": "sha256:lm" },
            { "target": "repo", "status": "missing", "writeRevision": "sha256:rm" },
        ],
        "writeTarget": "repo",
        "writeRevision": "sha256:rm",
        "changed": false,
    }))
    .set_config()
    .unwrap();
    assert_eq!(clear.write_target, ConfigWriteTarget::Repo);
    assert!(!clear.changed);

    let mut bad: Vec<(&str, Value)> = Vec::new();
    for key in ["writeTarget", "writeRevision", "changed"] {
        let mut missing = data.clone();
        missing.as_object_mut().unwrap().remove(key);
        bad.push((key, missing));
    }
    let mut wrong = data.clone();
    wrong["changed"] = json!("yes");
    bad.push(("changed type", wrong));
    let mut wrong = data.clone();
    wrong["writeTarget"] = json!("repo");
    bad.push(("target of another store", wrong));
    let mut wrong = data.clone();
    wrong["writeRevision"] = json!("sha256:other");
    bad.push(("revision differs from its target", wrong));
    let mut wrong = data.clone();
    wrong["writeRevision"] = json!("");
    bad.push(("empty revision", wrong));
    let mut wrong = data.clone();
    wrong["store"] = json!("models");
    wrong["scope"] = json!("global+project");
    wrong["readOnly"] = json!(true);
    wrong.as_object_mut().unwrap().remove("writeTargets");
    bad.push(("read-only store", wrong));
    for (label, data) in bad {
        assert!(
            config_response(data.clone()).set_config().is_err(),
            "{label}: {data} must not decode as a set_config result"
        );
    }
}

#[test]
fn config_failures_keep_their_documented_codes() {
    let codes = [
        (ConfigErrorCode::UnknownStore, "unknown_store"),
        (ConfigErrorCode::ReadOnly, "read_only"),
        (ConfigErrorCode::InvalidData, "invalid_data"),
        (ConfigErrorCode::RevisionConflict, "revision_conflict"),
        (ConfigErrorCode::WriteFailed, "write_failed"),
        (ConfigErrorCode::ConfigUnavailable, "config_unavailable"),
        // A profile store write refused while an apply runs elsewhere.
        (ConfigErrorCode::ProfileApplyBusy, "profile_apply_busy"),
    ];
    assert_eq!(ConfigErrorCode::ALL.len(), codes.len());
    for (code, name) in codes {
        assert_eq!(code.as_str(), name);
        assert_eq!(ConfigErrorCode::parse(name), Some(code));
        let response = record_response(
            decode_widget(&widget(&error_line("desktop-1", name, "nope")))
                .unwrap()
                .unwrap(),
        );
        assert_eq!(response.error().unwrap().config_error(), Some(code));
        assert!(matches!(
            response.set_config(),
            Err(DesktopRpcError::RemoteError(info)) if info.code == name
        ));
        assert!(response.config().is_err());
    }
    assert_eq!(ConfigErrorCode::parse("internal_error"), None);

    let conflict = record_response(
        decode_widget(&widget(&error_line(
            "desktop-1",
            "revision_conflict",
            "Config changed since expectedRevision was read",
        )))
        .unwrap()
        .unwrap(),
    );
    let info = conflict.error().unwrap();
    assert_eq!(info.config_error(), Some(ConfigErrorCode::RevisionConflict));
    assert_eq!(
        info.message,
        "Config changed since expectedRevision was read"
    );
    let other = record_response(
        decode_widget(&widget(&error_line("desktop-1", "invalid_request", "x")))
            .unwrap()
            .unwrap(),
    );
    assert_eq!(other.error().unwrap().config_error(), None);
}

#[test]
fn config_changed_decodes_its_exact_data_and_checks_the_event_name() {
    let data = json!({
        "store": "profiles",
        "scope": "global",
        "writeTarget": "global",
        "revision": "sha256:1c80",
        "writeRevision": "sha256:c4b0",
        "source": "desktop",
    });
    let event = config_event(1, "config_changed", data.clone());
    assert_eq!(
        event.config_changed().unwrap(),
        ConfigChanged {
            store: ConfigStore::Profiles,
            scope: ConfigScope::Global,
            write_target: ConfigWriteTarget::Global,
            revision: "sha256:1c80".into(),
            write_revision: "sha256:c4b0".into(),
            source: "desktop".into(),
        }
    );

    let pin = config_event(
        2,
        "config_changed",
        json!({
            "store": "pins",
            "scope": "local+repo",
            "writeTarget": "local",
            "revision": "sha256:p",
            "writeRevision": "sha256:l",
            "source": "desktop",
        }),
    )
    .config_changed()
    .unwrap();
    assert_eq!(pin.write_target, ConfigWriteTarget::Local);

    // A foreign event with the same shape is refused, not misread.
    let foreign = config_event(3, "permission_mode_changed", data.clone());
    assert!(matches!(
        foreign.config_changed(),
        Err(DesktopRpcError::UnexpectedEvent { expected: "config_changed", got })
            if got == "permission_mode_changed"
    ));

    let mut bad: Vec<Value> = Vec::new();
    for key in [
        "store",
        "scope",
        "writeTarget",
        "revision",
        "writeRevision",
        "source",
    ] {
        let mut missing = data.clone();
        missing.as_object_mut().unwrap().remove(key);
        bad.push(missing);
    }
    for (key, value) in [
        ("store", json!("models")),
        ("store", json!("unknown")),
        ("scope", json!("local+repo")),
        ("writeTarget", json!("repo")),
        ("revision", json!("")),
        ("writeRevision", json!(7)),
    ] {
        let mut wrong = data.clone();
        wrong[key] = value;
        bad.push(wrong);
    }
    for data in bad {
        assert!(
            config_event(4, "config_changed", data.clone())
                .config_changed()
                .is_err(),
            "{data} must not decode as config_changed"
        );
    }
}

#[test]
fn config_requests_flow_through_session_correlation_and_sequence() {
    let mut session = DesktopSession::new();

    let read = session.issue_get_config(ConfigStore::Pins).unwrap();
    assert_eq!(read.request_id(), "desktop-1");
    assert_eq!(read.method(), DesktopMethod::GetConfig);
    assert_eq!(
        read.payload(),
        request_payload("desktop-1", "get_config", json!({ "store": "pins" }))
    );
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::GetConfig))
    );
    assert!(matches!(
        session.issue_set_config(ConfigWrite::clear_pin(PinSource::Local), "sha256:l"),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));
    // A response for another id never answers this one.
    assert!(matches!(
        session.accept(&widget(&response_line("desktop-9", profiles_read()))),
        Err(DesktopRpcError::UncorrelatedResponse { .. })
    ));
    session
        .accept(&widget(&response_line("desktop-1", profiles_read())))
        .unwrap();
    assert!(session.pending().is_none());

    let write = session
        .issue_set_config(ConfigWrite::clear_pin(PinSource::Local), "sha256:l")
        .unwrap();
    assert_eq!(write.request_id(), "desktop-2");
    assert_eq!(write.method(), DesktopMethod::SetConfig);
    assert_eq!(
        write.payload(),
        request_payload(
            "desktop-2",
            "set_config",
            json!({
                "store": "pins",
                "data": { "source": "local", "profile": null },
                "expectedRevision": "sha256:l",
            }),
        )
    );
    // A conflict answers and releases the request.
    let answered = session
        .accept(&widget(&error_line(
            "desktop-2",
            "revision_conflict",
            "Config changed since expectedRevision was read",
        )))
        .unwrap()
        .unwrap();
    assert_eq!(
        record_response(answered).error().unwrap().config_error(),
        Some(ConfigErrorCode::RevisionConflict)
    );
    assert!(session.pending().is_none());

    // The change event follows the sequence rules like any other event.
    let changed = json!({
        "store": "pins",
        "scope": "local+repo",
        "writeTarget": "local",
        "revision": "sha256:p",
        "writeRevision": "sha256:l2",
        "source": "desktop",
    });
    let first = session
        .accept(&widget(&event_line(4, "config_changed", changed.clone())))
        .unwrap()
        .unwrap();
    assert_eq!(
        record_event(first).config_changed().unwrap().write_revision,
        "sha256:l2"
    );
    assert!(matches!(
        session.accept(&widget(&event_line(4, "config_changed", changed.clone()))),
        Err(DesktopRpcError::DuplicateSequence { .. })
    ));
    assert!(matches!(
        session.accept(&widget(&event_line(6, "config_changed", changed))),
        Err(DesktopRpcError::OutOfOrderSequence { .. })
    ));
}

#[test]
fn capabilities_expose_the_config_methods_and_event() {
    let response = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(
                &["capabilities", "get_config", "set_config"],
                &["config_changed"],
            ),
        )))
        .unwrap()
        .unwrap(),
    );
    let capabilities = response.capabilities().unwrap();
    assert!(capabilities.supports(DesktopMethod::GetConfig));
    assert!(capabilities.supports(DesktopMethod::SetConfig));
    assert!(capabilities.emits("config_changed"));

    let without = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(&["capabilities", "get_config"], &[]),
        )))
        .unwrap()
        .unwrap(),
    )
    .capabilities()
    .unwrap();
    assert!(without.supports(DesktopMethod::GetConfig));
    assert!(!without.supports(DesktopMethod::SetConfig));
    assert!(!without.emits("config_changed"));
}

// --- profile apply: apply_profile and profile_applied ---------------------

fn apply_response(data: Value) -> DesktopResponse {
    record_response(
        decode_widget(&widget(&response_line("desktop-1", data)))
            .unwrap()
            .unwrap(),
    )
}

fn confirmation_data() -> Value {
    json!({
        "profile": "team",
        "mode": "default",
        "status": "confirmation_required",
        "confirmation": {
            "title": "Apply profile \"team\"?",
            "message": "Profile \"team\" has agent routing entries. Continue?",
            "token": "3f1c",
        },
    })
}

#[test]
fn apply_profile_uses_the_exact_wire_names() {
    assert_eq!(DesktopMethod::ApplyProfile.as_str(), "apply_profile");
    assert_eq!(
        DesktopMethod::parse("apply_profile").unwrap(),
        DesktopMethod::ApplyProfile
    );
    assert_eq!(
        ProfileApplyMode::ALL.map(ProfileApplyMode::as_str),
        ["default", "session"]
    );
    for mode in ProfileApplyMode::ALL {
        assert_eq!(mode.as_str().parse::<ProfileApplyMode>().unwrap(), mode);
        assert_eq!(mode.to_string(), mode.as_str());
    }
    for unknown in ["", "Default", "global", "repo"] {
        assert!(matches!(
            unknown.parse::<ProfileApplyMode>(),
            Err(DesktopRpcError::InvalidProfileApplyMode(got)) if got == unknown
        ));
    }
}

#[test]
fn apply_profile_sends_exactly_profile_mode_and_an_optional_confirmation() {
    let first =
        DesktopRequest::apply_profile("desktop-62", "team", ProfileApplyMode::Default, None)
            .unwrap();
    assert_eq!(first.method(), DesktopMethod::ApplyProfile);
    assert_eq!(
        first.payload(),
        format!(
            r#"{{"schema":"{SCHEMA}","requestId":"desktop-62","method":"apply_profile","params":{{"profile":"team","mode":"default"}}}}"#
        )
    );
    let confirmed = DesktopRequest::apply_profile(
        "desktop-63",
        "team",
        ProfileApplyMode::Default,
        Some("3f1c"),
    )
    .unwrap();
    assert_eq!(
        confirmed.payload(),
        request_payload(
            "desktop-63",
            "apply_profile",
            json!({ "profile": "team", "mode": "default", "confirmation": "3f1c" }),
        )
    );
    let session =
        DesktopRequest::apply_profile("desktop-64", "team", ProfileApplyMode::Session, None)
            .unwrap();
    assert_eq!(
        session.payload(),
        request_payload(
            "desktop-64",
            "apply_profile",
            json!({ "profile": "team", "mode": "session" }),
        )
    );
}

#[test]
fn apply_profile_rejects_unusable_names_and_tokens_before_sending() {
    let long = "p".repeat(129);
    for profile in ["", long.as_str()] {
        assert!(matches!(
            DesktopRequest::apply_profile("desktop-1", profile, ProfileApplyMode::Default, None),
            Err(DesktopRpcError::InvalidProfileName)
        ));
    }
    assert!(
        DesktopRequest::apply_profile(
            "desktop-1",
            &"p".repeat(128),
            ProfileApplyMode::Default,
            None
        )
        .is_ok()
    );
    for token in ["", long.as_str()] {
        assert!(matches!(
            DesktopRequest::apply_profile(
                "desktop-1",
                "team",
                ProfileApplyMode::Default,
                Some(token)
            ),
            Err(DesktopRpcError::InvalidConfirmationToken)
        ));
    }
    let mut session = DesktopSession::new();
    assert!(matches!(
        session.issue_apply_profile("", ProfileApplyMode::Session, None),
        Err(DesktopRpcError::InvalidProfileName)
    ));
    // A refused request never occupies the session.
    assert!(session.pending().is_none());
    // Only the typed API builds the method.
    assert!(matches!(
        DesktopRequest::new(
            "desktop-1",
            DesktopMethod::ApplyProfile,
            json!({ "profile": "team", "mode": "default", "path": "/etc" }),
        ),
        Err(DesktopRpcError::TypedParamsRequired(
            DesktopMethod::ApplyProfile
        ))
    ));
    assert!(matches!(
        session.issue(DesktopMethod::ApplyProfile),
        Err(DesktopRpcError::TypedParamsRequired(
            DesktopMethod::ApplyProfile
        ))
    ));
}

#[test]
fn apply_profile_decodes_a_confirmation_with_its_exact_text_and_token() {
    let result = apply_response(confirmation_data()).apply_profile().unwrap();
    assert_eq!(
        result,
        ApplyProfileResult {
            profile: "team".into(),
            mode: ProfileApplyMode::Default,
            outcome: ApplyProfileOutcome::ConfirmationRequired(ProfileConfirmation {
                title: "Apply profile \"team\"?".into(),
                message: "Profile \"team\" has agent routing entries. Continue?".into(),
                token: "3f1c".into(),
            }),
        }
    );
}

#[test]
fn apply_profile_decodes_applied_and_not_applied_with_their_notes() {
    let applied = apply_response(json!({
        "profile": "team",
        "mode": "session",
        "status": "applied",
        "notes": [
            { "level": "warning", "text": "Seeded profiles.json." },
            { "level": "info", "text": "Applied profile \"team\"." },
        ],
    }))
    .apply_profile()
    .unwrap();
    assert_eq!(applied.mode, ProfileApplyMode::Session);
    assert_eq!(
        applied.outcome,
        ApplyProfileOutcome::Applied(vec![
            ProfileNote {
                level: ProfileNoteLevel::Warning,
                text: "Seeded profiles.json.".into(),
            },
            ProfileNote {
                level: ProfileNoteLevel::Info,
                text: "Applied profile \"team\".".into(),
            },
        ])
    );

    // `not_applied` may end with no notice at all.
    let quiet = apply_response(json!({
        "profile": "team",
        "mode": "default",
        "status": "not_applied",
        "notes": [],
    }))
    .apply_profile()
    .unwrap();
    assert_eq!(quiet.outcome, ApplyProfileOutcome::NotApplied(vec![]));
    let failed = apply_response(json!({
        "profile": "team",
        "mode": "default",
        "status": "not_applied",
        "notes": [{ "level": "error", "text": "rolled back" }],
    }))
    .apply_profile()
    .unwrap();
    assert_eq!(
        failed.outcome,
        ApplyProfileOutcome::NotApplied(vec![ProfileNote {
            level: ProfileNoteLevel::Error,
            text: "rolled back".into(),
        }])
    );
    assert_eq!(
        ProfileNoteLevel::ALL.map(ProfileNoteLevel::as_str),
        ["info", "warning", "error"]
    );
}

#[test]
fn apply_profile_rejects_answers_outside_the_contract() {
    let applied = json!({
        "profile": "team",
        "mode": "default",
        "status": "applied",
        "notes": [{ "level": "info", "text": "ok" }],
    });
    let with = |base: &Value, key: &str, value: Value| {
        let mut data = base.clone();
        data[key] = value;
        data
    };
    let without = |base: &Value, key: &str| {
        let mut data = base.clone();
        data.as_object_mut().unwrap().remove(key);
        data
    };
    let confirmation = confirmation_data();
    let cases: Vec<(&str, Value)> = vec![
        ("missing profile", without(&applied, "profile")),
        ("empty profile", with(&applied, "profile", json!(""))),
        ("unknown mode", with(&applied, "mode", json!("global"))),
        ("unknown status", with(&applied, "status", json!("done"))),
        ("missing notes", without(&applied, "notes")),
        ("notes not an array", with(&applied, "notes", json!("ok"))),
        (
            "unknown note level",
            with(
                &applied,
                "notes",
                json!([{ "level": "debug", "text": "x" }]),
            ),
        ),
        (
            "note without text",
            with(&applied, "notes", json!([{ "level": "info" }])),
        ),
        (
            "confirmation missing",
            without(&confirmation, "confirmation"),
        ),
        (
            "confirmation without token",
            with(
                &confirmation,
                "confirmation",
                json!({ "title": "t", "message": "m" }),
            ),
        ),
        (
            "confirmation with an empty token",
            with(
                &confirmation,
                "confirmation",
                json!({ "title": "t", "message": "m", "token": "" }),
            ),
        ),
        (
            "confirmation without a message",
            with(
                &confirmation,
                "confirmation",
                json!({ "title": "t", "token": "x" }),
            ),
        ),
    ];
    for (label, data) in cases {
        assert!(
            apply_response(data.clone()).apply_profile().is_err(),
            "{label}: {data} must not decode as an apply_profile answer"
        );
    }
    assert!(matches!(
        apply_response(with(&applied, "status", json!("done"))).apply_profile(),
        Err(DesktopRpcError::InvalidProfileApplyStatus(got)) if got == "done"
    ));
    assert!(matches!(
        apply_response(with(
            &applied,
            "notes",
            json!([{ "level": "debug", "text": "x" }])
        ))
        .apply_profile(),
        Err(DesktopRpcError::InvalidProfileNoteLevel(got)) if got == "debug"
    ));
}

#[test]
fn apply_profile_failures_keep_their_documented_codes() {
    let codes = [
        (ProfileApplyErrorCode::UnknownProfile, "unknown_profile"),
        (
            ProfileApplyErrorCode::ProfileApplyBusy,
            "profile_apply_busy",
        ),
        (
            ProfileApplyErrorCode::ProfilesUnavailable,
            "profiles_unavailable",
        ),
    ];
    assert_eq!(ProfileApplyErrorCode::ALL.len(), codes.len());
    for (code, name) in codes {
        assert_eq!(code.as_str(), name);
        assert_eq!(code.to_string(), name);
        assert_eq!(ProfileApplyErrorCode::parse(name), Some(code));
        let response = record_response(
            decode_widget(&widget(&error_line("desktop-1", name, "nope")))
                .unwrap()
                .unwrap(),
        );
        let info = response.error().unwrap();
        assert_eq!(info.profile_apply_error(), Some(code));
        // Only `profile_apply_busy` is also a `set_config` failure.
        assert_eq!(
            info.config_error(),
            (code == ProfileApplyErrorCode::ProfileApplyBusy)
                .then_some(ConfigErrorCode::ProfileApplyBusy)
        );
        assert!(matches!(
            response.apply_profile(),
            Err(DesktopRpcError::RemoteError(info)) if info.code == name
        ));
    }
    assert_eq!(ProfileApplyErrorCode::parse("invalid_request"), None);
    assert_eq!(ProfileApplyErrorCode::parse("revision_conflict"), None);
}

#[test]
fn profile_applied_decodes_its_exact_data_and_checks_the_event_name() {
    let data = json!({ "profile": "team", "mode": "default", "source": "desktop" });
    let event = config_event(3, "profile_applied", data.clone());
    assert_eq!(
        event.profile_applied().unwrap(),
        ProfileApplied {
            profile: "team".into(),
            mode: ProfileApplyMode::Default,
            source: "desktop".into(),
        }
    );
    let foreign = config_event(4, "config_changed", data.clone());
    assert!(matches!(
        foreign.profile_applied(),
        Err(DesktopRpcError::UnexpectedEvent { expected: "profile_applied", got })
            if got == "config_changed"
    ));
    let mut bad: Vec<Value> = Vec::new();
    for key in ["profile", "mode", "source"] {
        let mut missing = data.clone();
        missing.as_object_mut().unwrap().remove(key);
        bad.push(missing);
    }
    for (key, value) in [
        ("profile", json!("")),
        ("mode", json!("global")),
        ("source", json!(7)),
    ] {
        let mut wrong = data.clone();
        wrong[key] = value;
        bad.push(wrong);
    }
    for data in bad {
        assert!(
            config_event(5, "profile_applied", data.clone())
                .profile_applied()
                .is_err(),
            "{data} must not decode as profile_applied"
        );
    }
}

#[test]
fn apply_profile_flows_through_session_correlation_and_sequence() {
    let mut session = DesktopSession::new();
    let ask = session
        .issue_apply_profile("team", ProfileApplyMode::Default, None)
        .unwrap();
    assert_eq!(ask.request_id(), "desktop-1");
    assert_eq!(
        session.pending(),
        Some(("desktop-1", DesktopMethod::ApplyProfile))
    );
    assert!(matches!(
        session.issue_apply_profile("team", ProfileApplyMode::Default, Some("3f1c")),
        Err(DesktopRpcError::RequestInFlight { .. })
    ));
    let answered = session
        .accept(&widget(&response_line("desktop-1", confirmation_data())))
        .unwrap()
        .unwrap();
    let ApplyProfileOutcome::ConfirmationRequired(confirmation) =
        record_response(answered).apply_profile().unwrap().outcome
    else {
        panic!("a confirmation was asked")
    };
    assert!(session.pending().is_none());

    // The human approved: the same profile and mode, with the token.
    let confirmed = session
        .issue_apply_profile(
            "team",
            ProfileApplyMode::Default,
            Some(confirmation.token.as_str()),
        )
        .unwrap();
    assert_eq!(confirmed.request_id(), "desktop-2");
    assert_eq!(
        confirmed.payload(),
        request_payload(
            "desktop-2",
            "apply_profile",
            json!({ "profile": "team", "mode": "default", "confirmation": "3f1c" }),
        )
    );
    session
        .accept(&widget(&error_line(
            "desktop-2",
            "profile_apply_busy",
            "busy",
        )))
        .unwrap();
    assert!(session.pending().is_none());

    let event = session
        .accept(&widget(&event_line(
            7,
            "profile_applied",
            json!({ "profile": "team", "mode": "default", "source": "desktop" }),
        )))
        .unwrap()
        .unwrap();
    assert_eq!(
        record_event(event).profile_applied().unwrap().profile,
        "team"
    );
    assert!(matches!(
        session.accept(&widget(&event_line(
            9,
            "profile_applied",
            json!({ "profile": "team", "mode": "default", "source": "desktop" }),
        ))),
        Err(DesktopRpcError::OutOfOrderSequence { .. })
    ));
}

#[test]
fn capabilities_expose_the_profile_apply_method_and_event() {
    let capabilities = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(&["capabilities", "apply_profile"], &["profile_applied"]),
        )))
        .unwrap()
        .unwrap(),
    )
    .capabilities()
    .unwrap();
    assert!(capabilities.supports(DesktopMethod::ApplyProfile));
    assert!(capabilities.emits("profile_applied"));

    let older = record_response(
        decode_widget(&widget(&response_line(
            "desktop-1",
            capabilities_data(&["capabilities", "get_config", "set_config"], &[]),
        )))
        .unwrap()
        .unwrap(),
    )
    .capabilities()
    .unwrap();
    assert!(!older.supports(DesktopMethod::ApplyProfile));
    assert!(!older.emits("profile_applied"));
}
