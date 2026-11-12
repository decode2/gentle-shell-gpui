//! Client adapter for `gentle-shell.desktop-rpc/v1`.
//!
//! The desktop app talks to Gentle Shell's extension, not to a session: each
//! call is a standard Pi RPC prompt whose message is `/gentle:desktop <json>`,
//! where `<json>` is the inner Desktop request. Pi answers that prompt with a
//! plain `response` record — `disposition: "handled"` only means the request
//! was accepted — and the domain answer arrives separately as an
//! `extension_ui_request` with method `setWidget`, widget key
//! `gentle-desktop-rpc` and the JSON record in `widgetLines`.
//!
//! This module owns the wire format and the correlation rules, no transport:
//! it builds the outer command to hand to [`crate::PiProcess::request`] and
//! decodes the widget records that arrive in [`crate::Incoming::Ui`]. The Pi
//! request `id` is assigned by the transport and never identifies a Desktop
//! request; correlation uses the inner `requestId` only.
//!
//! [`DesktopSession`] also owns the two recovery paths the protocol documents:
//! [`DesktopSession::abandon`] for a request that will never be answered (a
//! transport timeout, or a disposition other than `handled`), and
//! [`DesktopSession::reset`] / [`DesktopSession::resync_from_snapshot`] for a
//! backend restart or a lost event sequence.

use crate::record::{Response as PiResponse, UiMethod, UiRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt;

/// Schema of the inner request, response and event records.
pub const SCHEMA: &str = "gentle-shell.desktop-rpc/v1";
/// Slash command that carries an inner request to Gentle Shell's extension.
pub const PROMPT_COMMAND: &str = "/gentle:desktop";
/// Widget key the Desktop records are published under.
pub const WIDGET_KEY: &str = "gentle-desktop-rpc";
/// Largest inner request accepted, in bytes of its JSON encoding.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Largest inner `requestId`, in UTF-16 code units — the unit the server
/// counts, so non-BMP characters use two.
pub const MAX_REQUEST_ID_UNITS: usize = 128;

/// The inner methods this client knows. Anything else is a server or
/// protocol change and is refused instead of guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DesktopMethod {
    Capabilities,
    GetSnapshot,
    GetPermissionMode,
    SetPermissionMode,
}

impl DesktopMethod {
    /// The wire name of the method.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::GetSnapshot => "get_snapshot",
            Self::GetPermissionMode => "get_permission_mode",
            Self::SetPermissionMode => "set_permission_mode",
        }
    }

    /// Parse a wire method name.
    pub fn parse(method: &str) -> Result<Self, DesktopRpcError> {
        match method {
            "capabilities" => Ok(Self::Capabilities),
            "get_snapshot" => Ok(Self::GetSnapshot),
            "get_permission_mode" => Ok(Self::GetPermissionMode),
            "set_permission_mode" => Ok(Self::SetPermissionMode),
            other => Err(DesktopRpcError::UnsupportedMethod(other.to_string())),
        }
    }
}

impl fmt::Display for DesktopMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for DesktopMethod {
    type Err = DesktopRpcError;

    fn from_str(method: &str) -> Result<Self, Self::Err> {
        Self::parse(method)
    }
}

/// The permission modes the wire defines. The wire name is lowercase snake
/// case; `super-yolo` is the only mode with a hyphen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionMode {
    Normal,
    Auto,
    Yolo,
    SuperYolo,
}

impl PermissionMode {
    /// Every mode the protocol speaks, in wire order.
    pub const ALL: [Self; 4] = [Self::Normal, Self::Auto, Self::Yolo, Self::SuperYolo];

    /// The wire name of the mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Auto => "auto",
            Self::Yolo => "yolo",
            Self::SuperYolo => "super-yolo",
        }
    }
}

impl serde::Serialize for PermissionMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl std::str::FromStr for PermissionMode {
    type Err = DesktopRpcError;

    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "normal" => Ok(Self::Normal),
            "auto" => Ok(Self::Auto),
            "yolo" => Ok(Self::Yolo),
            "super-yolo" => Ok(Self::SuperYolo),
            other => Err(DesktopRpcError::InvalidPermissionMode(other.to_string())),
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One validated inner request, ready to be sent as a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRequest {
    request_id: String,
    method: DesktopMethod,
    payload: String,
}

#[derive(serde::Serialize)]
struct WireRequest<'a> {
    schema: &'a str,
    #[serde(rename = "requestId")]
    request_id: &'a str,
    method: &'a str,
    params: &'a Value,
}

