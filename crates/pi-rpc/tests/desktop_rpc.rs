//! Wire-format and correlation tests for the Gentle Desktop RPC client
//! adapter (`gentle-shell.desktop-rpc/v1`). No process, network or UI: the
//! adapter only builds the outer prompt and decodes the widget records.

use pi_rpc::desktop_rpc::{
    DesktopAgent, DesktopAgentStep, DesktopAgentUsage, DesktopEvent, DesktopJob, DesktopMethod,
    DesktopRecord, DesktopRequest, DesktopResponse, DesktopRpcError, DesktopSession, Handoff,
    JobExitCode, MAX_REQUEST_BYTES, PROMPT_COMMAND, PermissionMode, PermissionModeChanged,
    PermissionModeState, SCHEMA, SetPermissionModeResult, WIDGET_KEY, decode_payload,
    decode_widget,
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
