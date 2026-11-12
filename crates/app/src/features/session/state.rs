//! Groups of [`super::SessionView`] fields that always change together.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pi_rpc::{PiProcess, UiMethod};
use serde_json::Value;

use super::view;
use crate::features::git::GitSnapshot;
use gentle_core::tools::TodoTask;

/// Thinking levels a reasoning model has before the backend lists its own
/// (`xhigh` / `max` appear only for models that support them).
const DEFAULT_THINKING_LEVELS: [&str; 5] = ["off", "minimal", "low", "medium", "high"];

pub(super) enum BackendStatus {
    Starting,
    Ready(Arc<PiProcess>),
    Failed(String),
    Exited(Option<i32>),
}

/// The `gentle-shell --mode rpc` process and what waits on its start.
pub(super) struct Backend {
    pub(super) status: BackendStatus,
    /// The backend was started at least once (tabs start when first shown).
    pub(super) started: bool,
    pub(super) version: Option<String>,
    /// Its last stderr lines, shown when it exits.
    pub(super) stderr: VecDeque<String>,
    /// Sent once the backend is up (`--prompt`).
    pub(super) pending_prompt: Option<String>,
    /// Resumed once the backend is up (a session opened from another project).
    pub(super) pending_session: Option<PathBuf>,
    /// A new Gentle Shell was installed mid-turn: restart on it once idle.
    pub(super) restart_pending: bool,
}

impl Backend {
    pub(super) fn new(prompt: Option<String>) -> Self {
        Self {
            status: BackendStatus::Starting,
            started: false,
            version: None,
            stderr: VecDeque::new(),
            pending_prompt: prompt,
            pending_session: None,
            restart_pending: false,
        }
    }

    pub(super) fn is_ready(&self) -> bool {
        matches!(self.status, BackendStatus::Ready(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ModelInfo {
    pub(super) provider: String,
    pub(super) id: String,
    pub(super) name: String,
    pub(super) context_window: u64,
    pub(super) reasoning: bool,
    /// `input` lists `"image"` (assumed when Pi does not say).
    pub(super) images: bool,
}

impl ModelInfo {
    pub(super) fn parse(value: &Value) -> Option<Self> {
        let id = value["id"].as_str()?;
        if id == "unknown" {
            return None;
        }
        Some(Self {
            provider: value["provider"].as_str().unwrap_or_default().to_string(),
            id: id.to_string(),
            name: value["name"].as_str().unwrap_or(id).to_string(),
            context_window: value["contextWindow"].as_u64().unwrap_or(0),
            reasoning: value["reasoning"].as_bool().unwrap_or(false),
            images: value["input"]
                .as_array()
                .is_none_or(|input| input.iter().any(|kind| kind == "image")),
        })
    }
}

/// The session's model, the ones it can switch to, and its thinking level.
pub(super) struct Models {
    pub(super) current: Option<ModelInfo>,
    pub(super) available: Vec<ModelInfo>,
    /// `get_available_models` answered since the backend started.
    pub(super) loaded: bool,
    /// Bumped whenever `available` is replaced.
    pub(super) rev: u64,
    pub(super) thinking: String,
    /// What the current model accepts (`get_available_thinking_levels`), low
    /// to high; the effort slider has one stop per level.
    pub(super) thinking_levels: Vec<String>,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            current: None,
            available: Vec::new(),
            loaded: false,
            rev: 0,
            thinking: "off".into(),
            thinking_levels: DEFAULT_THINKING_LEVELS
                .iter()
                .map(|l| l.to_string())
                .collect(),
        }
    }
}

/// Context and cost of the session, and its compaction.
#[derive(Default)]
pub(super) struct Usage {
    pub(super) context_percent: Option<f64>,
    pub(super) cost: f64,
    /// A compaction (manual or automatic) is running.
    pub(super) compacting: bool,
    /// `get_state.autoCompactionEnabled`.
    pub(super) auto_compaction: Option<bool>,
    /// Context usage crossed the alert threshold.
    pub(super) context_latch: crate::features::alerts::ContextLatch,
}

pub(super) struct Dialog {
    pub(super) id: String,
    pub(super) method: UiMethod,
}

/// What extensions put on screen through UI requests.
#[derive(Default)]
pub(super) struct ExtensionUi {
    /// Dialogs waiting for an answer; the front one shows.
    pub(super) dialogs: VecDeque<Dialog>,
    pub(super) statuses: BTreeMap<String, String>,
    pub(super) widgets: BTreeMap<String, Vec<String>>,
    /// Rate-limit usage extensions reported, by window (`seven_day`).
    pub(super) rate_limits: BTreeMap<String, view::RateLimit>,
    /// When each warning toast was last shown, to drop repeats.
    pub(super) recent_toasts: HashMap<String, Instant>,
}

impl ExtensionUi {
    /// False when the same message toasted within the last ten minutes.
    pub(super) fn first_toast(&mut self, message: &str) -> bool {
        const WINDOW: Duration = Duration::from_secs(600);
        self.recent_toasts.retain(|_, at| at.elapsed() < WINDOW);
        if self.recent_toasts.contains_key(message) {
            return false;
        }
        self.recent_toasts
            .insert(message.to_string(), Instant::now());
        true
    }
}

/// What the agent is doing besides the conversation: its phase, queued
/// messages, background jobs and helpers.
#[derive(Default)]
pub(super) struct TurnActivity {
    pub(super) phase: Option<&'static str>,
    pub(super) queued: usize,
    pub(super) jobs: Vec<gentle_core::gentle_state::Job>,
    /// `bash_background`/`monitor` commands by tool call, to label their jobs.
    pub(super) job_commands: HashMap<String, String>,
    /// Helpers (`gentle-agents` widget).
    pub(super) agents: Option<gentle_core::activity::Activity>,
}

/// The task list and its island's fold (see [`view`] for the morph).
pub(super) struct TodoIsland {
    pub(super) tasks: Vec<TodoTask>,
    /// The island is unfolded.
    pub(super) open: bool,
    /// Morph progress at `motion_at`, and where it heads (0 folded, 1 open).
    pub(super) motion_from: f32,
    pub(super) motion_to: f32,
    pub(super) motion_at: Instant,
}

impl Default for TodoIsland {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            open: false,
            motion_from: 0.0,
            motion_to: 0.0,
            motion_at: Instant::now(),
        }
    }
}