/// The typed params of `set_permission_mode`; `persist` is omitted rather than
/// sent as `null`, so the server's documented default applies.
#[derive(serde::Serialize)]
struct SetPermissionParams {
    mode: PermissionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    persist: Option<bool>,
}

/// The wire `params` object for `set_permission_mode`.
fn set_permission_mode_params(
    mode: PermissionMode,
    persist: Option<bool>,
) -> Result<Value, DesktopRpcError> {
    serde_json::to_value(SetPermissionParams { mode, persist })
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))
}

impl DesktopRequest {
    /// The `capabilities` request. It takes no params.
    pub fn capabilities(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::Capabilities, json!({}))
    }

    /// The `get_snapshot` request. It takes no params.
    pub fn get_snapshot(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::GetSnapshot, json!({}))
    }

    /// The `get_permission_mode` request. It takes no params.
    pub fn get_permission_mode(request_id: impl Into<String>) -> Result<Self, DesktopRpcError> {
        Self::new(request_id, DesktopMethod::GetPermissionMode, json!({}))
    }

    /// The `set_permission_mode` request. `persist` is `None` to let the
    /// server default it to `false`, or `Some` to send an explicit flag.
    pub fn set_permission_mode(
        request_id: impl Into<String>,
        mode: PermissionMode,
        persist: Option<bool>,
    ) -> Result<Self, DesktopRpcError> {
        Self::new(
            request_id,
            DesktopMethod::SetPermissionMode,
            set_permission_mode_params(mode, persist)?,
        )
    }

    /// A request for a known method, validated against the id and size limits
    /// before anything reaches the transport.
    pub fn new(
        request_id: impl Into<String>,
        method: DesktopMethod,
        params: Value,
    ) -> Result<Self, DesktopRpcError> {
        let request_id = request_id.into();
        let units = request_id.encode_utf16().count();
        if units == 0 || units > MAX_REQUEST_ID_UNITS {
            return Err(DesktopRpcError::InvalidRequestId { units });
        }
        if !params.is_object() {
            return Err(DesktopRpcError::ParamsNotObject);
        }
        let payload = serde_json::to_string(&WireRequest {
            schema: SCHEMA,
            request_id: &request_id,
            method: method.as_str(),
            params: &params,
        })
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
        if payload.len() > MAX_REQUEST_BYTES {
            return Err(DesktopRpcError::RequestTooLarge {
                bytes: payload.len(),
                limit: MAX_REQUEST_BYTES,
            });
        }
        Ok(Self {
            request_id,
            method,
            payload,
        })
    }

    /// The inner `requestId`, the only correlation key of this protocol.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The method being called.
    pub fn method(&self) -> DesktopMethod {
        self.method
    }

    /// The inner request as the JSON document sent inside the prompt.
    pub fn payload(&self) -> &str {
        &self.payload
    }

    /// The exact prompt message: `/gentle:desktop <json>`.
    pub fn prompt_message(&self) -> String {
        format!("{PROMPT_COMMAND} {}", self.payload)
    }

    /// The `prompt` command to hand to the JSONL transport. It carries no
    /// `id`: the transport assigns the Pi request id, which is not the
    /// Desktop `requestId`.
    pub fn prompt_command(&self) -> Value {
        json!({ "type": "prompt", "message": self.prompt_message() })
    }
}

/// The server-reported failure of an inner request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopErrorInfo {
    pub code: String,
    pub message: String,
}

/// A decoded inner response. `requestId` is kept so the caller can prove the
/// correlation instead of trusting arrival order; it is absent only for the
/// server's `requestId: null` report of a request it could not parse.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopResponse {
    request_id: Option<String>,
    result: Result<Value, DesktopErrorInfo>,
}

impl DesktopResponse {
    /// The inner `requestId` this response answers, when the server had one.
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Whether the server answered with data instead of an error.
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }

    /// The error the server reported, if it failed.
    pub fn error(&self) -> Option<&DesktopErrorInfo> {
        self.result.as_ref().err()
    }

    /// The response to `capabilities`.
    pub fn capabilities(&self) -> Result<Capabilities, DesktopRpcError> {
        Capabilities::from_data(self.data()?)
    }

    /// The response to `get_snapshot`.
    pub fn snapshot(&self) -> Result<Snapshot, DesktopRpcError> {
        Snapshot::from_data(self.data()?)
    }

    /// The response to `get_permission_mode`.
    pub fn permission_mode(&self) -> Result<PermissionModeState, DesktopRpcError> {
        PermissionModeState::from_data(self.data()?)
    }

    /// The response to `set_permission_mode`.
    pub fn set_permission_mode(&self) -> Result<SetPermissionModeResult, DesktopRpcError> {
        SetPermissionModeResult::from_data(self.data()?)
    }

    fn data(&self) -> Result<&Value, DesktopRpcError> {
        self.result
            .as_ref()
            .map_err(|info| DesktopRpcError::RemoteError(info.clone()))
    }
}

