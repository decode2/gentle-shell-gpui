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
        Capabilities, DesktopAgent, DesktopJob, DesktopMethod, DesktopRecord, DesktopRequest,
        DesktopRpcError, DesktopSession, PROMPT_COMMAND, Snapshot, WIDGET_KEY,
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
        /// A gap arrived while a permission request was in flight: one
        /// `get_snapshot` is owed once that request settles or rolls back.
        pending_resync: bool,
        /// Live subagents and background jobs over the same channel.
        supervision: supervision::Agents,
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
                pending_resync: false,
                supervision: supervision::Agents::default(),
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
            self.pending_resync = false;
            self.supervision.reset();
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
        /// drop the request that is pending now. Because the permission slice
        /// borrows this same session, a late disposition also never drops a
        /// permission request this channel did not issue.
        pub(in crate::features::session) fn outer(
            &mut self,
            generation: u64,
            handled: bool,
        ) -> Applied {
            if handled || generation != self.generation || !self.owns_pending() {
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
                            self.capabilities = Some(capabilities.clone());
                            self.supervision.set_capabilities(capabilities);
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
                    // The permission and supervision methods are served by
                    // their own slices; this channel only probes and sequences.
                    Some(DesktopMethod::GetPermissionMode)
                    | Some(DesktopMethod::SetPermissionMode)
                    | Some(DesktopMethod::ListSubagents)
                    | Some(DesktopMethod::ListJobs)
                    | Some(DesktopMethod::CancelSubagent)
                    | Some(DesktopMethod::CancelJob) => Applied::default(),
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
            if generation != self.generation || !self.owns_pending() {
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

        /// The shared [`DesktopSession`] the permission slice borrows, so its
        /// requests and the channel's probes share one id space and one
        /// pending slot. The wiring routes each record to exactly one slice.
        pub(in crate::features::session) fn session(&mut self) -> &mut DesktopSession {
            &mut self.session
        }

        /// The structured subagents, once seeded; empty until then.
        pub(in crate::features::session) fn subagents(&self) -> &[DesktopAgent] {
            self.supervision.subagents()
        }

        /// The structured jobs, once seeded; empty until then.
        pub(in crate::features::session) fn jobs(&self) -> &[DesktopJob] {
            self.supervision.jobs()
        }

        /// Whether the structured subagent list is the preferred source.
        pub(in crate::features::session) fn subagents_ready(&self) -> bool {
            self.supervision.subagents_ready()
        }

        /// Whether the structured job list is the preferred source.
        pub(in crate::features::session) fn jobs_ready(&self) -> bool {
            self.supervision.jobs_ready()
        }

        /// Whether `cancel_subagent` is offered by the peer.
        pub(in crate::features::session) fn cancel_subagent_supported(&self) -> bool {
            self.supervision.cancel_subagent_supported()
        }

        /// Whether `cancel_job` is offered by the peer.
        pub(in crate::features::session) fn cancel_job_supported(&self) -> bool {
            self.supervision.cancel_job_supported()
        }

        /// Seed the subagent and job lists once the probe and any in-flight
        /// request settle.
        pub(in crate::features::session) fn supervision_seed(&mut self) -> supervision::Applied {
            let Desktop {
                session,
                supervision,
                ..
            } = self;
            supervision.seed(session)
        }

        /// One incoming UI request for the supervision slice.
        pub(in crate::features::session) fn supervision_apply(
            &mut self,
            request: &UiRequest,
        ) -> supervision::Applied {
            let Desktop {
                session,
                supervision,
                ..
            } = self;
            supervision.apply(session, request)
        }

        /// The pending timeout for a supervision request.
        pub(in crate::features::session) fn supervision_expire(
            &mut self,
            generation: u64,
        ) -> supervision::Applied {
            let Desktop {
                session,
                supervision,
                ..
            } = self;
            supervision.expire(session, generation)
        }

        /// Pi answered the outer prompt of a supervision request.
        pub(in crate::features::session) fn supervision_outer(
            &mut self,
            generation: u64,
            handled: bool,
        ) -> supervision::Applied {
            let Desktop {
                session,
                supervision,
                ..
            } = self;
            supervision.outer(session, generation, handled)
        }

        /// Issue `cancel_subagent` for the id the panel reported.
        pub(in crate::features::session) fn cancel_subagent(
            &mut self,
            id: String,
        ) -> supervision::Applied {
            let Desktop {
                session,
                supervision,
                ..
            } = self;
            supervision.cancel_subagent(session, id)
        }

        /// Issue `cancel_job` for the id the jobs chip reported.
        pub(in crate::features::session) fn cancel_job(
            &mut self,
            id: String,
        ) -> supervision::Applied {
            let Desktop {
                session,
                supervision,
                ..
            } = self;
            supervision.cancel_job(session, id)
        }

        /// The cancel target a server `unsupported_method` answered, so the
        /// wiring can keep the legacy prompt path exactly once.
        pub(in crate::features::session) fn take_cancel_target(
            &mut self,
        ) -> Option<supervision::CancelTarget> {
            self.supervision.take_cancel_target()
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
        pub(in crate::features::session) fn resync(&mut self) -> Vec<Send> {
            if self.session.pending().is_some() {
                // A request is in flight. A channel probe or resync already
                // covers the gap; a permission request owes one resync once it
                // settles or rolls back.
                if !self.owns_pending() {
                    self.pending_resync = true;
                }
                return Vec::new();
            }
            self.issue(DesktopMethod::GetSnapshot, Step::Snapshot)
        }

        /// Issue the resync deferred while a permission request was in flight.
        /// Exactly one `get_snapshot` goes out once that request settles or
        /// rolls back; nothing while another request is still pending.
        pub(in crate::features::session) fn flush_resync(&mut self) -> Vec<Send> {
            if !self.pending_resync || self.session.pending().is_some() {
                return Vec::new();
            }
            self.pending_resync = false;
            self.issue(DesktopMethod::GetSnapshot, Step::Snapshot)
        }

        /// Whether the pending request is one this channel issued (a probe or
        /// resync). The permission slice borrows the same session, so a stale
        /// probe timer must not abandon a permission request now in flight.
        fn owns_pending(&self) -> bool {
            matches!(
                self.session.pending().map(|(_, method)| method),
                Some(DesktopMethod::Capabilities) | Some(DesktopMethod::GetSnapshot)
            )
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

    /// Gentle Desktop's permission-mode state in one tab.
    ///
    /// This slice owns only the permission concern: the confirmed state the
    /// server reported and the one pending `set_permission_mode` intent. It is
    /// pure state on purpose: the tab only sends what these calls return and
    /// feeds back what arrives. It adds no consent claim — the human picker
    /// represents consent later — and never restarts or writes config. It
    /// borrows the parent's [`DesktopSession`], so probe and permission
    /// requests share one id space, one pending slot and one event baseline.
    ///
    /// Every entry point here is called by the lifecycle/rpc slice that wires
    /// the picker later, so none of them is live yet.
    #[allow(dead_code)]
    pub(in crate::features::session) mod permission {
        use pi_rpc::UiRequest;
        use pi_rpc::desktop_rpc::{
            Capabilities, DesktopErrorInfo, DesktopEvent, DesktopMethod, DesktopRecord,
            DesktopResponse, DesktopRpcError, DesktopSession, PermissionMode,
        };

        use super::Send;

        /// The wire name of the only event this slice applies.
        pub(in crate::features::session) const PERMISSION_MODE_CHANGED: &str =
            "permission_mode_changed";

        /// The confirmed (server-reported) permission state. Every field is the
        /// server's truth, never an echo of a request; `None` means the server
        /// has not said yet.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        pub(in crate::features::session) struct Confirmed {
            pub(in crate::features::session) mode: Option<PermissionMode>,
            pub(in crate::features::session) default_mode: Option<PermissionMode>,
            pub(in crate::features::session) persisted: Option<bool>,
        }

        /// The one pending `set_permission_mode` intent: the mode and persist
        /// flag the user picked. It is not consent, and it never claims the
        /// server applied anything.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(in crate::features::session) struct Requested {
            pub(in crate::features::session) mode: PermissionMode,
            pub(in crate::features::session) persist: Option<bool>,
        }

        /// What one decision produced: the requests to send, and a failure or
        /// warning the tab shows through its report path.
        #[derive(Debug, Default)]
        pub(in crate::features::session) struct Applied {
            pub(in crate::features::session) sends: Vec<Send>,
            pub(in crate::features::session) report: Option<Report>,
        }

        /// Why a permission request stopped or what it surfaced. The first two
        /// variants are the legacy-fallback signals; every other variant leaves
        /// the confirmed state unchanged.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(in crate::features::session) enum Report {
            /// The extension does not advertise the method at all.
            Unsupported,
            /// The server answered `unsupported_method` for a method its
            /// capabilities claimed.
            UnsupportedMethod,
            /// Capabilities are not known yet: the peer may still offer the
            /// method, so this is not a legacy-fallback signal.
            NotReady,
            /// The request timed out before the server answered.
            Timeout,
            /// The mode changed live, but the default could not be saved.
            PersistError(DesktopErrorInfo),
            /// A transport, server or decoding failure.
            Failure(DesktopRpcError),
            /// A gap in the event sequence: events were lost, so the channel
            /// must resync on a fresh snapshot. The record was already
            /// accepted, so the wiring must not accept it again.
            OutOfOrderSequence,
        }

        impl Report {
            /// What the conversation says about it.
            pub(in crate::features::session) fn message(&self) -> String {
                match self {
                    Self::Unsupported | Self::UnsupportedMethod => {
                        "This Gentle Shell does not support changing the permission mode from Desktop."
                            .to_string()
                    }
                    Self::NotReady => {
                        "Gentle Desktop is still loading; try the permission change again in a moment."
                            .to_string()
                    }
                    Self::Timeout => {
                        "The permission request timed out; it was not applied.".to_string()
                    }
                    Self::PersistError(error) => format!(
                        "The permission mode changed, but the new default was not saved ({}: {}).",
                        error.code, error.message
                    ),
                    Self::Failure(error) => {
                        format!("The permission mode change failed ({error}).")
                    }
                    Self::OutOfOrderSequence => {
                        "Gentle Desktop events were lost; resyncing the channel.".to_string()
                    }
                }
            }
        }

        /// Whether the pending request belongs to this slice (a permission
        /// method). The probe channel shares the session, so a stale permission
        /// timer or disposition must not abandon a probe now in flight.
        fn owns_pending(session: &DesktopSession) -> bool {
            matches!(
                session.pending().map(|(_, method)| method),
                Some(DesktopMethod::GetPermissionMode) | Some(DesktopMethod::SetPermissionMode)
            )
        }

        pub(in crate::features::session) struct Permission {
            capabilities: Option<Capabilities>,
            confirmed: Confirmed,
            requested: Option<Requested>,
            /// Bumped per request, so an old timer cannot drop a newer one.
            generation: u64,
            /// The live state was fetched once for this backend.
            seeded: bool,
        }

        impl Default for Permission {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Permission {
            pub(in crate::features::session) fn new() -> Self {
                Self {
                    capabilities: None,
                    confirmed: Confirmed::default(),
                    requested: None,
                    generation: 0,
                    seeded: false,
                }
            }

            /// Adopt the peer's capabilities once the probe completes; kept so
            /// requests and events are refused when the peer does not offer
            /// them.
            pub(in crate::features::session) fn set_capabilities(
                &mut self,
                capabilities: Capabilities,
            ) {
                self.capabilities = Some(capabilities);
            }

            /// What the extension offers, when the probe has said.
            #[allow(dead_code)]
            pub(in crate::features::session) fn capabilities(&self) -> Option<&Capabilities> {
                self.capabilities.as_ref()
            }

            /// The confirmed (server-reported) permission state.
            pub(in crate::features::session) fn confirmed(&self) -> Confirmed {
                self.confirmed
            }

            /// The pending `set_permission_mode` intent, if one is in flight.
            pub(in crate::features::session) fn requested(&self) -> Option<Requested> {
                self.requested
            }

            /// Ask the server for its live permission state.
            pub(in crate::features::session) fn get_permission_mode(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                let Some(capabilities) = &self.capabilities else {
                    return Applied {
                        report: Some(Report::NotReady),
                        ..Applied::default()
                    };
                };
                if !capabilities.supports(DesktopMethod::GetPermissionMode) {
                    return Applied {
                        report: Some(Report::Unsupported),
                        ..Applied::default()
                    };
                }
                let request = match session.issue(DesktopMethod::GetPermissionMode) {
                    Ok(request) => request,
                    Err(error) => return self.failure(error),
                };
                self.generation += 1;
                Applied {
                    sends: vec![Send {
                        request,
                        generation: self.generation,
                    }],
                    report: None,
                }
            }

            /// Fetch the server's live permission state once this backend's
            /// capabilities are known. It waits for any in-flight request to
            /// settle, so the probe and the permission slice never issue two
            /// requests at once.
            pub(in crate::features::session) fn seed(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                if self.seeded || !self.supports(DesktopMethod::GetPermissionMode) {
                    return Applied::default();
                }
                if session.pending().is_some() {
                    return Applied::default();
                }
                self.seeded = true;
                self.get_permission_mode(session)
            }

            /// Request a permission mode change. `persist` is `None` to let the
            /// server default it to `false`; the confirmed state moves only when
            /// the server answers. Consent is the caller's concern.
            pub(in crate::features::session) fn set_permission_mode(
                &mut self,
                session: &mut DesktopSession,
                mode: PermissionMode,
                persist: Option<bool>,
            ) -> Applied {
                let Some(capabilities) = &self.capabilities else {
                    return Applied {
                        report: Some(Report::NotReady),
                        ..Applied::default()
                    };
                };
                if !capabilities.supports(DesktopMethod::SetPermissionMode) {
                    return Applied {
                        report: Some(Report::Unsupported),
                        ..Applied::default()
                    };
                }
                let request = match session.issue_set_permission_mode(mode, persist) {
                    Ok(request) => request,
                    Err(error) => return self.failure(error),
                };
                self.generation += 1;
                self.requested = Some(Requested { mode, persist });
                Applied {
                    sends: vec![Send {
                        request,
                        generation: self.generation,
                    }],
                    report: None,
                }
            }

            /// Pi answered the outer `prompt`. Only `handled` means the request
            /// was accepted; anything else rolls it back instead of waiting
            /// forever. `generation` is the one its [`Send`] carried.
            pub(in crate::features::session) fn outer(
                &mut self,
                session: &mut DesktopSession,
                generation: u64,
                handled: bool,
            ) -> Applied {
                if handled || generation != self.generation || !owns_pending(session) {
                    return Applied::default();
                }
                self.rollback(session);
                Applied {
                    sends: Vec::new(),
                    report: Some(Report::Failure(DesktopRpcError::UnexpectedDisposition {
                        disposition: None,
                    })),
                }
            }

            /// One incoming UI request; applies only the records this slice owns
            /// (typed get/set responses and `permission_mode_changed` events)
            /// and leaves everything else alone.
            pub(in crate::features::session) fn apply(
                &mut self,
                session: &mut DesktopSession,
                request: &UiRequest,
            ) -> Applied {
                // The session clears the pending request while correlating, so
                // the method it answered is read first.
                let awaiting = session.pending().map(|(_, method)| method);
                match session.accept(request) {
                    Ok(Some(DesktopRecord::Response(response))) => match awaiting {
                        Some(DesktopMethod::GetPermissionMode) => self.apply_get(response),
                        Some(DesktopMethod::SetPermissionMode) => self.apply_set(response),
                        _ => Applied::default(),
                    },
                    Ok(Some(DesktopRecord::Event(event))) => self.apply_event(event),
                    // Events were lost: surface the gap so the wiring asks the
                    // channel for a fresh snapshot. The record was already
                    // accepted, so it must not be accepted again.
                    Err(DesktopRpcError::OutOfOrderSequence { .. }) => Applied {
                        report: Some(Report::OutOfOrderSequence),
                        ..Applied::default()
                    },
                    // Not this protocol's widget, or a late, repeated or
                    // malformed record: the outer channel owns resync, so this
                    // slice only refuses to apply it.
                    _ => Applied::default(),
                }
            }

            /// The pending timeout for `generation` fired. The request is
            /// dropped, and the timeout is reported rather than disappearing
            /// silently.
            pub(in crate::features::session) fn expire(
                &mut self,
                session: &mut DesktopSession,
                generation: u64,
            ) -> Applied {
                if generation != self.generation || !owns_pending(session) {
                    return Applied::default();
                }
                self.rollback(session);
                Applied {
                    sends: Vec::new(),
                    report: Some(Report::Timeout),
                }
            }

            /// Give up on the request in flight and its intent (a cancelled
            /// picker, a closed tab). The confirmed state stays as the server
            /// last reported it.
            pub(in crate::features::session) fn abandon(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                if !owns_pending(session) {
                    return Applied::default();
                }
                self.rollback(session);
                Applied::default()
            }

            /// The backend was replaced or exited: nothing it said or numbered
            /// applies. Request ids keep advancing, so a late answer can never
            /// match a new request.
            pub(in crate::features::session) fn reset(&mut self, session: &mut DesktopSession) {
                session.reset();
                self.capabilities = None;
                self.confirmed = Confirmed::default();
                self.requested = None;
                self.generation += 1;
                self.seeded = false;
            }

            /// Whether the peer offers `method`. Without capabilities nothing
            /// is offered, so nothing is requested or applied.
            fn supports(&self, method: DesktopMethod) -> bool {
                self.capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.supports(method))
            }

            /// A request could not be issued (one already in flight, or the
            /// request itself failed validation).
            fn failure(&self, error: DesktopRpcError) -> Applied {
                Applied {
                    sends: Vec::new(),
                    report: Some(Report::Failure(error)),
                }
            }

            /// Drop the request in flight and the intent behind it. The
            /// generation moves, so a late timer or disposition for the
            /// abandoned request is stale.
            fn rollback(&mut self, session: &mut DesktopSession) {
                session.abandon();
                self.requested = None;
                self.generation += 1;
            }

            /// The typed `get_permission_mode` answer: server truth.
            fn apply_get(&mut self, response: DesktopResponse) -> Applied {
                match response.permission_mode() {
                    Ok(state) => {
                        self.confirmed.mode = Some(state.mode);
                        self.confirmed.default_mode = Some(state.default_mode);
                        Applied::default()
                    }
                    Err(DesktopRpcError::RemoteError(info))
                        if info.code == "unsupported_method" =>
                    {
                        Applied {
                            report: Some(Report::UnsupportedMethod),
                            ..Applied::default()
                        }
                    }
                    Err(error) => Applied {
                        report: Some(Report::Failure(error)),
                        ..Applied::default()
                    },
                }
            }

            /// The typed `set_permission_mode` answer. `mode` and `default_mode`
            /// are server truth, never the request echoed back; `changed: false`
            /// is a no-op; `persist_error` is live success plus a warning that
            /// the default was not saved.
            fn apply_set(&mut self, response: DesktopResponse) -> Applied {
                // The request is answered either way; only a real answer moves
                // the confirmed state.
                self.requested = None;
                match response.set_permission_mode() {
                    Ok(result) => {
                        if !result.changed {
                            return Applied::default();
                        }
                        self.confirmed = Confirmed {
                            mode: Some(result.mode),
                            default_mode: Some(result.default_mode),
                            persisted: Some(result.persisted),
                        };
                        match result.persist_error {
                            Some(error) => Applied {
                                report: Some(Report::PersistError(error)),
                                ..Applied::default()
                            },
                            None => Applied::default(),
                        }
                    }
                    Err(DesktopRpcError::RemoteError(info))
                        if info.code == "unsupported_method" =>
                    {
                        Applied {
                            report: Some(Report::UnsupportedMethod),
                            ..Applied::default()
                        }
                    }
                    Err(error) => Applied {
                        report: Some(Report::Failure(error)),
                        ..Applied::default()
                    },
                }
            }

            /// A `permission_mode_changed` event, applied only when the peer
            /// advertised that exact event. Foreign names and unadvertised
            /// events never touch the confirmed state.
            fn apply_event(&mut self, event: DesktopEvent) -> Applied {
                if event.event != PERMISSION_MODE_CHANGED {
                    return Applied::default();
                }
                if !self
                    .capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.emits(PERMISSION_MODE_CHANGED))
                {
                    return Applied::default();
                }
                match event.permission_mode_changed() {
                    Ok(change) => {
                        self.confirmed = Confirmed {
                            mode: Some(change.mode),
                            default_mode: Some(change.default_mode),
                            persisted: Some(change.persisted),
                        };
                        Applied::default()
                    }
                    Err(error) => Applied {
                        report: Some(Report::Failure(error)),
                        ..Applied::default()
                    },
                }
            }
        }
    }

    /// Gentle Desktop's live subagents and background jobs in one tab.
    ///
    /// When the backend's `capabilities` advertise `list_subagents` and/or
    /// `list_jobs`, this slice seeds a full list once the probe (capabilities
    /// → snapshot) and the permission seed have settled, then reconciles
    /// `agents_update`/`jobs_update` events by id. It also owns the one
    /// `cancel_subagent`/`cancel_job` request at a time, with the same
    /// timeout/rollback discipline as the permission slice. The structured
    /// lists are the primary source once seeded; the legacy `gentle-agents`
    /// widget and `details.gentleJobs` stay as the fallback for older Shells.
    pub(in crate::features::session) mod supervision {
        use pi_rpc::UiRequest;
        use pi_rpc::desktop_rpc::{
            Capabilities, DesktopAgent, DesktopEvent, DesktopJob, DesktopMethod, DesktopRecord,
            DesktopResponse, DesktopRpcError, DesktopSession,
        };

        use super::Send;

        /// The wire name of the only subagent event this slice applies.
        pub(in crate::features::session) const AGENTS_UPDATE: &str = "agents_update";
        /// The wire name of the only job event this slice applies.
        pub(in crate::features::session) const JOBS_UPDATE: &str = "jobs_update";

        /// What one decision produced: the requests to send, and a failure or
        /// warning the tab shows through its report path.
        #[derive(Debug, Default)]
        pub(in crate::features::session) struct Applied {
            pub(in crate::features::session) sends: Vec<Send>,
            pub(in crate::features::session) report: Option<Report>,
        }

        /// Why a supervision request stopped or what it surfaced. The first two
        /// variants are the legacy-fallback signals for cancels; the rest leave
        /// the lists and the pending cancel untouched.
        #[derive(Debug, Clone, PartialEq)]
        pub(in crate::features::session) enum Report {
            /// The extension does not advertise the method at all.
            Unsupported,
            /// The server answered `unsupported_method` for a method its
            /// capabilities claimed.
            UnsupportedMethod,
            /// Capabilities are not known yet: the peer may still offer the
            /// method, so this is not a legacy-fallback signal.
            NotReady,
            /// The server answered `not_found`: the helper or job is gone.
            NotFound,
            /// The request timed out before the server answered.
            Timeout,
            /// A transport, server or decoding failure.
            Failure(DesktopRpcError),
            /// A gap in the event sequence: events were lost, so the lists must
            /// be re-seeded. The record was already accepted, so the wiring must
            /// not accept it again.
            OutOfOrderSequence,
        }

        impl Report {
            /// What the conversation says about it.
            pub(in crate::features::session) fn message(&self) -> String {
                match self {
                    Self::Unsupported | Self::UnsupportedMethod => {
                        "This Gentle Shell does not support stopping helpers and jobs from Desktop."
                            .to_string()
                    }
                    Self::NotReady => {
                        "Gentle Desktop is still loading; try again in a moment.".to_string()
                    }
                    Self::NotFound => "That helper or job is no longer running.".to_string(),
                    Self::Timeout => {
                        "The cancel request timed out; it was not applied.".to_string()
                    }
                    Self::Failure(error) => format!("The cancel request failed ({error})."),
                    Self::OutOfOrderSequence => {
                        "Gentle Desktop events were lost; resyncing helpers and jobs.".to_string()
                    }
                }
            }
        }

        /// What a cancel request asked for, so a timeout, a response, or an
        /// `unsupported_method` answer can be reconciled with the right list.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(in crate::features::session) enum CancelTarget {
            Subagent(String),
            Job(String),
        }

        impl CancelTarget {
            pub(in crate::features::session) fn id(&self) -> &str {
                match self {
                    Self::Subagent(id) | Self::Job(id) => id,
                }
            }

            pub(in crate::features::session) fn is_subagent(&self) -> bool {
                matches!(self, Self::Subagent(_))
            }
        }

        /// The structured subagent and job state, plus the one pending cancel.
        pub(in crate::features::session) struct Agents {
            capabilities: Option<Capabilities>,
            subagents: Vec<DesktopAgent>,
            jobs: Vec<DesktopJob>,
            /// A valid `list_subagents`/`agents_update` arrived for this backend.
            subagents_fetched: bool,
            /// A valid `list_jobs`/`jobs_update` arrived for this backend.
            jobs_fetched: bool,
            /// `list_subagents` failed once: stop retrying, keep the legacy widget.
            subagents_unavailable: bool,
            /// `list_jobs` failed once: stop retrying, keep `details.gentleJobs`.
            jobs_unavailable: bool,
            /// The cancel request in flight, if any.
            pending_cancel: Option<CancelTarget>,
            /// Bumped per request, so an old timer cannot drop a newer one.
            generation: u64,
        }

        impl Default for Agents {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Agents {
            pub(in crate::features::session) fn new() -> Self {
                Self {
                    capabilities: None,
                    subagents: Vec::new(),
                    jobs: Vec::new(),
                    subagents_fetched: false,
                    jobs_fetched: false,
                    subagents_unavailable: false,
                    jobs_unavailable: false,
                    pending_cancel: None,
                    generation: 0,
                }
            }

            /// Adopt the peer's capabilities once the probe completes.
            pub(in crate::features::session) fn set_capabilities(
                &mut self,
                capabilities: Capabilities,
            ) {
                self.capabilities = Some(capabilities);
            }

            /// What the extension offers, when the probe has said.
            #[allow(dead_code)]
            pub(in crate::features::session) fn capabilities(&self) -> Option<&Capabilities> {
                self.capabilities.as_ref()
            }

            /// The structured subagents, once seeded; empty until then.
            pub(in crate::features::session) fn subagents(&self) -> &[DesktopAgent] {
                &self.subagents
            }

            /// The structured jobs, once seeded; empty until then.
            pub(in crate::features::session) fn jobs(&self) -> &[DesktopJob] {
                &self.jobs
            }

            /// Whether the structured subagent list is the preferred source
            /// (the peer offers it and a list has arrived).
            pub(in crate::features::session) fn subagents_ready(&self) -> bool {
                self.supports(DesktopMethod::ListSubagents) && self.subagents_fetched
            }

            /// Whether the structured job list is the preferred source.
            pub(in crate::features::session) fn jobs_ready(&self) -> bool {
                self.supports(DesktopMethod::ListJobs) && self.jobs_fetched
            }

            /// Whether `cancel_subagent` is offered by the peer.
            pub(in crate::features::session) fn cancel_subagent_supported(&self) -> bool {
                self.supports(DesktopMethod::CancelSubagent)
            }

            /// Whether `cancel_job` is offered by the peer.
            pub(in crate::features::session) fn cancel_job_supported(&self) -> bool {
                self.supports(DesktopMethod::CancelJob)
            }

            /// The id (and kind) of the cancel a server `unsupported_method`
            /// answered, so the wiring can keep the legacy path exactly once.
            pub(in crate::features::session) fn take_cancel_target(
                &mut self,
            ) -> Option<CancelTarget> {
                self.pending_cancel.take()
            }

            /// Ask the server for its live lists once the probe and any in-flight
            /// request settle. Each list is fetched at most once per backend;
            /// events reconcile after that.
            pub(in crate::features::session) fn seed(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                if session.pending().is_some() {
                    return Applied::default();
                }
                if self.supports(DesktopMethod::ListSubagents)
                    && !self.subagents_fetched
                    && !self.subagents_unavailable
                {
                    return self.issue_list(session, DesktopMethod::ListSubagents);
                }
                if self.supports(DesktopMethod::ListJobs)
                    && !self.jobs_fetched
                    && !self.jobs_unavailable
                {
                    return self.issue_list(session, DesktopMethod::ListJobs);
                }
                Applied::default()
            }

            /// Issue `cancel_subagent` for the id the panel reported.
            pub(in crate::features::session) fn cancel_subagent(
                &mut self,
                session: &mut DesktopSession,
                id: String,
            ) -> Applied {
                self.cancel(session, DesktopMethod::CancelSubagent, id)
            }

            /// Issue `cancel_job` for the id the jobs chip reported.
            pub(in crate::features::session) fn cancel_job(
                &mut self,
                session: &mut DesktopSession,
                id: String,
            ) -> Applied {
                self.cancel(session, DesktopMethod::CancelJob, id)
            }

            /// Pi answered the outer `prompt`. Only `handled` means the request
            /// was accepted; anything else rolls it back instead of waiting.
            pub(in crate::features::session) fn outer(
                &mut self,
                session: &mut DesktopSession,
                generation: u64,
                handled: bool,
            ) -> Applied {
                if handled || generation != self.generation || !owns_pending(session) {
                    return Applied::default();
                }
                self.rollback(session);
                Applied {
                    sends: Vec::new(),
                    report: Some(Report::Failure(DesktopRpcError::UnexpectedDisposition {
                        disposition: None,
                    })),
                }
            }

            /// One incoming UI request; applies only the records this slice owns
            /// (typed list/cancel responses and `agents_update`/`jobs_update`
            /// events) and leaves everything else alone.
            pub(in crate::features::session) fn apply(
                &mut self,
                session: &mut DesktopSession,
                request: &UiRequest,
            ) -> Applied {
                // The session clears the pending request while correlating, so
                // the method it answered is read first.
                let awaiting = session.pending().map(|(_, method)| method);
                match session.accept(request) {
                    Ok(Some(DesktopRecord::Response(response))) => match awaiting {
                        Some(DesktopMethod::ListSubagents) => self.apply_list_subagents(response),
                        Some(DesktopMethod::ListJobs) => self.apply_list_jobs(response),
                        Some(DesktopMethod::CancelSubagent) => self.apply_cancel_subagent(response),
                        Some(DesktopMethod::CancelJob) => self.apply_cancel_job(response),
                        _ => Applied::default(),
                    },
                    Ok(Some(DesktopRecord::Event(event))) => self.apply_event(event),
                    // Events were lost: forget the fetched markers so the lists
                    // re-seed once the channel resyncs. The record was already
                    // accepted, so it must not be accepted again.
                    Err(DesktopRpcError::OutOfOrderSequence { .. }) => {
                        self.subagents_fetched = false;
                        self.jobs_fetched = false;
                        self.subagents_unavailable = false;
                        self.jobs_unavailable = false;
                        Applied {
                            sends: Vec::new(),
                            report: Some(Report::OutOfOrderSequence),
                        }
                    }
                    // Not this protocol's widget, or a late, repeated or
                    // malformed record: the outer channel owns resync, so this
                    // slice only refuses to apply it.
                    _ => Applied::default(),
                }
            }

            /// The pending timeout for `generation` fired. A cancel is rolled
            /// back and reported; a list fetch is silently marked unavailable
            /// so it is never retried in a loop.
            pub(in crate::features::session) fn expire(
                &mut self,
                session: &mut DesktopSession,
                generation: u64,
            ) -> Applied {
                if generation != self.generation || !owns_pending(session) {
                    return Applied::default();
                }
                let was_cancel = matches!(
                    session.pending().map(|(_, method)| method),
                    Some(DesktopMethod::CancelSubagent) | Some(DesktopMethod::CancelJob)
                );
                self.rollback(session);
                Applied {
                    sends: Vec::new(),
                    report: was_cancel.then_some(Report::Timeout),
                }
            }

            /// The backend was replaced or exited: nothing it said applies, and
            /// the lists must be re-seeded on the new backend.
            pub(in crate::features::session) fn reset(&mut self) {
                self.capabilities = None;
                self.subagents.clear();
                self.jobs.clear();
                self.subagents_fetched = false;
                self.jobs_fetched = false;
                self.subagents_unavailable = false;
                self.jobs_unavailable = false;
                self.pending_cancel = None;
                self.generation += 1;
            }

            fn supports(&self, method: DesktopMethod) -> bool {
                self.capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.supports(method))
            }

            fn emits(&self, event: &str) -> bool {
                self.capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.emits(event))
            }

            /// One `list_subagents`/`list_jobs` request.
            fn issue_list(
                &mut self,
                session: &mut DesktopSession,
                method: DesktopMethod,
            ) -> Applied {
                let request = match session.issue(method) {
                    Ok(request) => request,
                    Err(error) => {
                        return Applied {
                            sends: Vec::new(),
                            report: Some(Report::Failure(error)),
                        };
                    }
                };
                self.generation += 1;
                Applied {
                    sends: vec![Send {
                        request,
                        generation: self.generation,
                    }],
                    report: None,
                }
            }

            /// One `cancel_subagent`/`cancel_job` request.
            fn cancel(
                &mut self,
                session: &mut DesktopSession,
                method: DesktopMethod,
                id: String,
            ) -> Applied {
                if !self.supports(method) {
                    let report = if self.capabilities.is_some() {
                        Report::Unsupported
                    } else {
                        Report::NotReady
                    };
                    return Applied {
                        sends: Vec::new(),
                        report: Some(report),
                    };
                }
                let request = match method {
                    DesktopMethod::CancelSubagent => session.issue_cancel_subagent(id.as_str()),
                    DesktopMethod::CancelJob => session.issue_cancel_job(id.as_str()),
                    _ => unreachable!("cancel is only called with a cancel method"),
                };
                let request = match request {
                    Ok(request) => request,
                    Err(error) => {
                        return Applied {
                            sends: Vec::new(),
                            report: Some(Report::Failure(error)),
                        };
                    }
                };
                self.generation += 1;
                self.pending_cancel = Some(match method {
                    DesktopMethod::CancelSubagent => CancelTarget::Subagent(id),
                    DesktopMethod::CancelJob => CancelTarget::Job(id),
                    _ => unreachable!(),
                });
                Applied {
                    sends: vec![Send {
                        request,
                        generation: self.generation,
                    }],
                    report: None,
                }
            }

            fn apply_list_subagents(&mut self, response: DesktopResponse) -> Applied {
                match response.list_subagents() {
                    Ok(list) => {
                        self.subagents = list.agents;
                        self.subagents_fetched = true;
                        self.subagents_unavailable = false;
                        Applied::default()
                    }
                    Err(_) => {
                        self.subagents_unavailable = true;
                        Applied::default()
                    }
                }
            }

            fn apply_list_jobs(&mut self, response: DesktopResponse) -> Applied {
                match response.list_jobs() {
                    Ok(list) => {
                        self.jobs = list.jobs;
                        self.jobs_fetched = true;
                        self.jobs_unavailable = false;
                        Applied::default()
                    }
                    Err(_) => {
                        self.jobs_unavailable = true;
                        Applied::default()
                    }
                }
            }

            fn apply_cancel_subagent(&mut self, response: DesktopResponse) -> Applied {
                match response.cancel_subagent() {
                    Ok(result) => {
                        self.pending_cancel = None;
                        reconcile_agent(&mut self.subagents, result.agent);
                        Applied::default()
                    }
                    Err(DesktopRpcError::RemoteError(info))
                        if info.code == "unsupported_method" =>
                    {
                        // Keep the target so the wiring can fall back exactly once.
                        Applied {
                            sends: Vec::new(),
                            report: Some(Report::UnsupportedMethod),
                        }
                    }
                    Err(DesktopRpcError::RemoteError(info)) if info.code == "not_found" => {
                        self.pending_cancel = None;
                        Applied {
                            sends: Vec::new(),
                            report: Some(Report::NotFound),
                        }
                    }
                    Err(error) => {
                        self.pending_cancel = None;
                        Applied {
                            sends: Vec::new(),
                            report: Some(Report::Failure(error)),
                        }
                    }
                }
            }

            fn apply_cancel_job(&mut self, response: DesktopResponse) -> Applied {
                match response.cancel_job() {
                    Ok(result) => {
                        self.pending_cancel = None;
                        reconcile_job(&mut self.jobs, result.job);
                        Applied::default()
                    }
                    Err(DesktopRpcError::RemoteError(info))
                        if info.code == "unsupported_method" =>
                    {
                        Applied {
                            sends: Vec::new(),
                            report: Some(Report::UnsupportedMethod),
                        }
                    }
                    Err(DesktopRpcError::RemoteError(info)) if info.code == "not_found" => {
                        self.pending_cancel = None;
                        Applied {
                            sends: Vec::new(),
                            report: Some(Report::NotFound),
                        }
                    }
                    Err(error) => {
                        self.pending_cancel = None;
                        Applied {
                            sends: Vec::new(),
                            report: Some(Report::Failure(error)),
                        }
                    }
                }
            }

            /// An `agents_update`/`jobs_update` event, applied only when the peer
            /// advertised that exact name. Foreign names and unadvertised events
            /// never touch the lists. The event is a full bounded projection, so
            /// it replaces the list (missing ids drop) but never marks it
            /// fetched: only the full `list_subagents`/`list_jobs` seed does.
            fn apply_event(&mut self, event: DesktopEvent) -> Applied {
                match event.event.as_str() {
                    AGENTS_UPDATE => {
                        if !self.emits(AGENTS_UPDATE) {
                            return Applied::default();
                        }
                        match event.agents_update() {
                            Ok(list) => {
                                self.subagents = list.agents;
                                Applied::default()
                            }
                            Err(error) => Applied {
                                sends: Vec::new(),
                                report: Some(Report::Failure(error)),
                            },
                        }
                    }
                    JOBS_UPDATE => {
                        if !self.emits(JOBS_UPDATE) {
                            return Applied::default();
                        }
                        match event.jobs_update() {
                            Ok(list) => {
                                self.jobs = list.jobs;
                                Applied::default()
                            }
                            Err(error) => Applied {
                                sends: Vec::new(),
                                report: Some(Report::Failure(error)),
                            },
                        }
                    }
                    _ => Applied::default(),
                }
            }

            /// Drop the request in flight and the intent behind it. The
            /// generation moves, so a late timer or disposition is stale.
            fn rollback(&mut self, session: &mut DesktopSession) {
                let method = session.pending().map(|(_, method)| method);
                session.abandon();
                match method {
                    Some(DesktopMethod::CancelSubagent) | Some(DesktopMethod::CancelJob) => {
                        self.pending_cancel = None;
                    }
                    Some(DesktopMethod::ListSubagents) => self.subagents_unavailable = true,
                    Some(DesktopMethod::ListJobs) => self.jobs_unavailable = true,
                    _ => {}
                }
                self.generation += 1;
            }
        }

        /// Whether the pending request is one this slice issued (a list or
        /// cancel method). The probe and permission slices share the session, so
        /// a stale supervision timer must not abandon their requests.
        fn owns_pending(session: &DesktopSession) -> bool {
            matches!(
                session.pending().map(|(_, method)| method),
                Some(DesktopMethod::ListSubagents)
                    | Some(DesktopMethod::ListJobs)
                    | Some(DesktopMethod::CancelSubagent)
                    | Some(DesktopMethod::CancelJob)
            )
        }

        /// Upsert one subagent into the current list by id: replace it if
        /// present, append it if new. This is the merge used by the single-entry
        /// `cancel_subagent` response; full lists (the seed response and
        /// `agents_update` events) replace the whole list instead.
        fn reconcile_agent(current: &mut Vec<DesktopAgent>, agent: DesktopAgent) {
            if let Some(existing) = current.iter_mut().find(|entry| entry.id == agent.id) {
                *existing = agent;
            } else {
                current.push(agent);
            }
        }

        /// Upsert one job into the current list by id, the same single-entry
        /// merge as [`reconcile_agent`], used by the `cancel_job` response.
        fn reconcile_job(current: &mut Vec<DesktopJob>, job: DesktopJob) {
            if let Some(existing) = current.iter_mut().find(|entry| entry.id == job.id) {
                *existing = job;
            } else {
                current.push(job);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use pi_rpc::UiMethod;
    use pi_rpc::UiRequest;
    use pi_rpc::desktop_rpc::{
        Capabilities, DesktopMethod, DesktopSession, PROMPT_COMMAND, PermissionMode, SCHEMA,
        WIDGET_KEY,
    };
    use serde_json::{Value, json};

    use super::desktop::permission::{self, Permission, Report as PermissionReport};
    use super::desktop::supervision::{Agents, Report as SupervisionReport};
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

    // Gentle Desktop's permission slice: capabilities, responses and events
    // are built directly, so no process runs and no file is written.

    fn permission_capabilities(methods: &[&str], events: &[&str]) -> Capabilities {
        Capabilities {
            methods: methods.iter().map(|name| (*name).to_string()).collect(),
            events: events.iter().map(|name| (*name).to_string()).collect(),
            schema: Some(SCHEMA.into()),
            command: Some(PROMPT_COMMAND.into()),
            widget: Some(WIDGET_KEY.into()),
        }
    }

    /// A slice whose peer offers both permission methods and `events`, and the
    /// one session it borrows.
    fn permission_ready(events: &[&str]) -> (Permission, DesktopSession) {
        let mut permission = Permission::new();
        permission.set_capabilities(permission_capabilities(
            &["get_permission_mode", "set_permission_mode"],
            events,
        ));
        (permission, DesktopSession::new())
    }

    fn get_mode_lines(request_id: &str, mode: &str, default_mode: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {"mode": mode, "default_mode": default_mode},
        }))
    }

    fn set_mode_lines(request_id: &str, data: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": data,
        }))
    }

    fn error_lines(request_id: &str, code: &str, message: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": false,
            "error": {"code": code, "message": message},
        }))
    }

    fn permission_event_lines(seq: u64, data: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": seq,
            "event": "permission_mode_changed",
            "data": data,
        }))
    }

    fn set_result(mode: &str, default_mode: &str, persisted: bool, changed: bool) -> Value {
        json!({
            "mode": mode,
            "default_mode": default_mode,
            "persisted": persisted,
            "changed": changed,
        })
    }

    fn permission_change(
        mode: &str,
        previous_mode: &str,
        default_mode: &str,
        persisted: bool,
    ) -> Value {
        json!({
            "mode": mode,
            "previous_mode": previous_mode,
            "default_mode": default_mode,
            "persisted": persisted,
            "source": "desktop",
        })
    }

    #[test]
    fn permission_request_encodes_every_mode_and_persist() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        for mode in PermissionMode::ALL {
            let applied = permission.set_permission_mode(&mut session, mode, Some(true));
            assert_eq!(applied.sends.len(), 1);
            assert_eq!(
                applied.sends[0].request.method(),
                DesktopMethod::SetPermissionMode
            );
            let payload: Value = serde_json::from_str(applied.sends[0].request.payload()).unwrap();
            assert_eq!(payload["method"], "set_permission_mode");
            assert_eq!(payload["params"]["mode"], mode.as_str());
            assert_eq!(payload["params"]["persist"], true);

            // Answer it so the next mode's request is not refused as in-flight.
            let id = applied.sends[0].request.request_id().to_string();
            let applied = permission.apply(
                &mut session,
                &desktop_widget(set_mode_lines(
                    &id,
                    set_result(mode.as_str(), mode.as_str(), true, true),
                )),
            );
            assert!(applied.report.is_none());
        }

        // A `persist: None` request omits the flag so the server default applies.
        let applied = permission.set_permission_mode(&mut session, PermissionMode::Normal, None);
        let payload: Value = serde_json::from_str(applied.sends[0].request.payload()).unwrap();
        assert!(payload["params"].get("persist").is_none());
    }

    #[test]
    fn permission_request_tracks_the_pending_intent() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::SuperYolo, Some(false));
        assert_eq!(applied.sends.len(), 1);
        assert_eq!(
            permission.requested(),
            Some(permission::Requested {
                mode: PermissionMode::SuperYolo,
                persist: Some(false),
            })
        );
        // Nothing is confirmed until the server answers.
        assert_eq!(permission.confirmed(), permission::Confirmed::default());
    }

    #[test]
    fn permission_get_reads_the_server_state() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let applied = permission.get_permission_mode(&mut session);
        assert_eq!(applied.sends.len(), 1);
        assert_eq!(
            applied.sends[0].request.method(),
            DesktopMethod::GetPermissionMode
        );
        let id = applied.sends[0].request.request_id().to_string();

        let applied = permission.apply(
            &mut session,
            &desktop_widget(get_mode_lines(&id, "auto", "yolo")),
        );
        assert!(applied.report.is_none());
        assert_eq!(permission.confirmed().mode, Some(PermissionMode::Auto));
        assert_eq!(
            permission.confirmed().default_mode,
            Some(PermissionMode::Yolo)
        );
        assert_eq!(
            permission.confirmed().persisted,
            None,
            "get does not report whether the default is saved"
        );
    }

    #[test]
    fn permission_set_success_confirms_server_state() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        let id = applied.sends[0].request.request_id().to_string();

        let applied = permission.apply(
            &mut session,
            &desktop_widget(set_mode_lines(&id, set_result("yolo", "yolo", true, true))),
        );
        assert!(applied.report.is_none());
        assert_eq!(permission.confirmed().mode, Some(PermissionMode::Yolo));
        assert_eq!(
            permission.confirmed().default_mode,
            Some(PermissionMode::Yolo)
        );
        assert_eq!(permission.confirmed().persisted, Some(true));
        assert_eq!(
            permission.requested(),
            None,
            "the answered intent is consumed"
        );
    }

    #[test]
    fn permission_response_mode_is_server_truth_not_the_request() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let applied = permission.set_permission_mode(&mut session, PermissionMode::Normal, None);
        let id = applied.sends[0].request.request_id().to_string();

        // The server applied a different mode than the request asked for: its
        // answer wins, the request is never echoed back.
        let applied = permission.apply(
            &mut session,
            &desktop_widget(set_mode_lines(
                &id,
                set_result("super-yolo", "yolo", false, true),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(permission.confirmed().mode, Some(PermissionMode::SuperYolo));
        assert_eq!(
            permission.confirmed().default_mode,
            Some(PermissionMode::Yolo)
        );
        assert_eq!(permission.confirmed().persisted, Some(false));
    }

    #[test]
    fn permission_unchanged_set_is_a_no_op() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        let id = applied.sends[0].request.request_id().to_string();

        let applied = permission.apply(
            &mut session,
            &desktop_widget(set_mode_lines(&id, set_result("yolo", "yolo", true, false))),
        );
        assert!(applied.report.is_none());
        assert_eq!(permission.confirmed(), permission::Confirmed::default());
        assert_eq!(permission.requested(), None);
    }

    #[test]
    fn permission_persist_error_is_live_success_with_a_warning() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        let id = applied.sends[0].request.request_id().to_string();

        let mut data = set_result("yolo", "yolo", false, true);
        data["persist_error"] = json!({"code": "readonly", "message": "config is read-only"});

        let applied = permission.apply(&mut session, &desktop_widget(set_mode_lines(&id, data)));
        // Live success: the mode moved even though the default was not saved.
        assert_eq!(permission.confirmed().mode, Some(PermissionMode::Yolo));
        assert_eq!(
            permission.confirmed().default_mode,
            Some(PermissionMode::Yolo)
        );
        assert_eq!(permission.confirmed().persisted, Some(false));
        match applied.report.as_ref() {
            Some(PermissionReport::PersistError(error)) => {
                assert_eq!(error.code, "readonly");
            }
            other => panic!("expected a persist warning, got {other:?}"),
        }
        assert!(
            applied
                .report
                .as_ref()
                .unwrap()
                .message()
                .contains("not saved")
        );
    }

    #[test]
    fn permission_advertised_event_moves_state() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let applied = permission.apply(
            &mut session,
            &desktop_widget(permission_event_lines(
                1,
                permission_change("yolo", "normal", "yolo", true),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(permission.confirmed().mode, Some(PermissionMode::Yolo));
        assert_eq!(
            permission.confirmed().default_mode,
            Some(PermissionMode::Yolo)
        );
        assert_eq!(permission.confirmed().persisted, Some(true));
    }

    #[test]
    fn permission_foreign_and_unadvertised_events_do_not_move_state() {
        // A foreign event name is never applied, even when the slice could.
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);
        let applied = permission.apply(&mut session, &desktop_widget(event_lines(1)));
        assert!(applied.report.is_none());
        assert_eq!(permission.confirmed(), permission::Confirmed::default());

        // A permission event the peer never advertised is ignored too.
        let (mut unadvertised, mut unadvertised_session) = permission_ready(&["state_changed"]);
        let applied = unadvertised.apply(
            &mut unadvertised_session,
            &desktop_widget(permission_event_lines(
                1,
                permission_change("yolo", "normal", "yolo", true),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(unadvertised.confirmed(), permission::Confirmed::default());
    }

    #[test]
    fn permission_unsupported_is_distinct_from_other_failures() {
        // Missing capability: nothing is sent, and the report says legacy.
        let mut missing = Permission::new();
        missing.set_capabilities(permission_capabilities(&["get_permission_mode"], &[]));
        let mut missing_session = DesktopSession::new();
        let applied = missing.set_permission_mode(&mut missing_session, PermissionMode::Yolo, None);
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(PermissionReport::Unsupported));

        // The server answers unsupported_method: also legacy, but from the wire.
        let (mut claimed, mut claimed_session) = permission_ready(&["permission_mode_changed"]);
        let applied = claimed.set_permission_mode(&mut claimed_session, PermissionMode::Yolo, None);
        let id = applied.sends[0].request.request_id().to_string();
        let applied = claimed.apply(
            &mut claimed_session,
            &desktop_widget(error_lines(&id, "unsupported_method", "nope")),
        );
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(PermissionReport::UnsupportedMethod));
        assert_eq!(claimed.confirmed(), permission::Confirmed::default());

        // Any other failure is neither legacy signal, and state stays unchanged.
        let (mut failed, mut failed_session) = permission_ready(&["permission_mode_changed"]);
        let applied = failed.set_permission_mode(&mut failed_session, PermissionMode::Yolo, None);
        let id = applied.sends[0].request.request_id().to_string();
        let applied = failed.apply(
            &mut failed_session,
            &desktop_widget(error_lines(&id, "readonly", "nope")),
        );
        assert!(matches!(applied.report, Some(PermissionReport::Failure(_))));
        assert_eq!(failed.confirmed(), permission::Confirmed::default());
    }

    #[test]
    fn permission_timeout_rolls_back_the_intent() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);
        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        let generation = applied.sends[0].generation;

        // A timer from before this request is stale.
        assert!(
            permission
                .expire(&mut session, generation.wrapping_sub(1))
                .sends
                .is_empty()
        );
        assert!(permission.requested().is_some());

        // Its own timer rolls the intent back and leaves the state untouched.
        // Its own timer rolls the intent back, reports the timeout, and
        // leaves the confirmed state untouched.
        let applied = permission.expire(&mut session, generation);
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(PermissionReport::Timeout));
        assert!(!applied.report.as_ref().unwrap().message().is_empty());
        assert_eq!(permission.requested(), None);
        assert_eq!(permission.confirmed(), permission::Confirmed::default());
    }

    #[test]
    fn permission_abandon_rolls_back_without_a_timeout() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);
        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        assert_eq!(applied.sends.len(), 1);

        // A cancelled picker gives up the intent without a timeout, and never
        // claims the server applied anything.
        let applied = permission.abandon(&mut session);
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert_eq!(permission.requested(), None);
        assert_eq!(permission.confirmed(), permission::Confirmed::default());
    }

    #[test]
    fn permission_reset_forgets_answers_and_a_late_response_is_benign() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);
        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        let id = applied.sends[0].request.request_id().to_string();

        permission.reset(&mut session);
        assert_eq!(permission.confirmed(), permission::Confirmed::default());
        assert_eq!(permission.requested(), None);
        assert!(permission.capabilities().is_none());

        // The old answer arrives late: not applied, and never reported.
        let applied = permission.apply(
            &mut session,
            &desktop_widget(set_mode_lines(&id, set_result("yolo", "yolo", true, true))),
        );
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert_eq!(permission.confirmed(), permission::Confirmed::default());

        // A fresh request gets a new id, so old answers cannot match it.
        permission.set_capabilities(permission_capabilities(
            &["get_permission_mode", "set_permission_mode"],
            &["permission_mode_changed"],
        ));
        let applied = permission.set_permission_mode(&mut session, PermissionMode::Auto, None);
        assert_eq!(applied.sends[0].request.request_id(), "desktop-2");
    }

    #[test]
    fn permission_refuses_a_second_request_while_one_is_in_flight() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        let first = permission.set_permission_mode(&mut session, PermissionMode::Yolo, None);
        assert_eq!(first.sends.len(), 1);

        let second = permission.set_permission_mode(&mut session, PermissionMode::Normal, None);
        assert!(second.sends.is_empty());
        assert!(matches!(second.report, Some(PermissionReport::Failure(_))));
        // The first intent is intact and still pending.
        assert_eq!(
            permission.requested(),
            Some(permission::Requested {
                mode: PermissionMode::Yolo,
                persist: None,
            })
        );
    }

    #[test]
    fn permission_outer_handled_keeps_the_request_and_unhandled_rolls_back() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);
        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        let generation = applied.sends[0].generation;

        // Accepted: the request keeps waiting for its widget record.
        let applied = permission.outer(&mut session, generation, true);
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert!(permission.requested().is_some());

        // A later rejection rolls it back and reports a transport failure.
        let applied = permission.outer(&mut session, generation, false);
        assert!(applied.sends.is_empty());
        assert!(matches!(applied.report, Some(PermissionReport::Failure(_))));
        assert_eq!(permission.requested(), None);
        assert_eq!(permission.confirmed(), permission::Confirmed::default());

        // A stale rejection cannot drop a newer request.
        let applied = permission.set_permission_mode(&mut session, PermissionMode::Auto, None);
        assert_eq!(applied.sends.len(), 1);
        let applied = permission.outer(&mut session, generation, false);
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert!(permission.requested().is_some());
    }

    #[test]
    fn permission_shares_the_probe_session_id_space_and_survives_reset() {
        let mut desktop = Desktop::new();
        // The probe uses desktop-1 through the one shared session.
        let probe = desktop.ready(true);
        assert_eq!(probe.sends[0].request.request_id(), "desktop-1");
        desktop.expire(probe.sends[0].generation);

        let mut permission = Permission::new();
        permission.set_capabilities(permission_capabilities(
            &["get_permission_mode", "set_permission_mode"],
            &["permission_mode_changed"],
        ));

        // The permission request continues the shared id space instead of
        // restarting at desktop-1.
        let applied =
            permission.set_permission_mode(desktop.session(), PermissionMode::Yolo, Some(true));
        assert_eq!(applied.sends[0].request.request_id(), "desktop-2");
        let id = applied.sends[0].request.request_id().to_string();

        // Reset the shared session: the old answer must not be applied.
        permission.reset(desktop.session());
        assert_eq!(permission.confirmed(), permission::Confirmed::default());
        let applied = permission.apply(
            desktop.session(),
            &desktop_widget(set_mode_lines(&id, set_result("yolo", "yolo", true, true))),
        );
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert_eq!(permission.confirmed(), permission::Confirmed::default());
    }

    #[test]
    fn permission_event_gap_signals_one_resync_and_a_later_snapshot_recovers() {
        // The channel is ready at sequence 4, and the permission slice shares
        // its session.
        let mut desktop = probed(4);
        let mut permission = Permission::new();
        permission.set_capabilities(permission_capabilities(
            &["get_permission_mode", "set_permission_mode"],
            &["permission_mode_changed"],
        ));

        // A permission event that skipped a sequence surfaces the gap instead
        // of applying it; the slice accepted the record exactly once.
        let applied = permission.apply(
            desktop.session(),
            &desktop_widget(permission_event_lines(
                6,
                permission_change("yolo", "normal", "yolo", true),
            )),
        );
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(PermissionReport::OutOfOrderSequence));
        assert_eq!(permission.confirmed(), permission::Confirmed::default());

        // The channel owns the recovery: exactly one get_snapshot, and a
        // second resync while it is in flight sends nothing (no duplicate).
        let sends = desktop.resync();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        let resync_id = sends[0].request.request_id().to_string();
        assert!(desktop.resync().is_empty());

        // The snapshot answers and adopts the sequence, so later events line
        // up again.
        let applied = desktop.ui(&desktop_widget(snapshot_lines(&resync_id, 6)));
        assert!(applied.sends.is_empty());
        assert_eq!(desktop.covered(), Some(6));
        assert_eq!(desktop.session().last_sequence(), Some(6));
    }

    #[test]
    fn permission_event_gap_during_a_pending_request_defers_resync_until_settle() {
        let mut desktop = probed(4);
        let mut permission = Permission::new();
        permission.set_capabilities(permission_capabilities(
            &["get_permission_mode", "set_permission_mode"],
            &["permission_mode_changed"],
        ));

        // A set request is in flight on the shared session.
        let set =
            permission.set_permission_mode(desktop.session(), PermissionMode::Yolo, Some(true));
        assert_eq!(set.sends.len(), 1);
        let set_id = set.sends[0].request.request_id().to_string();

        // A permission event gap arrives while the set is pending: no
        // get_snapshot is sent now, and the gap is remembered.
        let gap = permission.apply(
            desktop.session(),
            &desktop_widget(permission_event_lines(
                6,
                permission_change("auto", "yolo", "auto", false),
            )),
        );
        assert_eq!(gap.report, Some(PermissionReport::OutOfOrderSequence));
        assert!(gap.sends.is_empty());
        assert_eq!(desktop.session().last_sequence(), Some(4));
        assert!(
            desktop.resync().is_empty(),
            "no resync while a request is pending"
        );

        // The set settles: exactly one get_snapshot is now issued.
        let settled = permission.apply(
            desktop.session(),
            &desktop_widget(set_mode_lines(
                &set_id,
                set_result("yolo", "yolo", true, true),
            )),
        );
        assert!(settled.report.is_none());
        let sends = desktop.flush_resync();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        assert!(desktop.flush_resync().is_empty(), "one resync only");
    }

    #[test]
    fn permission_event_gap_during_a_pending_request_defers_resync_until_timeout() {
        let mut desktop = probed(4);
        let mut permission = Permission::new();
        permission.set_capabilities(permission_capabilities(
            &["get_permission_mode", "set_permission_mode"],
            &["permission_mode_changed"],
        ));

        let set =
            permission.set_permission_mode(desktop.session(), PermissionMode::Yolo, Some(true));
        let generation = set.sends[0].generation;

        let gap = permission.apply(
            desktop.session(),
            &desktop_widget(permission_event_lines(
                6,
                permission_change("auto", "yolo", "auto", false),
            )),
        );
        assert_eq!(gap.report, Some(PermissionReport::OutOfOrderSequence));
        assert!(desktop.resync().is_empty());

        // The pending request times out (rolls back); the deferred resync is
        // due, and the timeout is reported instead of vanishing.
        let timed_out = permission.expire(desktop.session(), generation);
        assert_eq!(timed_out.report, Some(PermissionReport::Timeout));
        let sends = desktop.flush_resync();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        assert!(desktop.flush_resync().is_empty());
    }

    #[test]
    fn permission_set_without_capabilities_is_not_ready_not_unsupported() {
        let mut permission = Permission::new();
        let mut session = DesktopSession::new();
        // Capabilities not known yet: this is "try again", never the legacy
        // fallback signal.
        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(PermissionReport::NotReady));

        // Once capabilities are known to omit the method, it is Unsupported.
        permission.set_capabilities(permission_capabilities(&["get_permission_mode"], &[]));
        let applied =
            permission.set_permission_mode(&mut session, PermissionMode::Yolo, Some(true));
        assert_eq!(applied.report, Some(PermissionReport::Unsupported));
    }

    #[test]
    fn permission_seed_fetches_once_and_waits_for_a_pending_request() {
        let (mut permission, mut session) = permission_ready(&["permission_mode_changed"]);

        // A request is in flight (the probe's snapshot, say): seed defers.
        let probe = session.issue(DesktopMethod::GetSnapshot).unwrap();
        let seeded = permission.seed(&mut session);
        assert!(seeded.sends.is_empty() && seeded.report.is_none());

        // The probe settles; seed now issues exactly one get_permission_mode.
        session
            .accept(&desktop_widget(snapshot_lines(probe.request_id(), 4)))
            .unwrap();
        let seeded = permission.seed(&mut session);
        assert_eq!(seeded.sends.len(), 1);
        assert_eq!(
            seeded.sends[0].request.method(),
            DesktopMethod::GetPermissionMode
        );
        assert!(permission.seed(&mut session).sends.is_empty());

        // The answer seeds the confirmed state.
        let id = seeded.sends[0].request.request_id().to_string();
        let applied = permission.apply(
            &mut session,
            &desktop_widget(get_mode_lines(&id, "auto", "yolo")),
        );
        assert!(applied.report.is_none());
        assert_eq!(permission.confirmed().mode, Some(PermissionMode::Auto));
        assert_eq!(
            permission.confirmed().default_mode,
            Some(PermissionMode::Yolo)
        );

        // Without get_permission_mode in the capabilities, seed does nothing.
        let mut missing = Permission::new();
        missing.set_capabilities(permission_capabilities(&["set_permission_mode"], &[]));
        assert!(missing.seed(&mut DesktopSession::new()).sends.is_empty());
    }

    // Gentle Desktop's supervision slice: subagents and background jobs are
    // built directly, so no process runs and no file is written.

    fn supervision_capabilities(methods: &[&str], events: &[&str]) -> Capabilities {
        Capabilities {
            methods: methods.iter().map(|name| (*name).to_string()).collect(),
            events: events.iter().map(|name| (*name).to_string()).collect(),
            schema: Some(SCHEMA.into()),
            command: Some(PROMPT_COMMAND.into()),
            widget: Some(WIDGET_KEY.into()),
        }
    }

    fn agent_json(status: &str) -> Value {
        json!({
            "id": "a1",
            "agent": "worker",
            "status": status,
            "model": "anthropic/claude-sonnet-4",
            "lastStep": "writing",
            "usage": {"total": 100, "cost": 0.0421, "input": 90, "output": 10},
        })
    }

    fn job_json(status: &str) -> Value {
        json!({"id": "j1", "label": "Deploy", "status": status, "startedAt": 1})
    }

    fn list_subagents_lines(request_id: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {"agents": [agent_json("running")]},
        }))
    }

    fn list_jobs_lines(request_id: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {"jobs": [job_json("running")]},
        }))
    }

    fn cancel_subagent_lines(request_id: &str, data: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": data,
        }))
    }

    fn agents_update_lines(seq: u64, agents: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": seq,
            "event": "agents_update",
            "data": {"agents": agents},
        }))
    }

    fn supervision_capabilities_lines(request_id: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {
                "schema": SCHEMA,
                "command": PROMPT_COMMAND,
                "widget": WIDGET_KEY,
                "methods": ["capabilities", "get_snapshot", "list_subagents", "list_jobs", "cancel_subagent", "cancel_job"],
                "events": ["agents_update", "jobs_update"],
            },
        }))
    }

    fn supervision_ready() -> (Agents, DesktopSession) {
        let mut supervision = Agents::new();
        supervision.set_capabilities(supervision_capabilities(
            &[
                "list_subagents",
                "list_jobs",
                "cancel_subagent",
                "cancel_job",
            ],
            &["agents_update", "jobs_update"],
        ));
        (supervision, DesktopSession::new())
    }

    fn seed_supervision(supervision: &mut Agents, session: &mut DesktopSession) {
        let list = supervision.seed(session);
        assert_eq!(list.sends.len(), 1);
        let id = list.sends[0].request.request_id().to_string();
        assert!(
            supervision
                .apply(session, &desktop_widget(list_subagents_lines(&id)))
                .report
                .is_none()
        );
        let list = supervision.seed(session);
        assert_eq!(list.sends.len(), 1);
        let id = list.sends[0].request.request_id().to_string();
        assert!(
            supervision
                .apply(session, &desktop_widget(list_jobs_lines(&id)))
                .report
                .is_none()
        );
    }

    #[test]
    fn supervision_seeds_both_lists_once_the_probe_settles() {
        let (mut supervision, mut session) = supervision_ready();

        // A request in flight (the probe's snapshot) defers the seed.
        let probe = session.issue(DesktopMethod::GetSnapshot).unwrap();
        assert!(supervision.seed(&mut session).sends.is_empty());
        session
            .accept(&desktop_widget(snapshot_lines(probe.request_id(), 4)))
            .unwrap();

        // The first seed asks for subagents, then jobs, then nothing.
        let list = supervision.seed(&mut session);
        assert_eq!(list.sends.len(), 1);
        assert_eq!(list.sends[0].request.method(), DesktopMethod::ListSubagents);
        let id = list.sends[0].request.request_id().to_string();
        assert!(
            supervision
                .apply(&mut session, &desktop_widget(list_subagents_lines(&id)))
                .report
                .is_none()
        );
        assert!(supervision.subagents_ready());

        let list = supervision.seed(&mut session);
        assert_eq!(list.sends.len(), 1);
        assert_eq!(list.sends[0].request.method(), DesktopMethod::ListJobs);
        let id = list.sends[0].request.request_id().to_string();
        assert!(
            supervision
                .apply(&mut session, &desktop_widget(list_jobs_lines(&id)))
                .report
                .is_none()
        );
        assert!(supervision.jobs_ready());

        assert!(supervision.seed(&mut session).sends.is_empty());
    }

    #[test]
    fn supervision_ready_is_the_fallback_precedence_gate() {
        let mut supervision = Agents::new();
        let mut session = DesktopSession::new();
        // No capabilities: nothing is offered, nothing is seeded.
        assert!(!supervision.subagents_ready());
        assert!(!supervision.jobs_ready());
        assert!(supervision.seed(&mut session).sends.is_empty());

        // Capabilities without a fetched list: not ready yet (legacy wins).
        supervision.set_capabilities(supervision_capabilities(
            &["list_subagents", "list_jobs"],
            &[],
        ));
        assert!(!supervision.subagents_ready());
        let list = supervision.seed(&mut session);
        assert_eq!(list.sends.len(), 1);
        assert!(!supervision.subagents_ready());

        let id = list.sends[0].request.request_id().to_string();
        supervision.apply(&mut session, &desktop_widget(list_subagents_lines(&id)));
        assert!(supervision.subagents_ready());
    }

    #[test]
    fn supervision_reconciles_events_by_id_and_ignores_duplicates() {
        let (mut supervision, mut session) = supervision_ready();
        seed_supervision(&mut supervision, &mut session);
        assert_eq!(supervision.subagents().len(), 1);
        assert_eq!(supervision.subagents()[0].status, "running");

        // An agents_update event replaces the known id and appends a new one.
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(agents_update_lines(
                1,
                json!([
                    {"id": "a1", "agent": "worker", "status": "completed", "model": "m", "lastStep": "completed", "usage": {"total": 1, "cost": 0.0}},
                    {"id": "a2", "agent": "explore", "status": "running", "model": "m", "lastStep": "running", "usage": {"total": 1, "cost": 0.0}},
                ]),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(supervision.subagents().len(), 2);
        assert_eq!(supervision.subagents()[0].status, "completed");
        assert_eq!(supervision.subagents()[1].id, "a2");

        // A replayed event (same sequence) is refused and changes nothing.
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(agents_update_lines(
                1,
                json!([
                    {"id": "a1", "agent": "worker", "status": "failed", "model": "m", "lastStep": "failed", "usage": {"total": 1, "cost": 0.0}},
                ]),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(supervision.subagents()[0].status, "completed");
    }

    #[test]
    fn supervision_events_are_full_projections_that_drop_missing_ids() {
        let (mut supervision, mut session) = supervision_ready();
        seed_supervision(&mut supervision, &mut session);

        // First event adds a second id.
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(agents_update_lines(
                1,
                json!([
                    {"id": "a1", "agent": "worker", "status": "running", "model": "m", "lastStep": "running", "usage": {"total": 1, "cost": 0.0}},
                    {"id": "a2", "agent": "explore", "status": "running", "model": "m", "lastStep": "running", "usage": {"total": 1, "cost": 0.0}},
                ]),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(supervision.subagents().len(), 2);

        // The next event omits a2: it is removed, never grown forever.
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(agents_update_lines(
                2,
                json!([
                    {"id": "a1", "agent": "worker", "status": "completed", "model": "m", "lastStep": "completed", "usage": {"total": 1, "cost": 0.0}},
                ]),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(supervision.subagents().len(), 1);
        assert_eq!(supervision.subagents()[0].id, "a1");
        assert_eq!(supervision.subagents()[0].status, "completed");
    }

    #[test]
    fn a_preseed_event_does_not_mark_the_list_fetched() {
        let (mut supervision, mut session) = supervision_ready();

        // An agents_update event arrives before the full list seed.
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(agents_update_lines(
                1,
                json!([
                    {"id": "a9", "agent": "worker", "status": "running", "model": "m", "lastStep": "running", "usage": {"total": 1, "cost": 0.0}},
                ]),
            )),
        );
        assert!(applied.report.is_none());
        assert!(
            !supervision.subagents_ready(),
            "an event is not the full seed"
        );
        assert_eq!(
            supervision.subagents().len(),
            1,
            "the event's projection is kept"
        );

        // The full seed still occurs and replaces the event's partial data.
        let list = supervision.seed(&mut session);
        assert_eq!(list.sends.len(), 1);
        assert_eq!(list.sends[0].request.method(), DesktopMethod::ListSubagents);
        let id = list.sends[0].request.request_id().to_string();
        assert!(
            supervision
                .apply(&mut session, &desktop_widget(list_subagents_lines(&id)))
                .report
                .is_none()
        );
        assert!(supervision.subagents_ready());
        assert_eq!(supervision.subagents().len(), 1);
        assert_eq!(
            supervision.subagents()[0].id,
            "a1",
            "the full seed replaced the event's partial data"
        );
    }

    #[test]
    fn session_switch_resets_the_channel_pending_and_reseeds_once() {
        let mut desktop = Desktop::new();
        // Probe → capabilities (with supervision) → snapshot.
        let probe = desktop.ready(true);
        let _ = desktop.outer(probe.sends[0].generation, true);
        let applied = desktop.ui(&desktop_widget(supervision_capabilities_lines("desktop-1")));
        let snapshot_id = applied.sends[0].request.request_id().to_string();
        let _ = desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, 4)));

        // Seed both lists through the wrapper, then leave a cancel in flight.
        let list = desktop.supervision_seed();
        assert_eq!(list.sends[0].request.method(), DesktopMethod::ListSubagents);
        let id = list.sends[0].request.request_id().to_string();
        desktop.supervision_apply(&desktop_widget(list_subagents_lines(&id)));
        let list = desktop.supervision_seed();
        assert_eq!(list.sends[0].request.method(), DesktopMethod::ListJobs);
        let id = list.sends[0].request.request_id().to_string();
        desktop.supervision_apply(&desktop_widget(list_jobs_lines(&id)));
        assert!(desktop.subagents_ready() && desktop.jobs_ready());

        let cancel = desktop.cancel_subagent("a1".into());
        let cancel_id = cancel.sends[0].request.request_id().to_string();
        assert!(desktop.session().pending().is_some());

        // Session switch: reset frees the pending slot and clears the baseline.
        desktop.reset();
        assert!(desktop.session().pending().is_none());
        assert!(desktop.session().last_sequence().is_none());
        assert!(!desktop.subagents_ready() && !desktop.jobs_ready());

        // A late old cancel response is benign and reconciles nothing.
        let applied = desktop.supervision_apply(&desktop_widget(cancel_subagent_lines(
            &cancel_id,
            json!({ "agent": agent_json("cancelled"), "changed": true }),
        )));
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert!(desktop.subagents().is_empty());

        // Re-probe re-discovers capabilities and re-seeds exactly once.
        let probe = desktop.ready(true);
        assert_eq!(probe.sends.len(), 1);
        assert_eq!(probe.sends[0].request.method(), DesktopMethod::Capabilities);
        let probe_id = probe.sends[0].request.request_id().to_string();
        let applied = desktop.ui(&desktop_widget(supervision_capabilities_lines(&probe_id)));
        assert_eq!(applied.sends.len(), 1, "the snapshot follows the re-probe");
        let snapshot_id = applied.sends[0].request.request_id().to_string();
        let _ = desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, 4)));
        // The supervision seed chains exactly once behind the probe.
        let list = desktop.supervision_seed();
        assert_eq!(list.sends.len(), 1);
        assert_eq!(list.sends[0].request.method(), DesktopMethod::ListSubagents);
    }

    #[test]
    fn supervision_gap_resets_the_seeded_markers_without_fabricating_empty() {
        let (mut supervision, mut session) = supervision_ready();
        seed_supervision(&mut supervision, &mut session);
        assert!(supervision.subagents_ready() && supervision.jobs_ready());

        // A first event sets the baseline.
        let first = supervision.apply(
            &mut session,
            &desktop_widget(agents_update_lines(
                1,
                json!([
                    {"id": "a1", "agent": "worker", "status": "running", "model": "m", "lastStep": "running", "usage": {"total": 1, "cost": 0.0}},
                ]),
            )),
        );
        assert!(first.report.is_none());

        // An event that skipped a sequence is a gap, not an update.
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(agents_update_lines(
                7,
                json!([
                    {"id": "a9", "agent": "worker", "status": "running", "model": "m", "lastStep": "running", "usage": {"total": 1, "cost": 0.0}},
                ]),
            )),
        );
        assert_eq!(applied.report, Some(SupervisionReport::OutOfOrderSequence));
        assert!(!supervision.subagents_ready());
        assert!(!supervision.jobs_ready());
        assert_eq!(
            supervision.subagents().len(),
            1,
            "a gap never fabricates an empty list"
        );
    }

    #[test]
    fn cancel_subagent_is_an_rpc_request_with_one_in_flight() {
        let (mut supervision, mut session) = supervision_ready();
        let applied = supervision.cancel_subagent(&mut session, "a1".into());
        assert_eq!(applied.sends.len(), 1);
        assert_eq!(
            applied.sends[0].request.method(),
            DesktopMethod::CancelSubagent
        );
        let payload: Value = serde_json::from_str(applied.sends[0].request.payload()).unwrap();
        assert_eq!(payload["method"], "cancel_subagent");
        assert_eq!(payload["params"]["id"], "a1");
        // It rides the Desktop channel, never a raw conversation prompt.
        assert!(
            applied.sends[0]
                .request
                .prompt_message()
                .starts_with(PROMPT_COMMAND)
        );

        // One request in flight: a second cancel is refused, not queued.
        let second = supervision.cancel_job(&mut session, "j1".into());
        assert!(second.sends.is_empty());
        assert!(matches!(second.report, Some(SupervisionReport::Failure(_))));
    }

    #[test]
    fn cancel_response_keeps_requested_vs_changed_truth() {
        let (mut supervision, mut session) = supervision_ready();
        seed_supervision(&mut supervision, &mut session);

        // Async: requested true, changed absent → the server's projection
        // (still running) is kept; changed is never invented.
        let applied = supervision.cancel_subagent(&mut session, "a1".into());
        let id = applied.sends[0].request.request_id().to_string();
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(cancel_subagent_lines(
                &id,
                json!({ "agent": agent_json("running"), "requested": true }),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(supervision.subagents()[0].status, "running");

        // Sync: changed true → the server's cancelled projection is kept.
        let applied = supervision.cancel_subagent(&mut session, "a1".into());
        let id = applied.sends[0].request.request_id().to_string();
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(cancel_subagent_lines(
                &id,
                json!({
                    "agent": agent_json("cancelled"),
                    "requested": true,
                    "changed": true,
                }),
            )),
        );
        assert!(applied.report.is_none());
        assert_eq!(supervision.subagents()[0].status, "cancelled");
    }

    #[test]
    fn cancel_failures_report_timeout_unsupported_and_not_found() {
        // Timeout: the request is rolled back and reported.
        let (mut supervision, mut session) = supervision_ready();
        let applied = supervision.cancel_subagent(&mut session, "a1".into());
        let generation = applied.sends[0].generation;
        let timed_out = supervision.expire(&mut session, generation);
        assert_eq!(timed_out.report, Some(SupervisionReport::Timeout));

        // Unsupported: capabilities known but the method is absent.
        let mut missing = Agents::new();
        missing.set_capabilities(supervision_capabilities(&["list_subagents"], &[]));
        let mut missing_session = DesktopSession::new();
        let applied = missing.cancel_subagent(&mut missing_session, "a1".into());
        assert_eq!(applied.report, Some(SupervisionReport::Unsupported));

        // Not ready: capabilities unknown.
        let mut not_ready = Agents::new();
        let mut not_ready_session = DesktopSession::new();
        let applied = not_ready.cancel_subagent(&mut not_ready_session, "a1".into());
        assert_eq!(applied.report, Some(SupervisionReport::NotReady));

        // unsupported_method: the server answers it, the target is kept for
        // the legacy fallback, and it is not a generic failure.
        let (mut claimed, mut claimed_session) = supervision_ready();
        let applied = claimed.cancel_subagent(&mut claimed_session, "a1".into());
        let id = applied.sends[0].request.request_id().to_string();
        let applied = claimed.apply(
            &mut claimed_session,
            &desktop_widget(error_lines(&id, "unsupported_method", "nope")),
        );
        assert_eq!(applied.report, Some(SupervisionReport::UnsupportedMethod));
        assert_eq!(
            claimed.take_cancel_target().map(|t| t.id().to_string()),
            Some("a1".to_string())
        );

        // not_found: a distinct report, and the pending cancel is cleared.
        let (mut gone, mut gone_session) = supervision_ready();
        let applied = gone.cancel_job(&mut gone_session, "j1".into());
        let id = applied.sends[0].request.request_id().to_string();
        let applied = gone.apply(
            &mut gone_session,
            &desktop_widget(error_lines(&id, "not_found", "gone")),
        );
        assert_eq!(applied.report, Some(SupervisionReport::NotFound));
        assert!(gone.take_cancel_target().is_none());
    }

    #[test]
    fn cancel_job_reconciles_and_reset_forgets_a_late_response() {
        let (mut supervision, mut session) = supervision_ready();
        seed_supervision(&mut supervision, &mut session);

        let applied = supervision.cancel_job(&mut session, "j1".into());
        let id = applied.sends[0].request.request_id().to_string();
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(record(json!({
                "schema": SCHEMA,
                "kind": "response",
                "requestId": id,
                "ok": true,
                "data": {"job": job_json("exited"), "changed": true},
            }))),
        );
        assert!(applied.report.is_none());
        assert_eq!(supervision.jobs()[0].status, "exited");

        // A new backend resets the slice; the old answer arrives late and is
        // benign (never applied, never reported).
        supervision.reset();
        assert!(!supervision.subagents_ready() && !supervision.jobs_ready());
        assert!(supervision.subagents().is_empty() && supervision.jobs().is_empty());
        let applied = supervision.apply(
            &mut session,
            &desktop_widget(record(json!({
                "schema": SCHEMA,
                "kind": "response",
                "requestId": id,
                "ok": true,
                "data": {"job": job_json("failed"), "changed": true},
            }))),
        );
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert!(supervision.jobs().is_empty());
    }

    #[test]
    fn desktop_wrapper_rides_the_shared_session() {
        let mut desktop = Desktop::new();
        let probe = desktop.ready(true);
        let _ = desktop.outer(probe.sends[0].generation, true);
        let applied = desktop.ui(&desktop_widget(record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": "desktop-1",
            "ok": true,
            "data": {
                "schema": SCHEMA,
                "command": PROMPT_COMMAND,
                "widget": WIDGET_KEY,
                "methods": ["capabilities", "get_snapshot", "list_subagents", "list_jobs", "cancel_subagent", "cancel_job"],
                "events": ["agents_update", "jobs_update"],
            },
        }))));
        let snapshot_id = applied.sends[0].request.request_id().to_string();
        let _ = desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, 4)));

        // The wrapper issues the cancel method and shares the session's ids.
        let applied = desktop.cancel_subagent("a1".into());
        assert_eq!(applied.sends.len(), 1);
        assert_eq!(
            applied.sends[0].request.method(),
            DesktopMethod::CancelSubagent
        );
        assert!(desktop.cancel_subagent_supported());
        assert!(desktop.cancel_job_supported());
    }
}
