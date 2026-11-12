//! Wire-format and correlation tests for the Gentle Desktop RPC client
//! adapter (`gentle-shell.desktop-rpc/v1`). No process, network or UI: the
//! adapter only builds the outer prompt and decodes the widget records.

use pi_rpc::desktop_rpc::{
    DesktopEvent, DesktopMethod, DesktopRecord, DesktopRequest, DesktopResponse, DesktopRpcError,
    DesktopSession, Handoff, MAX_REQUEST_BYTES, PROMPT_COMMAND, SCHEMA, WIDGET_KEY, decode_payload,
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