/// One inner event. `seq` is session-scoped and advances by one.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopEvent {
    pub seq: u64,
    pub event: String,
    pub data: Value,
}

impl DesktopEvent {
    /// Decode the data of a `permission_mode_changed` event. Like the other
    /// typed accessors it decodes the data as-is; an event of another name
    /// simply fails to fit the shape.
    pub fn permission_mode_changed(&self) -> Result<PermissionModeChanged, DesktopRpcError> {
        PermissionModeChanged::from_data(&self.data)
    }
}

/// A record decoded from the Desktop widget.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopRecord {
    Response(DesktopResponse),
    Event(DesktopEvent),
}

/// The decoded `capabilities` data: what the extension offers and how it
/// describes itself on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Inner methods the extension implements.
    pub methods: Vec<String>,
    /// Events it publishes.
    pub events: Vec<String>,
    /// The schema it declares, when it declares one.
    pub schema: Option<String>,
    /// The slash command it declares, when it declares one.
    pub command: Option<String>,
    /// The widget key it declares, when it declares one.
    pub widget: Option<String>,
}

impl Capabilities {
    /// Whether the peer offers `method`.
    pub fn supports(&self, method: DesktopMethod) -> bool {
        self.methods.iter().any(|name| name == method.as_str())
    }

    /// Whether the peer publishes the `event` name.
    pub fn emits(&self, event: &str) -> bool {
        self.events.iter().any(|name| name == event)
    }

    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let schema = text_field(data, "schema")?;
        expect_field("schema", SCHEMA, &schema)?;
        let command = text_field(data, "command")?;
        expect_field("command", PROMPT_COMMAND, &command)?;
        let widget = text_field(data, "widget")?;
        expect_field("widget", WIDGET_KEY, &widget)?;
        Ok(Self {
            methods: name_list(data, "methods")?,
            events: name_list(data, "events")?,
            schema,
            command,
            widget,
        })
    }
}

/// An optional string field of a data document.
fn text_field(data: &Value, field: &'static str) -> Result<Option<String>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be a string"
        ))),
    }
}

/// Refuse a declared wire value the client does not speak.
fn expect_field(
    field: &'static str,
    expected: &'static str,
    got: &Option<String>,
) -> Result<(), DesktopRpcError> {
    match got {
        Some(got) if got != expected => Err(DesktopRpcError::UnexpectedValue {
            field,
            expected,
            got: got.clone(),
        }),
        _ => Ok(()),
    }
}

/// A required array of names from a data document.
fn name_list(data: &Value, field: &'static str) -> Result<Vec<String>, DesktopRpcError> {
    let items = data.get(field).and_then(Value::as_array).ok_or_else(|| {
        DesktopRpcError::MalformedPayload(format!("capabilities response has no {field} array"))
    })?;
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        let name = item.as_str().ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("capabilities {field} must be strings"))
        })?;
        names.push(name.to_string());
    }
    Ok(names)
}

/// A required permission mode field of a data document.
fn permission_mode_field(
    data: &Value,
    field: &'static str,
) -> Result<PermissionMode, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a mode string")))?
        .parse()
}

/// A required boolean field of a data document.
fn bool_field(data: &Value, field: &'static str) -> Result<bool, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a boolean")))
}

/// A required string field of a data document.
fn string_field(data: &Value, field: &'static str) -> Result<String, DesktopRpcError> {
    data.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} must be a string")))
}

/// An optional `{ code, message }` error object of a data document, such as
/// the `persist_error` of a `set_permission_mode` response.
fn optional_error_field(
    data: &Value,
    field: &'static str,
) -> Result<Option<DesktopErrorInfo>, DesktopRpcError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => {
            let code = object
                .get("code")
                .and_then(Value::as_str)
                .filter(|code| !code.is_empty())
                .ok_or_else(|| DesktopRpcError::MalformedPayload(format!("{field} has no code")))?;
            let message = object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(Some(DesktopErrorInfo {
                code: code.to_string(),
                message: message.to_string(),
            }))
        }
        Some(_) => Err(DesktopRpcError::MalformedPayload(format!(
            "{field} must be an error object"
        ))),
    }
}