impl TodoIsland {
    /// Replace the task list. An emptied list folds the island away.
    pub(super) fn apply(&mut self, tasks: Vec<TodoTask>) {
        if tasks.is_empty() {
            self.open = false;
            self.motion_from = 0.0;
            self.motion_to = 0.0;
        }
        self.tasks = tasks;
    }
}

/// The working tree's state and the pacing of its refreshes.
#[derive(Default)]
pub(super) struct GitStatus {
    pub(super) snapshot: Option<GitSnapshot>,
    /// When the working tree was last refreshed.
    pub(super) at: Option<Instant>,
    /// Bumped per refresh; a deferred refresh runs only if none ran since.
    pub(super) seq: u64,
    /// A deferred refresh is waiting.
    pub(super) scheduled: bool,
}

/// Gentle Desktop's RPC channel (`gentle-shell.desktop-rpc/v1`) in one tab.
///
/// The protocol is a standard Pi prompt whose answer is a widget record, so the
/// tab owns one [`DesktopSession`] per backend and this keeps what the channel
/// has: which request is in flight, what the extension offers, the last
/// snapshot, and the recovery paths. It is pure state on purpose: the tab only
/// sends what these calls return and feeds back what arrives.
pub(in crate::features::session) mod desktop {
    use pi_rpc::UiMethod;
    use pi_rpc::UiRequest;
    use pi_rpc::desktop_rpc::{
        Capabilities, DesktopMethod, DesktopRecord, DesktopRequest, DesktopRpcError,
        DesktopSession, PROMPT_COMMAND, Snapshot, WIDGET_KEY,
    };

    /// A request the tab must hand to its backend, with the generation of the
    /// pending timeout that guards it: a timer only expires its own generation.
    #[derive(Debug)]
    pub(in crate::features::session) struct Send {
        pub(in crate::features::session) request: DesktopRequest,
        pub(in crate::features::session) generation: u64,
    }

    /// What one decision produced: the requests to send, and a failure the tab
    /// shows through its report path.
    #[derive(Default)]
    pub(in crate::features::session) struct Applied {
        pub(in crate::features::session) sends: Vec<Send>,
        pub(in crate::features::session) report: Option<Report>,
    }