/// The decoded `get_snapshot` data.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub sequence: u64,
    pub state: Value,
}

impl Snapshot {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let schema = text_field(data, "schema")?;
        expect_field("schema", SCHEMA, &schema)?;
        let sequence = data
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| DesktopRpcError::MalformedPayload("snapshot has no sequence".into()))?;
        let state = data
            .get("state")
            .filter(|state| state.is_object())
            .cloned()
            .ok_or_else(|| {
                DesktopRpcError::MalformedPayload("snapshot has no state object".into())
            })?;
        Ok(Self { sequence, state })
    }
}

/// The live permission state read by `get_permission_mode`: the session mode
/// in effect now and the trusted default a new primary session would start
/// from. Both are the server's actual state, never an echo of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionModeState {
    pub mode: PermissionMode,
    pub default_mode: PermissionMode,
}

impl PermissionModeState {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            mode: permission_mode_field(data, "mode")?,
            default_mode: permission_mode_field(data, "default_mode")?,
        })
    }
}

/// The actual state after `set_permission_mode`. `mode` and `default_mode`
/// are re-read from the owner; `persisted` reports whether the request saved
/// the default; `changed` reports whether either value moved. `persist_error`
/// is present only when a requested save failed, and the live change stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPermissionModeResult {
    pub mode: PermissionMode,
    pub default_mode: PermissionMode,
    pub persisted: bool,
    pub changed: bool,
    pub persist_error: Option<DesktopErrorInfo>,
}

impl SetPermissionModeResult {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            mode: permission_mode_field(data, "mode")?,
            default_mode: permission_mode_field(data, "default_mode")?,
            persisted: bool_field(data, "persisted")?,
            changed: bool_field(data, "changed")?,
            persist_error: optional_error_field(data, "persist_error")?,
        })
    }
}

/// The data of a `permission_mode_changed` event: the change the server just
/// applied. `source` is `desktop` because this slice only publishes changes
/// it made itself; other session changes are visible through
/// `get_permission_mode` but do not emit this event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionModeChanged {
    pub mode: PermissionMode,
    pub previous_mode: PermissionMode,
    pub default_mode: PermissionMode,
    pub persisted: bool,
    pub source: String,
}

impl PermissionModeChanged {
    fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            mode: permission_mode_field(data, "mode")?,
            previous_mode: permission_mode_field(data, "previous_mode")?,
            default_mode: permission_mode_field(data, "default_mode")?,
            persisted: bool_field(data, "persisted")?,
            source: string_field(data, "source")?,
        })
    }
}

/// Pi's answer to the outer prompt: `disposition: "handled"` means accepted.
/// The domain result is never here, it arrives as a widget record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    pub disposition: String,
}

impl Handoff {
    /// Read the outer `response` record for a Desktop prompt.
    pub fn from_pi_response(response: &PiResponse) -> Result<Self, DesktopRpcError> {
        if !response.success {
            return Err(DesktopRpcError::OuterFailed {
                error: response.error.clone(),
            });
        }
        let disposition = response
            .data
            .get("disposition")
            .and_then(Value::as_str)
            .map(str::to_string);
        match disposition.as_deref() {
            // Only `handled` is acceptance. `started` and `queued` describe an
            // intermediate step, so the caller keeps waiting for the widget
            // record (or abandons the request when its transport times out).
            Some("handled") => Ok(Self {
                disposition: "handled".to_string(),
            }),
            _ => Err(DesktopRpcError::UnexpectedDisposition { disposition }),
        }
    }

    /// Whether Pi accepted the request. It says nothing about the outcome.
    pub fn is_handled(&self) -> bool {
        self.disposition == "handled"
    }
}

/// A session-scoped client: it hands out request ids, keeps one request in
/// flight and enforces the event sequence, so a caller never confuses a
/// response with an event or an old answer with a new one.
///
/// The session is not cloneable on purpose: a copy would hand out the same
/// request ids for different requests. Create one session per backend
/// connection and keep it in the owner of that connection.
#[derive(Debug)]
pub struct DesktopSession {
    next_request_id: u64,
    pending: Option<Pending>,
    last_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    request_id: String,
    method: DesktopMethod,
}

impl Default for DesktopSession {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopSession {
    /// A session whose first request id is `desktop-1`.
    pub fn new() -> Self {
        Self {
            next_request_id: 1,
            pending: None,
            last_seq: None,
        }
    }

    /// The request still awaiting its response, with its method.
    pub fn pending(&self) -> Option<(&str, DesktopMethod)> {
        self.pending
            .as_ref()
            .map(|pending| (pending.request_id.as_str(), pending.method))
    }

    /// The last accepted event sequence.
    pub fn last_sequence(&self) -> Option<u64> {
        self.last_seq
    }

    /// A new request with a fresh id. Only one request may be in flight:
    /// responses are correlated by id, but a serial client keeps the failure
    /// modes obvious.
    pub fn issue(&mut self, method: DesktopMethod) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(method, json!({}))
    }

    /// Issue `set_permission_mode` with its typed params. `get_permission_mode`
    /// has no params and is issued with [`Self::issue`].
    pub fn issue_set_permission_mode(
        &mut self,
        mode: PermissionMode,
        persist: Option<bool>,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        self.issue_with(
            DesktopMethod::SetPermissionMode,
            set_permission_mode_params(mode, persist)?,
        )
    }

    /// A new request with a fresh id; `params` is built by the typed callers
    /// above, never from app-supplied JSON.
    fn issue_with(
        &mut self,
        method: DesktopMethod,
        params: Value,
    ) -> Result<DesktopRequest, DesktopRpcError> {
        if let Some(pending) = &self.pending {
            return Err(DesktopRpcError::RequestInFlight {
                request_id: pending.request_id.clone(),
            });
        }
        let request =
            DesktopRequest::new(format!("desktop-{}", self.next_request_id), method, params)?;
        self.next_request_id += 1;
        self.pending = Some(Pending {
            request_id: request.request_id().to_string(),
            method,
        });
        Ok(request)
    }

    /// Give up on the request in flight and return its id: the recovery for a
    /// transport timeout, a cancelled or reloaded session, or an outer
    /// disposition other than `handled`. A late answer to it is refused as
    /// uncorrelated instead of being applied to the next request.
    pub fn abandon(&mut self) -> Option<String> {
        self.pending.take().map(|pending| pending.request_id)
    }

    /// Forget the in-flight request and the event baseline: the backend was
    /// replaced (restart, reload) and neither its answers nor its sequence
    /// apply any more. Ids keep advancing, so a late answer from the previous
    /// backend can never match a new request.
    pub fn reset(&mut self) {
        self.pending = None;
        self.last_seq = None;
    }

    /// Adopt a snapshot's sequence as the event baseline, the documented
    /// recovery after an [`DesktopRpcError::OutOfOrderSequence`] gap. A
    /// snapshot older than the last accepted event is refused, so a stale one
    /// cannot rewind the session into replays.
    pub fn resync_from_snapshot(&mut self, snapshot: &Snapshot) -> Result<(), DesktopRpcError> {
        if let Some(last) = self.last_seq.filter(|last| snapshot.sequence < *last) {
            return Err(DesktopRpcError::StaleSnapshot {
                last,
                sequence: snapshot.sequence,
            });
        }
        self.last_seq = Some(snapshot.sequence);
        Ok(())
    }

    /// Decode and correlate an `extension_ui_request`: `Ok(None)` when it is
    /// not this protocol's widget, an error when it is one but unusable or
    /// unexpected, otherwise the record.
    pub fn accept(
        &mut self,
        request: &UiRequest,
    ) -> Result<Option<DesktopRecord>, DesktopRpcError> {
        let Some(record) = decode_widget(request)? else {
            return Ok(None);
        };
        match &record {
            DesktopRecord::Response(response) => {
                let expected = self
                    .pending
                    .as_ref()
                    .map(|pending| pending.request_id.clone());
                let got = response.request_id().map(str::to_string);
                if got.is_some() && got == expected {
                    self.pending = None;
                } else if got.is_none() && !response.is_ok() && expected.is_some() {
                    // A null `requestId` is the server reporting a request it
                    // could not parse. With one request in flight it can only
                    // answer that one, and it must not stay pending forever.
                    self.pending = None;
                } else {
                    return Err(DesktopRpcError::UncorrelatedResponse { expected, got });
                }
            }
            DesktopRecord::Event(event) => match self.last_seq {
                Some(last) if event.seq <= last => {
                    return Err(DesktopRpcError::DuplicateSequence {
                        last,
                        got: event.seq,
                    });
                }
                Some(last) if event.seq != last + 1 => {
                    return Err(DesktopRpcError::OutOfOrderSequence {
                        expected: last + 1,
                        got: event.seq,
                    });
                }
                // The first event seen sets the baseline: the session does not
                // know how many events preceded its subscription.
                _ => self.last_seq = Some(event.seq),
            },
        }
        Ok(Some(record))
    }
}