    /// Why the channel stopped or misbehaved; the tab shows it once, as a
    /// warning line in the conversation, and never retries in a loop.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(in crate::features::session) enum Report {
        /// Pi lists no `gentle:desktop` command: the extension is not there.
        MissingExtension,
        /// Pi listed it but did not accept the prompt.
        NotHandled,
        /// The capabilities record arrived but could not be read.
        Capabilities(DesktopRpcError),
        /// The snapshot record arrived but could not be read.
        Snapshot(DesktopRpcError),
    }

    impl Report {
        /// What the conversation says about it.
        pub(in crate::features::session) fn message(&self) -> String {
            match self {
                Self::MissingExtension => {
                    "This Gentle Shell does not provide Gentle Desktop. Update gentle-pi, then reload the tab.".to_string()
                }
                Self::NotHandled => {
                    format!("Gentle Shell did not run {PROMPT_COMMAND}. Is gentle-pi up to date?")
                }
                Self::Capabilities(error) => format!(
                    "Gentle Desktop's capabilities could not be read ({error}); its channel stays off in this tab."
                ),
                Self::Snapshot(error) => format!(
                    "Gentle Desktop's snapshot could not be read ({error}); its channel stays off in this tab."
                ),
            }
        }
    }

    /// Whether Pi's command list has the extension's command. Names come
    /// without the slash the prompt uses; both spellings are accepted.
    pub(in crate::features::session) fn registers_desktop(commands: &[(String, String)]) -> bool {
        let command = PROMPT_COMMAND.trim_start_matches('/');
        commands
            .iter()
            .any(|(name, _)| name.trim_start_matches('/') == command)
    }

    /// Whether a UI request carries this protocol's records. Those are
    /// correlated here instead of being stored as a widget.
    pub(in crate::features::session) fn is_desktop_widget(request: &UiRequest) -> bool {
        matches!(&request.method, UiMethod::SetWidget { key, .. } if key == WIDGET_KEY)
    }

    /// Where the channel is on this backend.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(in crate::features::session) enum Step {
        /// Nothing asked yet: the next `ready` decides.
        Idle,
        Capabilities,
        Snapshot,
        Ready,
        /// This backend cannot serve the channel: no extension, or it answered
        /// something this client does not read. Nothing more is asked of it.
        Unavailable,
    }

    pub(in crate::features::session) struct Desktop {
        session: DesktopSession,
        capabilities: Option<Capabilities>,
        snapshot: Option<Snapshot>,
        /// The newest sequence the tab's state accounts for: a snapshot covers
        /// its own, and each accepted event advances it. What an event means
        /// belongs to the domain slices, so the stored snapshot is never
        /// rewritten from one.
        covered: Option<u64>,
        step: Step,
        /// Bumped per request, so an old timer cannot abandon a newer one.
        generation: u64,
    }

    impl Default for Desktop {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Desktop {
        pub(in crate::features::session) fn new() -> Self {
            Self {
                session: DesktopSession::new(),
                capabilities: None,
                snapshot: None,
                covered: None,
                step: Step::Idle,
                generation: 0,
            }
        }

        /// The backend was replaced or exited: nothing the old one said or
        /// numbered applies. Request ids keep advancing, so a late answer from
        /// it can never match a new request.
        pub(in crate::features::session) fn reset(&mut self) {
            self.session.reset();
            self.capabilities = None;
            self.snapshot = None;
            self.covered = None;
            self.step = Step::Idle;
            self.generation += 1;
        }

        /// The backend is ready and Pi listed (or did not list) the extension's
        /// command: the check that keeps a Desktop record from ever being sent
        /// as a prompt to a Shell that cannot read it.
        ///
        /// `registered` false sends nothing and reports once; a repeated `ready`
        /// neither re-asks nor moves the step before a request really went out.
        pub(in crate::features::session) fn ready(&mut self, registered: bool) -> Applied {
            if self.step != Step::Idle {
                // Already probing, ready, or known unusable on this backend.
                return Applied::default();
            }
            if !registered {
                self.step = Step::Unavailable;
                return Applied {
                    sends: Vec::new(),
                    report: Some(Report::MissingExtension),
                };
            }
            Applied {
                sends: self.issue(DesktopMethod::Capabilities, Step::Capabilities),
                report: None,
            }
        }

        /// Pi answered the outer `prompt`. Only `handled` means the request was
        /// accepted; anything else drops it instead of waiting forever.
        ///
        /// `generation` is the one its [`Send`] carried: a late answer to a
        /// prompt whose widget record already moved the channel on must not
        /// drop the request that is pending now.
        pub(in crate::features::session) fn outer(
            &mut self,
            generation: u64,
            handled: bool,
        ) -> Applied {
            if handled || generation != self.generation || self.session.pending().is_none() {
                return Applied::default();
            }
            self.abandon();
            Applied {
                sends: Vec::new(),
                report: Some(Report::NotHandled),
            }
        }

        /// One incoming UI request; what it makes the tab send or report.
        pub(in crate::features::session) fn ui(&mut self, request: &UiRequest) -> Applied {
            // The adapter clears the pending request while correlating, so the
            // method it answered is read first.
            let awaiting = self.session.pending().map(|(_, method)| method);
            match self.session.accept(request) {
                Ok(Some(DesktopRecord::Response(response))) => match awaiting {
                    Some(DesktopMethod::Capabilities) => match response.capabilities() {
                        Ok(capabilities) => {
                            self.capabilities = Some(capabilities);
                            Applied {
                                sends: self.issue(DesktopMethod::GetSnapshot, Step::Snapshot),
                                report: None,
                            }
                        }
                        Err(error) => self.unavailable(Report::Capabilities(error)),
                    },
                    Some(DesktopMethod::GetSnapshot) => match response.snapshot() {
                        Ok(snapshot) => {
                            // A snapshot sets (or keeps) the event baseline; an
                            // older one must not rewind the session into replays.
                            if self.session.resync_from_snapshot(&snapshot).is_ok() {
                                self.covered = Some(snapshot.sequence);
                                self.snapshot = Some(snapshot);
                            }
                            self.step = Step::Ready;
                            Applied::default()
                        }
                        Err(error) => self.unavailable(Report::Snapshot(error)),
                    },
                    None => Applied::default(),
                },
                Ok(Some(DesktopRecord::Event(event))) => {
                    self.covered = Some(event.seq);
                    Applied::default()
                }
                // Not this protocol's widget.
                Ok(None) => Applied::default(),
                // Events were lost: resync on a fresh snapshot instead of
                // refusing every record from here on.
                Err(DesktopRpcError::OutOfOrderSequence { .. }) => Applied {
                    sends: self.resync(),
                    report: None,
                },
                // Late, repeated, uncorrelated or malformed: not our request any
                // more, and never a reason to break the tab's other messages.
                Err(_) => Applied::default(),
            }
        }

        /// The pending timeout for `generation` fired.
        pub(in crate::features::session) fn expire(&mut self, generation: u64) -> Vec<Send> {
            if generation != self.generation || self.session.pending().is_none() {
                return Vec::new();
            }
            self.abandon();
            Vec::new()
        }

        /// What the extension offers; kept for the domain slices that follow.
        #[allow(dead_code)]
        pub(in crate::features::session) fn capabilities(&self) -> Option<&Capabilities> {
            self.capabilities.as_ref()
        }

        /// The last accepted snapshot, as the server sent it; kept for the
        /// domain slices that follow. Cf. [`Self::covered`].
        #[allow(dead_code)]
        pub(in crate::features::session) fn snapshot(&self) -> Option<&Snapshot> {
            self.snapshot.as_ref()
        }

        /// The newest sequence this tab accounts for.
        #[allow(dead_code)]
        pub(in crate::features::session) fn covered(&self) -> Option<u64> {
            self.covered
        }

        #[cfg(test)]
        pub(in crate::features::session) fn step(&self) -> Step {
            self.step
        }

        /// Stop asking this backend and say why once: the channel is unusable
        /// here until the backend is replaced.
        fn unavailable(&mut self, report: Report) -> Applied {
            self.session.abandon();
            self.step = Step::Unavailable;
            Applied {
                sends: Vec::new(),
                report: Some(report),
            }
        }

        /// Ask for the state again after a gap in the events.
        fn resync(&mut self) -> Vec<Send> {
            // One request at a time: an answer already on its way brings the
            // sequence forward, and the next gap asks once more.
            if self.session.pending().is_some() {
                return Vec::new();
            }
            self.issue(DesktopMethod::GetSnapshot, Step::Snapshot)
        }

        fn abandon(&mut self) {
            self.session.abandon();
            self.step = Step::Idle;
            self.generation += 1;
        }

        /// One request; the step follows only when the channel really asked.
        fn issue(&mut self, method: DesktopMethod, step: Step) -> Vec<Send> {
            let Ok(request) = self.session.issue(method) else {
                // One request in flight already, and it owns the step.
                return Vec::new();
            };
            self.generation += 1;
            self.step = step;
            vec![Send {
                request,
                generation: self.generation,
            }]
        }
    }
}
#[cfg(test)]
mod tests {
    use pi_rpc::UiMethod;
    use pi_rpc::UiRequest;
    use pi_rpc::desktop_rpc::{DesktopMethod, PROMPT_COMMAND, SCHEMA, WIDGET_KEY};
    use serde_json::{Value, json};