/// Decode one inner JSON document.
pub fn decode_payload(payload: &str) -> Result<DesktopRecord, DesktopRpcError> {
    let value: Value = serde_json::from_str(payload)
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
    if !value.is_object() {
        return Err(DesktopRpcError::MalformedPayload(
            "record must be a JSON object".into(),
        ));
    }
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("record has no schema".into()))?;
    if schema != SCHEMA {
        return Err(DesktopRpcError::ForeignSchema(schema.to_string()));
    }
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| DesktopRpcError::MalformedPayload("record has no kind".into()))?;
    match kind {
        "response" => decode_response(value),
        "event" => decode_event(value),
        other => Err(DesktopRpcError::UnexpectedKind(other.to_string())),
    }
}

/// Extract the record from the Desktop widget. UI requests of other methods or
/// keys, and a cleared widget, are not this protocol and decode to `None`.
pub fn decode_widget(request: &UiRequest) -> Result<Option<DesktopRecord>, DesktopRpcError> {
    let UiMethod::SetWidget { key, lines, .. } = &request.method else {
        return Ok(None);
    };
    if key != WIDGET_KEY {
        return Ok(None);
    }
    let Some(lines) = lines.as_ref().filter(|lines| !lines.is_empty()) else {
        return Ok(None);
    };
    match lines.as_slice() {
        [line] => decode_payload(line).map(Some),
        lines => Err(DesktopRpcError::MalformedPayload(format!(
            "desktop widget carries {} lines, expected 1",
            lines.len()
        ))),
    }
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    ok: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<RawError>,
}

#[derive(Deserialize)]
struct RawError {
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct RawEvent {
    seq: Option<u64>,
    event: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

fn decode_response(value: Value) -> Result<DesktopRecord, DesktopRpcError> {
    let raw: RawResponse = serde_json::from_value(value)
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
    // `requestId: null` is documented for a request the server could not
    // parse; an empty id cannot correlate either, so both decode to `None`.
    let request_id = raw.request_id.filter(|request_id| !request_id.is_empty());
    let label = request_id.as_deref().unwrap_or("<no requestId>");
    let ok = raw
        .ok
        .ok_or_else(|| DesktopRpcError::MalformedPayload("response has no ok".into()))?;
    let result: Result<Value, DesktopErrorInfo> = if ok {
        let data = raw.data.ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("response {label} has no data"))
        })?;
        Ok(data)
    } else {
        let error = raw.error.ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("response {label} has no error"))
        })?;
        let code = error.code.filter(|code| !code.is_empty()).ok_or_else(|| {
            DesktopRpcError::MalformedPayload(format!("response {label} error has no code"))
        })?;
        Err(DesktopErrorInfo {
            code,
            message: error.message.unwrap_or_default(),
        })
    };
    Ok(DesktopRecord::Response(DesktopResponse {
        request_id,
        result,
    }))
}

fn decode_event(value: Value) -> Result<DesktopRecord, DesktopRpcError> {
    let raw: RawEvent = serde_json::from_value(value)
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?;
    let seq = raw
        .seq
        .ok_or_else(|| DesktopRpcError::MalformedPayload("event has no seq".into()))?;
    let event = raw
        .event
        .filter(|event| !event.is_empty())
        .ok_or_else(|| DesktopRpcError::MalformedPayload("event has no name".into()))?;
    let data = raw
        .data
        .ok_or_else(|| DesktopRpcError::MalformedPayload("event has no data".into()))?;
    Ok(DesktopRecord::Event(DesktopEvent { seq, event, data }))
}

/// Everything that can go wrong before, while and after a request: request
/// validation, decoding and correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopRpcError {
    /// The inner `requestId` is empty or longer than
    /// [`MAX_REQUEST_ID_UNITS`] UTF-16 code units; `units` is its length.
    InvalidRequestId { units: usize },
    /// A method this client does not implement.
    UnsupportedMethod(String),
    /// A permission mode the wire does not define.
    InvalidPermissionMode(String),
    /// `params` is not a JSON object.
    ParamsNotObject,
    /// The encoded inner request exceeds [`MAX_REQUEST_BYTES`].
    RequestTooLarge { bytes: usize, limit: usize },
    /// A second request was issued before the first was answered.
    RequestInFlight { request_id: String },
    /// The record belongs to another schema.
    ForeignSchema(String),
    /// The envelope `kind` is neither `response` nor `event`.
    UnexpectedKind(String),
    /// A JSON document that is not the documented shape.
    MalformedPayload(String),
    /// A data document declares a wire value this client does not speak.
    UnexpectedValue {
        field: &'static str,
        expected: &'static str,
        got: String,
    },
    /// No request with that id is awaiting an answer. `got` is `None` when the
    /// record carried no `requestId`.
    UncorrelatedResponse {
        expected: Option<String>,
        got: Option<String>,
    },
    /// An event sequence already seen or already passed.
    DuplicateSequence { last: u64, got: u64 },
    /// An event sequence that skipped numbers, so events were lost.
    OutOfOrderSequence { expected: u64, got: u64 },
    /// A snapshot older than the last accepted event, which would rewind the
    /// session into replays.
    StaleSnapshot { last: u64, sequence: u64 },
    /// The server answered the inner request with an error.
    RemoteError(DesktopErrorInfo),
    /// Pi rejected the outer prompt.
    OuterFailed { error: Option<String> },
    /// Pi answered the prompt with a disposition other than `handled`.
    UnexpectedDisposition { disposition: Option<String> },
}

impl fmt::Display for DesktopRpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequestId { units } => write!(
                formatter,
                "desktop request id must be 1 to {MAX_REQUEST_ID_UNITS} UTF-16 code units, got {units}"
            ),
            Self::UnsupportedMethod(method) => {
                write!(formatter, "unsupported desktop method: {method}")
            }
            Self::InvalidPermissionMode(mode) => {
                write!(formatter, "unknown desktop permission mode: {mode}")
            }
            Self::ParamsNotObject => {
                formatter.write_str("desktop request params must be an object")
            }
            Self::RequestTooLarge { bytes, limit } => write!(
                formatter,
                "desktop request is {bytes} bytes, over the {limit} byte limit"
            ),
            Self::RequestInFlight { request_id } => {
                write!(
                    formatter,
                    "desktop request {request_id} is still unanswered"
                )
            }
            Self::ForeignSchema(schema) => {
                write!(formatter, "not a {SCHEMA} record: {schema}")
            }
            Self::UnexpectedKind(kind) => {
                write!(formatter, "unknown desktop record kind: {kind}")
            }
            Self::MalformedPayload(reason) => {
                write!(formatter, "malformed desktop record: {reason}")
            }
            Self::UnexpectedValue {
                field,
                expected,
                got,
            } => write!(
                formatter,
                "desktop data declares {field} {got}, expected {expected}"
            ),
            Self::UncorrelatedResponse { expected, got } => {
                let got = got.as_deref().unwrap_or("<no requestId>");
                match expected {
                    Some(expected) => write!(
                        formatter,
                        "desktop response answers {got}, but {expected} is pending"
                    ),
                    None => write!(formatter, "desktop response {got} answers no request"),
                }
            }
            Self::DuplicateSequence { last, got } => write!(
                formatter,
                "desktop event sequence {got} was already consumed (last {last})"
            ),
            Self::OutOfOrderSequence { expected, got } => write!(
                formatter,
                "desktop event sequence {got} skipped ahead, expected {expected}"
            ),
            Self::StaleSnapshot { last, sequence } => write!(
                formatter,
                "desktop snapshot sequence {sequence} is older than the last event {last}"
            ),
            Self::RemoteError(info) => {
                write!(formatter, "desktop {}: {}", info.code, info.message)
            }
            Self::OuterFailed { error } => match error {
                Some(error) => write!(formatter, "prompt failed: {error}"),
                None => formatter.write_str("prompt failed"),
            },
            Self::UnexpectedDisposition { disposition } => match disposition {
                Some(disposition) => write!(
                    formatter,
                    "prompt was not handled, disposition: {disposition}"
                ),
                None => formatter.write_str("prompt response carried no disposition"),
            },
        }
    }
}

impl std::error::Error for DesktopRpcError {}