    use super::desktop::{self, Desktop, Report, Step};
    use super::{ExtensionUi, ModelInfo, TodoIsland};
    use gentle_core::tools::TodoTask;

    #[test]
    fn models_parse_with_defaults_and_skip_unknown() {
        assert_eq!(ModelInfo::parse(&json!({"id": "unknown"})), None);
        assert_eq!(ModelInfo::parse(&json!({"name": "no id"})), None);
        let model = ModelInfo::parse(&json!({"id": "m", "provider": "p"})).unwrap();
        assert_eq!(model.name, "m");
        assert_eq!(model.context_window, 0);
        assert!(!model.reasoning);
        assert!(model.images, "image input is assumed when Pi does not say");
        let text_only = json!({"id": "m", "input": ["text"]});
        assert!(!ModelInfo::parse(&text_only).unwrap().images);
    }

    #[test]
    fn emptied_task_lists_fold_the_island() {
        let mut todo = TodoIsland {
            open: true,
            motion_from: 0.4,
            motion_to: 1.0,
            ..TodoIsland::default()
        };
        todo.apply(vec![TodoTask {
            title: "Ship".into(),
            status: "pending".into(),
            note: None,
        }]);
        assert!(todo.open, "a new list keeps the island as it was");
        assert_eq!(todo.tasks.len(), 1);

        todo.apply(Vec::new());
        assert!(!todo.open);
        assert_eq!((todo.motion_from, todo.motion_to), (0.0, 0.0));
        assert!(todo.tasks.is_empty());
    }

    #[test]
    fn repeated_toasts_show_once() {
        let mut ui = ExtensionUi::default();
        assert!(ui.first_toast("Usage is high"));
        assert!(!ui.first_toast("Usage is high"));
        assert!(ui.first_toast("Something else"));
    }

    // Gentle Desktop's channel: the records below are the wire documents the
    // extension publishes under the widget, hand-built so no process runs.

    fn widget(key: &str, lines: Option<Vec<String>>) -> UiRequest {
        UiRequest {
            id: "ui-1".into(),
            timeout: None,
            method: UiMethod::SetWidget {
                key: key.into(),
                lines,
                placement: None,
            },
        }
    }

    fn record(value: Value) -> Vec<String> {
        vec![value.to_string()]
    }

    fn capabilities_lines(request_id: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {
                "schema": SCHEMA,
                "command": PROMPT_COMMAND,
                "widget": WIDGET_KEY,
                "methods": ["capabilities", "get_snapshot"],
                "events": ["state_changed"],
            },
        }))
    }

    /// Capabilities this client cannot read: `methods` is not a list.
    fn unreadable_capabilities_lines(request_id: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {
                "schema": SCHEMA,
                "command": PROMPT_COMMAND,
                "widget": WIDGET_KEY,
                "methods": "nope",
                "events": [],
            },
        }))
    }

    fn snapshot_lines(request_id: &str, sequence: u64) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {"schema": SCHEMA, "sequence": sequence, "state": {}},
        }))
    }

    /// A snapshot without its sequence: the record decodes, the data does not.
    fn unreadable_snapshot_lines(request_id: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {"schema": SCHEMA, "state": {}},
        }))
    }

    fn event_lines(seq: u64) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": seq,
            "event": "state_changed",
            "data": {},
        }))
    }

    /// The Desktop widget, as Pi delivers a record.
    fn desktop_widget(lines: Vec<String>) -> UiRequest {
        widget(WIDGET_KEY, Some(lines))
    }

    /// A command list as `get_commands` reports it: names carry no slash.
    fn commands(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .map(|name| ((*name).to_string(), String::new()))
            .collect()
    }

    /// A tab whose discovery and probe already reached `sequence`.
    fn probed(sequence: u64) -> Desktop {
        let mut desktop = Desktop::new();
        let probe = desktop.ready(true);
        let _ = desktop.outer(probe.sends[0].generation, true);
        let snapshot = desktop.ui(&desktop_widget(capabilities_lines("desktop-1")));
        let _ = desktop.outer(snapshot.sends[0].generation, true);
        let _ = desktop.ui(&desktop_widget(snapshot_lines("desktop-2", sequence)));
        desktop
    }

    #[test]
    fn only_a_registered_extension_command_opens_the_channel() {
        assert!(desktop::registers_desktop(&commands(&["gentle:desktop"])));
        // Pi may list it with the slash the prompt uses; both are the command.
        assert!(desktop::registers_desktop(&commands(&["/gentle:desktop"])));
        assert!(!desktop::registers_desktop(&[]));
        assert!(!desktop::registers_desktop(&commands(&["gentle:mode"])));
        assert!(!desktop::registers_desktop(&commands(&["gentle:desktop2"])));
    }

    #[test]
    fn no_prompt_is_sent_without_the_extension() {
        let mut desktop = Desktop::new();
        let applied = desktop.ready(false);
        assert!(
            applied.sends.is_empty(),
            "the record JSON must never become a prompt"
        );
        assert!(matches!(applied.report, Some(Report::MissingExtension)));
        assert_eq!(desktop.step(), Step::Unavailable);

        // Reported once per backend, never retried in a loop.
        let again = desktop.ready(false);
        assert!(again.sends.is_empty());
        assert!(again.report.is_none());
        let with_extension = desktop.ready(true);
        assert!(with_extension.sends.is_empty());
        assert!(with_extension.report.is_none());
    }

    #[test]
    fn ready_probes_only_once_discovery_listed_the_command() {
        let mut desktop = Desktop::new();
        let applied = desktop.ready(true);
        assert_eq!(applied.sends.len(), 1, "one request at a time");
        assert_eq!(
            applied.sends[0].request.method(),
            DesktopMethod::Capabilities
        );
        assert!(applied.report.is_none());
        assert_eq!(desktop.step(), Step::Capabilities);

        let command = applied.sends[0].request.prompt_command();
        assert_eq!(command["type"], "prompt");
        assert!(
            command.get("id").is_none(),
            "the Pi request id belongs to the transport, not to Desktop"
        );
        assert!(
            command["message"]
                .as_str()
                .unwrap()
                .starts_with(PROMPT_COMMAND)
        );
    }

    #[test]
    fn a_repeated_ready_keeps_the_step_and_sends_nothing() {
        let mut desktop = Desktop::new();
        let _ = desktop.ready(true);
        assert!(desktop.ready(true).sends.is_empty());
        assert_eq!(
            desktop.step(),
            Step::Capabilities,
            "the probe is still pending"
        );

        // While the snapshot is in flight, Ready must not claim to probe again.
        let _ = desktop.ui(&desktop_widget(capabilities_lines("desktop-1")));
        assert_eq!(desktop.step(), Step::Snapshot);
        assert!(desktop.ready(true).sends.is_empty());
        assert_eq!(desktop.step(), Step::Snapshot);
    }

    #[test]
    fn capabilities_then_snapshot_complete_the_probe() {
        let mut desktop = Desktop::new();
        let probe = desktop.ready(true);
        let probe_generation = probe.sends[0].generation;

        // Pi accepted the prompt: nothing new until the widget record lands.
        assert!(desktop.outer(probe_generation, true).sends.is_empty());

        let applied = desktop.ui(&desktop_widget(capabilities_lines("desktop-1")));
        assert_eq!(
            applied.sends.len(),
            1,
            "the snapshot follows the capabilities"
        );
        assert_eq!(
            applied.sends[0].request.method(),
            DesktopMethod::GetSnapshot
        );
        assert_eq!(applied.sends[0].request.request_id(), "desktop-2");
        assert!(applied.report.is_none());
        assert!(desktop.capabilities().is_some());
        assert_eq!(desktop.step(), Step::Snapshot);
        let snapshot_generation = applied.sends[0].generation;

        assert!(desktop.outer(snapshot_generation, true).sends.is_empty());
        let applied = desktop.ui(&desktop_widget(snapshot_lines("desktop-2", 5)));
        assert!(applied.sends.is_empty());
        assert_eq!(
            desktop.snapshot().map(|snapshot| snapshot.sequence),
            Some(5)
        );
        assert_eq!(desktop.step(), Step::Ready);
        assert_eq!(desktop.covered(), Some(5), "a snapshot covers its sequence");
    }

    #[test]
    fn the_outer_prompt_id_never_identifies_a_desktop_request() {
        let mut desktop = Desktop::new();
        let _ = desktop.ready(true);
        // A record carrying the transport's own id answers nothing here.
        assert!(
            desktop
                .ui(&desktop_widget(snapshot_lines("gs-1", 1)))
                .sends
                .is_empty()
        );
        assert_eq!(desktop.step(), Step::Capabilities);
        // The real answer still correlates, so the probe is not stuck.
        assert_eq!(
            desktop
                .ui(&desktop_widget(capabilities_lines("desktop-1")))
                .sends
                .len(),
            1
        );
    }

    #[test]
    fn an_unhandled_prompt_abandons_and_reports() {
        let mut desktop = Desktop::new();
        let probe = desktop.ready(true);
        let generation = probe.sends[0].generation;

        let applied = desktop.outer(generation, false);
        assert!(applied.sends.is_empty());
        assert!(matches!(applied.report, Some(Report::NotHandled)));
        assert_eq!(desktop.step(), Step::Idle);

        // Its answer, if it ever arrives, changes nothing.
        assert!(
            desktop
                .ui(&desktop_widget(capabilities_lines("desktop-1")))
                .sends
                .is_empty()
        );
        assert!(desktop.capabilities().is_none());
        assert_eq!(desktop.step(), Step::Idle);
    }

    #[test]
    fn an_outer_failure_for_an_older_prompt_drops_nothing_newer() {
        let mut desktop = Desktop::new();
        let probe = desktop.ready(true);
        let stale_generation = probe.sends[0].generation;
        // The capabilities record already arrived, so a snapshot is in flight
        // when the old prompt's failure lands.
        let snapshot = desktop.ui(&desktop_widget(capabilities_lines("desktop-1")));
        assert_eq!(snapshot.sends.len(), 1);

        let applied = desktop.outer(stale_generation, false);
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert_eq!(desktop.step(), Step::Snapshot);
        // The snapshot still answers, so the probe is not stuck.
        assert!(
            desktop
                .ui(&desktop_widget(snapshot_lines("desktop-2", 1)))
                .sends
                .is_empty()
        );
        assert_eq!(desktop.step(), Step::Ready);
    }

    #[test]
    fn only_its_own_timeout_abandons_a_request() {
        let mut desktop = Desktop::new();
        let first = desktop.ready(true);
        let first_generation = first.sends[0].generation;

        // A timer from before this request is stale.
        assert!(desktop.expire(first_generation.wrapping_sub(1)).is_empty());
        assert!(desktop.outer(first_generation, true).sends.is_empty());
        assert_eq!(desktop.step(), Step::Capabilities);

        // Its own timer drops it: the request will never be answered.
        assert!(desktop.expire(first_generation).is_empty());
        assert_eq!(desktop.step(), Step::Idle);

        // A newer request has its own generation; the old timer cannot drop it.
        let second = desktop.ready(true);
        let second_generation = second.sends[0].generation;
        assert!(second_generation > first_generation);
        assert!(desktop.expire(first_generation).is_empty());
        assert_eq!(desktop.step(), Step::Capabilities);
        assert!(desktop.expire(second_generation).is_empty());
        assert_eq!(desktop.step(), Step::Idle);
    }

    #[test]
    fn a_replaced_backend_forgets_its_answers_and_sequences() {
        let mut desktop = probed(4);
        assert_eq!(desktop.step(), Step::Ready);
        assert_eq!(desktop.covered(), Some(4));

        desktop.reset();
        assert_eq!(desktop.step(), Step::Idle);
        assert!(desktop.capabilities().is_none());
        assert!(desktop.snapshot().is_none());
        assert!(desktop.covered().is_none());

        // What the old backend still owed arrives late: benign, not applied.
        assert!(
            desktop
                .ui(&desktop_widget(snapshot_lines("desktop-2", 4)))
                .sends
                .is_empty()
        );
        assert!(desktop.snapshot().is_none());

        // The new backend is asked with a fresh id, so old answers cannot match.
        let sends = desktop.ready(true).sends;
        assert_eq!(sends[0].request.request_id(), "desktop-3");
    }

    #[test]
    fn accepted_events_advance_the_covered_sequence_only() {
        let mut desktop = probed(5);
        let applied = desktop.ui(&desktop_widget(event_lines(6)));
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert_eq!(desktop.covered(), Some(6), "the marker advances");
        // The stored snapshot is exactly what the server sent: the payload of
        // an event belongs to the domain slices, not to this one.
        assert_eq!(
            desktop.snapshot().map(|snapshot| snapshot.sequence),
            Some(5)
        );
    }

    #[test]
    fn a_gap_in_the_events_resyncs_on_a_fresh_snapshot() {
        let mut desktop = probed(5);
        assert!(desktop.ui(&desktop_widget(event_lines(6))).sends.is_empty());

        let sends = desktop.ui(&desktop_widget(event_lines(8))).sends;
        assert_eq!(sends.len(), 1, "a gap asks for the state again");
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        let resync_id = sends[0].request.request_id().to_string();

        assert!(
            desktop
                .ui(&desktop_widget(snapshot_lines(&resync_id, 8)))
                .sends
                .is_empty()
        );
        assert_eq!(
            desktop.snapshot().map(|snapshot| snapshot.sequence),
            Some(8)
        );
        assert_eq!(desktop.covered(), Some(8));
        assert!(desktop.ui(&desktop_widget(event_lines(9))).sends.is_empty());
        // A replayed event is not a gap.
        assert!(desktop.ui(&desktop_widget(event_lines(9))).sends.is_empty());
    }

    #[test]
    fn a_stale_resync_snapshot_is_not_applied_or_retried_in_a_loop() {
        let mut desktop = probed(5);
        let _ = desktop.ui(&desktop_widget(event_lines(6)));
        let sends = desktop.ui(&desktop_widget(event_lines(8))).sends;
        let resync_id = sends[0].request.request_id().to_string();

        // Older than the accepted events: keep the baseline, do not rewind.
        assert!(
            desktop
                .ui(&desktop_widget(snapshot_lines(&resync_id, 4)))
                .sends
                .is_empty()
        );
        assert_eq!(
            desktop.snapshot().map(|snapshot| snapshot.sequence),
            Some(5)
        );
        assert_eq!(desktop.covered(), Some(6));
        assert_eq!(desktop.step(), Step::Ready);
    }

    #[test]
    fn unreadable_capabilities_are_reported_once_and_stop_the_channel() {
        let mut desktop = Desktop::new();
        let _ = desktop.ready(true);

        let applied = desktop.ui(&desktop_widget(unreadable_capabilities_lines("desktop-1")));
        assert!(applied.sends.is_empty(), "nothing is probed on broken data");
        assert!(matches!(applied.report, Some(Report::Capabilities(_))));
        assert!(!applied.report.unwrap().message().is_empty());
        assert_eq!(desktop.step(), Step::Unavailable);
        assert!(desktop.capabilities().is_none());

        // Reported once, and the channel stays off on this backend.
        let again = desktop.ui(&desktop_widget(unreadable_capabilities_lines("desktop-1")));
        assert!(again.sends.is_empty() && again.report.is_none());
        let later = desktop.ready(true);
        assert!(later.sends.is_empty() && later.report.is_none());
    }

    #[test]
    fn an_unreadable_snapshot_is_reported_once() {
        let mut desktop = Desktop::new();
        let _ = desktop.ready(true);
        let _ = desktop.ui(&desktop_widget(capabilities_lines("desktop-1")));

        let applied = desktop.ui(&desktop_widget(unreadable_snapshot_lines("desktop-2")));
        assert!(applied.sends.is_empty());
        assert!(matches!(applied.report, Some(Report::Snapshot(_))));
        assert_eq!(desktop.step(), Step::Unavailable);
        assert!(desktop.snapshot().is_none());

        let again = desktop.ui(&desktop_widget(unreadable_snapshot_lines("desktop-2")));
        assert!(again.sends.is_empty() && again.report.is_none());
    }

    #[test]
    fn late_and_foreign_records_are_benign() {
        let mut desktop = probed(3);
        // A response to nothing, a replayed event, broken JSON and a cleared
        // widget all leave the tab's state and its queue alone.
        for lines in [
            snapshot_lines("desktop-99", 9),
            event_lines(3),
            vec!["not json".to_string()],
            Vec::new(),
        ] {
            let applied = desktop.ui(&desktop_widget(lines));
            assert!(applied.sends.is_empty());
            assert!(applied.report.is_none(), "late records are not reported");
        }
        assert_eq!(desktop.step(), Step::Ready);
        assert!(desktop.snapshot().is_some());
    }

    #[test]
    fn only_the_desktop_widget_leaves_generic_widget_storage() {
        assert!(desktop::is_desktop_widget(&desktop_widget(vec![
            "not json".into()
        ])));
        // A cleared desktop widget is still control traffic, not a widget.
        assert!(desktop::is_desktop_widget(&widget(WIDGET_KEY, None)));
        // Every other widget keeps the handling it had.
        assert!(!desktop::is_desktop_widget(&widget(
            "gentle-agents",
            Some(vec!["{}".into()])
        )));
        assert!(!desktop::is_desktop_widget(&widget("usage", None)));
        assert!(!desktop::is_desktop_widget(&UiRequest {
            id: "ui-2".into(),
            timeout: None,
            method: UiMethod::SetStatus {
                key: WIDGET_KEY.into(),
                text: Some("x".into()),
            },
        }));
    }
}
