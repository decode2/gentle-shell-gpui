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
        Capabilities, ConfigStore, ConfigWrite, DesktopAgent, DesktopJob, DesktopMethod,
        DesktopRecord, DesktopRequest, DesktopRpcError, DesktopSession, PROMPT_COMMAND, Snapshot,
        WIDGET_KEY,
    };

    use crate::features::profiles::{
        ConfigChange, ConfigFailure, ConfigOutcome, ConfigSupport, ConfigTicket,
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
        /// Pi listed no `gentle:desktop` command: the extension is absent, as
        /// opposed to a channel that failed while probing.
        missing_extension: bool,
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
        /// Provider subscription usage over the same channel.
        subscription: subscription::Subscription,
        /// The design canvas over the same channel.
        design: design::Design,
        /// The config stores the Profiles screen reads and writes.
        config: config::Config,
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
                missing_extension: false,
                snapshot: None,
                covered: None,
                step: Step::Idle,
                generation: 0,
                pending_resync: false,
                supervision: supervision::Agents::default(),
                subscription: subscription::Subscription::default(),
                design: design::Design::new(),
                config: config::Config::default(),
            }
        }

        /// The backend was replaced or exited: nothing the old one said or
        /// numbered applies. Request ids keep advancing, so a late answer from
        /// it can never match a new request.
        pub(in crate::features::session) fn reset(&mut self) {
            self.session.reset();
            self.capabilities = None;
            self.missing_extension = false;
            self.snapshot = None;
            self.covered = None;
            self.step = Step::Idle;
            self.generation += 1;
            self.pending_resync = false;
            self.supervision.reset();
            self.subscription.reset(&mut self.session);
            self.design.reset(&mut self.session);
            self.config.reset(&mut self.session);
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
                self.missing_extension = true;
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
                            self.supervision.set_capabilities(capabilities.clone());
                            self.subscription.set_capabilities(capabilities.clone());
                            self.config.set_capabilities(capabilities.clone());
                            self.design.set_capabilities(capabilities);
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
                    // The permission, supervision, subscription, design and
                    // config methods are served by their own slices; this
                    // channel only probes and sequences.
                    Some(DesktopMethod::GetPermissionMode)
                    | Some(DesktopMethod::SetPermissionMode)
                    | Some(DesktopMethod::ListSubagents)
                    | Some(DesktopMethod::ListJobs)
                    | Some(DesktopMethod::CancelSubagent)
                    | Some(DesktopMethod::CancelJob)
                    | Some(DesktopMethod::GetSubscriptionUsage)
                    | Some(DesktopMethod::RefreshSubscriptionUsage)
                    | Some(DesktopMethod::GetDesignCanvas)
                    | Some(DesktopMethod::GetConfig)
                    | Some(DesktopMethod::SetConfig) => Applied::default(),
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

        /// The subscription slice's snapshot, as the Usage screen reads it.
        pub(in crate::features::session) fn subscription_snapshot(&self) -> subscription::Snapshot {
            self.subscription.snapshot()
        }

        /// Ask the server for the whole-account subscription usage once.
        pub(in crate::features::session) fn subscription_seed(&mut self) -> subscription::Applied {
            self.subscription.seed(&mut self.session)
        }

        /// Refresh the whole-account subscription usage.
        pub(in crate::features::session) fn subscription_refresh(
            &mut self,
        ) -> subscription::Applied {
            self.subscription.refresh(&mut self.session)
        }

        /// One incoming UI request for the subscription slice.
        pub(in crate::features::session) fn subscription_apply(
            &mut self,
            request: &UiRequest,
        ) -> subscription::Applied {
            self.subscription.apply(&mut self.session, request)
        }

        /// The pending timeout for a subscription request.
        pub(in crate::features::session) fn subscription_expire(
            &mut self,
            generation: u64,
        ) -> subscription::Applied {
            self.subscription.expire(&mut self.session, generation)
        }

        /// Pi answered the outer prompt of a subscription request.
        pub(in crate::features::session) fn subscription_outer(
            &mut self,
            generation: u64,
            handled: bool,
        ) -> subscription::Applied {
            self.subscription
                .outer(&mut self.session, generation, handled)
        }

        /// Seed the design canvas once the probe has landed (the channel is
        /// Ready) and no other request is in flight.
        pub(in crate::features::session) fn design_seed(&mut self) -> design::Applied {
            // A deferred resync is owed first: seeding ahead of its snapshot
            // would only be repeated behind it.
            if self.step != Step::Ready || self.pending_resync {
                return design::Applied::default();
            }
            self.design.seed(&mut self.session)
        }

        /// Whether `design_canvas` events are the canvas source on this
        /// backend: the channel is Ready and the peer advertised them. Then the
        /// legacy `notify` text would only navigate twice.
        pub(in crate::features::session) fn design_events_ready(&self) -> bool {
            self.step == Step::Ready && self.design.emits_events()
        }

        /// One incoming UI request for the design slice.
        pub(in crate::features::session) fn design_apply(
            &mut self,
            request: &UiRequest,
        ) -> design::Applied {
            self.design.apply(&mut self.session, request)
        }

        /// The pending timeout for a design request.
        pub(in crate::features::session) fn design_expire(
            &mut self,
            generation: u64,
        ) -> design::Applied {
            self.design.expire(&mut self.session, generation)
        }

        /// Pi answered the outer prompt of a design request.
        pub(in crate::features::session) fn design_outer(
            &mut self,
            generation: u64,
            handled: bool,
        ) -> design::Applied {
            self.design.outer(&mut self.session, generation, handled)
        }

        /// Whether this backend serves the config stores over the channel.
        /// Only a missing extension or capabilities without the methods mean
        /// the direct-file path; a channel that failed here is neither.
        pub(in crate::features::session) fn config_support(&self) -> ConfigSupport {
            if self.step == Step::Unavailable {
                return if self.missing_extension {
                    ConfigSupport::Legacy
                } else {
                    ConfigSupport::Unavailable
                };
            }
            match self.config.offers_config() {
                None => ConfigSupport::NotReady,
                Some(true) => ConfigSupport::Rpc,
                Some(false) => ConfigSupport::Legacy,
            }
        }

        /// Queue a `get_config`; [`Self::config_pump`] sends it.
        pub(in crate::features::session) fn config_get(
            &mut self,
            store: ConfigStore,
        ) -> Result<ConfigTicket, ConfigFailure> {
            self.config.get(store)
        }

        /// Queue a `set_config`; [`Self::config_pump`] sends it.
        pub(in crate::features::session) fn config_set(
            &mut self,
            write: ConfigWrite,
            expected_revision: String,
        ) -> Result<ConfigTicket, ConfigFailure> {
            self.config.set(write, expected_revision)
        }

        /// Send the next queued config request once the probe has landed, no
        /// resync is owed and no other request is in flight.
        pub(in crate::features::session) fn config_pump(&mut self) -> config::Applied {
            if self.step != Step::Ready || self.pending_resync {
                return config::Applied::default();
            }
            self.config.pump(&mut self.session)
        }

        /// One incoming UI request for the config slice.
        pub(in crate::features::session) fn config_apply(
            &mut self,
            request: &UiRequest,
        ) -> config::Applied {
            self.config.apply(&mut self.session, request)
        }

        /// The pending timeout for a config request.
        pub(in crate::features::session) fn config_expire(
            &mut self,
            generation: u64,
        ) -> config::Applied {
            self.config.expire(&mut self.session, generation)
        }

        /// Pi answered the outer prompt of a config request.
        pub(in crate::features::session) fn config_outer(
            &mut self,
            generation: u64,
            handled: bool,
        ) -> config::Applied {
            self.config.outer(&mut self.session, generation, handled)
        }

        /// The answers to config tickets since the last call.
        pub(in crate::features::session) fn take_config_outcomes(
            &mut self,
        ) -> Vec<(ConfigTicket, ConfigOutcome)> {
            self.config.take_outcomes()
        }

        /// The stores that changed (or may have) since the last call.
        pub(in crate::features::session) fn take_config_changes(&mut self) -> Vec<ConfigChange> {
            self.config.take_changes()
        }

        /// Stop asking this backend and say why once: the channel is unusable
        /// here until the backend is replaced.
        fn unavailable(&mut self, report: Report) -> Applied {
            self.session.abandon();
            self.config.drop_queued();
            self.step = Step::Unavailable;
            Applied {
                sends: Vec::new(),
                report: Some(report),
            }
        }

        /// Ask for the state again after a gap in the events.
        pub(in crate::features::session) fn resync(&mut self) -> Vec<Send> {
            // Whichever record showed the gap, a `design_canvas` transition may
            // be among the lost events: the canvas re-seeds behind the
            // snapshot. Lost `config_changed` events mean any store may have
            // moved: the screen re-reads them behind it.
            self.design.rearm();
            self.config.rearm();
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
            // Nothing pumps the config queue until the channel is probed
            // again, so its waiting tickets are answered now.
            self.config.drop_queued();
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

    /// Gentle Desktop's provider subscription usage in one tab.
    ///
    /// When the backend's `capabilities` advertise `get_subscription_usage`,
    /// the Usage screen reads the whole-account snapshot through this slice
    /// and refreshes it with `refresh_subscription_usage`. `rate_limit`
    /// events reconcile per-provider snapshots — Claude's 5h/week response
    /// headers included — so those windows finally arrive in the app. It
    /// borrows the parent's [`DesktopSession`], so probe, permission,
    /// supervision and subscription requests share one id space, one pending
    /// slot and one event baseline.
    ///
    /// The slice is pure RPC state: it never reads `auth.json`, resolves a
    /// `$VAR`/`!command`, builds a bearer header, or calls a provider API.
    /// When the peer does not offer the methods, the Usage screen keeps the
    /// legacy `auth.json` path; any other failure here never falls back to it.
    pub(in crate::features::session) mod subscription {
        use pi_rpc::UiRequest;
        use pi_rpc::desktop_rpc::{
            Capabilities, DesktopEvent, DesktopMethod, DesktopRecord, DesktopResponse,
            DesktopRpcError, DesktopSession, ProviderUsage, UsageStatus,
        };

        use super::Send;

        /// The wire name of the only event this slice applies.
        pub(in crate::features::session) const RATE_LIMIT: &str = "rate_limit";

        /// What one decision produced: the requests to send, and a failure the
        /// tab shows through its report path.
        #[derive(Debug, Default)]
        pub(in crate::features::session) struct Applied {
            pub(in crate::features::session) sends: Vec<Send>,
            pub(in crate::features::session) report: Option<Report>,
        }

        /// Why a subscription request stopped or what it surfaced. The first
        /// two variants are the legacy-fallback signals for the Usage screen;
        /// every other variant leaves the snapshot unchanged and never
        /// triggers a direct provider API call.
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
                        "This Gentle Shell does not support reading subscription usage from Desktop."
                            .to_string()
                    }
                    Self::NotReady => {
                        "Gentle Desktop is still loading; try again in a moment.".to_string()
                    }
                    Self::Timeout => "The subscription usage request timed out.".to_string(),
                    Self::Failure(error) => {
                        format!("The subscription usage request failed ({error}).")
                    }
                    Self::OutOfOrderSequence => {
                        "Gentle Desktop events were lost; resyncing subscription usage.".to_string()
                    }
                }
            }

            /// Only these two mean the Usage screen keeps its legacy `auth.json`
            /// path; every other failure is shown honestly with no fallback.
            pub(in crate::features::session) fn is_legacy(&self) -> bool {
                matches!(self, Self::Unsupported | Self::UnsupportedMethod)
            }
        }

        /// The slice's state as the Usage screen reads it. Plain data on
        /// purpose: the screen decides how to render, never this slice.
        #[derive(Debug, Clone, PartialEq)]
        pub(crate) struct Snapshot {
            pub(crate) providers: Vec<ProviderUsage>,
            pub(crate) legacy: bool,
            pub(crate) not_ready: bool,
            pub(crate) pending: bool,
            pub(crate) error: Option<String>,
        }

        pub(in crate::features::session) struct Subscription {
            capabilities: Option<Capabilities>,
            /// The last good providers, reconciled by provider id.
            providers: Vec<ProviderUsage>,
            /// A `get_subscription_usage` was issued for this backend.
            seeded: bool,
            /// A get/refresh request issued by this slice is in flight.
            pending: bool,
            /// The last failure, cleared by a successful answer.
            report: Option<Report>,
            /// Bumped per request, so an old timer cannot drop a newer one.
            generation: u64,
        }

        impl Default for Subscription {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Subscription {
            pub(in crate::features::session) fn new() -> Self {
                Self {
                    capabilities: None,
                    providers: Vec::new(),
                    seeded: false,
                    pending: false,
                    report: None,
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

            /// The state the Usage screen renders from.
            pub(in crate::features::session) fn snapshot(&self) -> Snapshot {
                Snapshot {
                    providers: self.providers.clone(),
                    legacy: self.legacy(),
                    not_ready: self.capabilities.is_none(),
                    pending: self.pending,
                    error: self
                        .report
                        .as_ref()
                        .filter(|report| !report.is_legacy() && !matches!(report, Report::NotReady))
                        .map(Report::message),
                }
            }

            /// Whether the peer is known to lack the methods, or refused one it
            /// advertised: the legacy-fallback signal for the Usage screen.
            fn legacy(&self) -> bool {
                match &self.report {
                    Some(Report::UnsupportedMethod) => true,
                    _ => self.capabilities.as_ref().is_some_and(|capabilities| {
                        !capabilities.supports(DesktopMethod::GetSubscriptionUsage)
                    }),
                }
            }

            /// Ask the server for the whole-account snapshot once per backend.
            /// It waits for any in-flight request, so the probe and the other
            /// slices never issue two requests at once.
            pub(in crate::features::session) fn seed(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                if self.seeded {
                    return Applied::default();
                }
                let Some(capabilities) = &self.capabilities else {
                    return Applied {
                        report: Some(Report::NotReady),
                        ..Applied::default()
                    };
                };
                if !capabilities.supports(DesktopMethod::GetSubscriptionUsage) {
                    return Applied {
                        report: Some(Report::Unsupported),
                        ..Applied::default()
                    };
                }
                if session.pending().is_some() {
                    return Applied::default();
                }
                self.seeded = true;
                self.get(session)
            }

            /// Fetch the whole-account snapshot (`get_subscription_usage`).
            pub(in crate::features::session) fn get(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                self.issue(session, DesktopMethod::GetSubscriptionUsage)
            }

            /// Refresh the whole-account snapshot (`refresh_subscription_usage`).
            pub(in crate::features::session) fn refresh(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                self.issue(session, DesktopMethod::RefreshSubscriptionUsage)
            }

            fn issue(&mut self, session: &mut DesktopSession, method: DesktopMethod) -> Applied {
                let Some(capabilities) = &self.capabilities else {
                    return Applied {
                        report: Some(Report::NotReady),
                        ..Applied::default()
                    };
                };
                if !capabilities.supports(method) {
                    return Applied {
                        report: Some(Report::Unsupported),
                        ..Applied::default()
                    };
                }
                let request = match method {
                    DesktopMethod::GetSubscriptionUsage => {
                        session.issue_get_subscription_usage(None)
                    }
                    DesktopMethod::RefreshSubscriptionUsage => {
                        session.issue_refresh_subscription_usage(None)
                    }
                    _ => unreachable!("subscription only issues its two methods"),
                };
                let request = match request {
                    Ok(request) => request,
                    Err(error) => return self.failure(error),
                };
                self.generation += 1;
                self.pending = true;
                Applied {
                    sends: vec![Send {
                        request,
                        generation: self.generation,
                    }],
                    report: None,
                }
            }

            /// Pi answered the outer `prompt` of a subscription request.
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
                self.report = Some(Report::Failure(DesktopRpcError::UnexpectedDisposition {
                    disposition: None,
                }));
                Applied {
                    report: self.report.clone(),
                    ..Applied::default()
                }
            }

            /// One incoming UI request; applies only the records this slice owns
            /// (typed get/refresh responses and `rate_limit` events).
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
                        Some(DesktopMethod::GetSubscriptionUsage)
                        | Some(DesktopMethod::RefreshSubscriptionUsage) => {
                            self.apply_response(response)
                        }
                        _ => Applied::default(),
                    },
                    Ok(Some(DesktopRecord::Event(event))) => self.apply_event(event),
                    // Events were lost: the channel owns the get_snapshot
                    // resync. The record was already accepted, so it is never
                    // accepted again.
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
            /// dropped and the timeout is reported rather than disappearing
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
                self.report = Some(Report::Timeout);
                Applied {
                    report: Some(Report::Timeout),
                    ..Applied::default()
                }
            }

            /// The backend was replaced or exited: nothing it said or numbered
            /// applies. Request ids keep advancing, so a late answer can never
            /// match a new request.
            pub(in crate::features::session) fn reset(&mut self, session: &mut DesktopSession) {
                session.reset();
                self.capabilities = None;
                self.providers.clear();
                self.seeded = false;
                self.pending = false;
                self.report = None;
                self.generation += 1;
            }

            fn emits(&self, event: &str) -> bool {
                self.capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.emits(event))
            }

            /// A request could not be issued (one already in flight, or the
            /// request itself failed validation).
            fn failure(&self, error: DesktopRpcError) -> Applied {
                Applied {
                    report: Some(Report::Failure(error)),
                    ..Applied::default()
                }
            }

            /// Drop the request in flight. The generation moves, so a late
            /// timer or disposition for the abandoned request is stale.
            fn rollback(&mut self, session: &mut DesktopSession) {
                session.abandon();
                self.pending = false;
                self.generation += 1;
            }

            fn apply_response(&mut self, response: DesktopResponse) -> Applied {
                // The request is answered either way, so it is no longer pending.
                self.pending = false;
                match response.subscription_usage() {
                    Ok(usage) => {
                        self.providers = merge_full(&self.providers, usage.providers);
                        self.report = None;
                        Applied::default()
                    }
                    Err(DesktopRpcError::RemoteError(info))
                        if info.code == "unsupported_method" =>
                    {
                        self.report = Some(Report::UnsupportedMethod);
                        Applied {
                            report: Some(Report::UnsupportedMethod),
                            ..Applied::default()
                        }
                    }
                    Err(error) => {
                        self.report = Some(Report::Failure(error.clone()));
                        Applied {
                            report: Some(Report::Failure(error)),
                            ..Applied::default()
                        }
                    }
                }
            }

            /// A `rate_limit` event, applied only when the peer advertised that
            /// exact name. Foreign names and unadvertised events never touch the
            /// snapshot. The snapshots are full provider projections, so they
            /// replace the matching provider (missing ids never drop others).
            fn apply_event(&mut self, event: DesktopEvent) -> Applied {
                if event.event != RATE_LIMIT {
                    return Applied::default();
                }
                if !self.emits(RATE_LIMIT) {
                    return Applied::default();
                }
                match event.rate_limit() {
                    Ok(limit) => {
                        reconcile(&mut self.providers, limit.snapshots);
                        self.report = None;
                        Applied::default()
                    }
                    Err(error) => Applied {
                        report: Some(Report::Failure(error)),
                        ..Applied::default()
                    },
                }
            }
        }

        /// Whether the pending request is one this slice issued. The probe and
        /// the other slices share the session, so a stale subscription timer or
        /// disposition must not abandon their requests.
        fn owns_pending(session: &DesktopSession) -> bool {
            matches!(
                session.pending().map(|(_, method)| method),
                Some(DesktopMethod::GetSubscriptionUsage)
                    | Some(DesktopMethod::RefreshSubscriptionUsage)
            )
        }

        /// Replace the whole snapshot, keeping the last good limits of a
        /// provider that reports `pending`/`stale` with none.
        fn merge_full(
            previous: &[ProviderUsage],
            incoming: Vec<ProviderUsage>,
        ) -> Vec<ProviderUsage> {
            incoming
                .into_iter()
                .map(|mut provider| {
                    retain_last_good(
                        previous
                            .iter()
                            .find(|existing| existing.provider == provider.provider),
                        &mut provider,
                    );
                    provider
                })
                .collect()
        }

        /// Upsert event snapshots into the current snapshot by provider id.
        fn reconcile(current: &mut Vec<ProviderUsage>, snapshots: Vec<ProviderUsage>) {
            for mut provider in snapshots {
                retain_last_good(
                    current
                        .iter()
                        .find(|existing| existing.provider == provider.provider),
                    &mut provider,
                );
                if let Some(existing) = current
                    .iter_mut()
                    .find(|entry| entry.provider == provider.provider)
                {
                    *existing = provider;
                } else {
                    current.push(provider);
                }
            }
        }

        /// Keep the last good limits when a `pending`/`stale` entry carries
        /// none: the data is still valid, just not fresh.
        fn retain_last_good(previous: Option<&ProviderUsage>, incoming: &mut ProviderUsage) {
            if incoming.limits.is_empty()
                && matches!(incoming.status, UsageStatus::Pending | UsageStatus::Stale)
                && let Some(previous) = previous.filter(|p| !p.limits.is_empty())
            {
                incoming.limits = previous.limits.clone();
            }
        }
    }

    /// Gentle Desktop's design canvas in one tab.
    ///
    /// When the backend's `capabilities` advertise `get_design_canvas`, the
    /// tab seeds the canvas state once after the probe and follows the
    /// `design_canvas` events. Both become the same browser notice the legacy
    /// `notify` text and `gentle_design` result produce, so the browser flow
    /// is unchanged. There is no open or stop method: the agent owns the
    /// canvas, the app only shows it.
    ///
    /// The canvas URL carries a token. It only ever reaches the browser notice
    /// for a loopback URL; reports never carry it and this slice keeps no copy.
    pub(in crate::features::session) mod design {
        use pi_rpc::UiRequest;
        use pi_rpc::desktop_rpc::{
            Capabilities, DesignCanvas, DesignTransition, DesktopEvent, DesktopMethod,
            DesktopRecord, DesktopResponse, DesktopRpcError, DesktopSession,
        };

        use super::Send;
        use crate::features::browser::CanvasNotice;

        /// The wire name of the only event this slice applies.
        pub(in crate::features::session) const DESIGN_CANVAS: &str = "design_canvas";

        /// What one decision produced. No `Debug`: `notice` carries the
        /// tokenized URL, which must never reach a log line.
        #[derive(Default)]
        pub(in crate::features::session) struct Applied {
            pub(in crate::features::session) sends: Vec<Send>,
            pub(in crate::features::session) report: Option<Report>,
            pub(in crate::features::session) notice: Option<CanvasNotice>,
        }

        /// Why a design request stopped or what it refused. None carries the
        /// canvas URL.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(in crate::features::session) enum Report {
            /// The extension does not advertise `get_design_canvas`.
            Unsupported,
            /// The server answered `unsupported_method` for it anyway.
            UnsupportedMethod,
            /// Capabilities are not known yet.
            NotReady,
            /// The request timed out before the server answered.
            Timeout,
            /// A transport, server or decoding failure.
            Failure(DesktopRpcError),
            /// A gap in the event sequence: the record was already accepted,
            /// so the wiring resyncs instead of accepting it again.
            OutOfOrderSequence,
            /// The canvas URL is not on this machine: nothing is opened.
            NotLoopback,
        }

        impl Report {
            /// What the conversation says about it. Never the canvas URL.
            pub(in crate::features::session) fn message(&self) -> String {
                match self {
                    Self::Unsupported | Self::UnsupportedMethod => {
                        "This Gentle Shell does not report its design canvas to Desktop."
                            .to_string()
                    }
                    Self::NotReady => {
                        "Gentle Desktop is still loading; try again in a moment.".to_string()
                    }
                    Self::Timeout => "The design canvas request timed out.".to_string(),
                    Self::Failure(error) => {
                        format!("The design canvas could not be read ({error}).")
                    }
                    Self::OutOfOrderSequence => {
                        "Gentle Desktop events were lost; resyncing the design canvas.".to_string()
                    }
                    Self::NotLoopback => {
                        "Gentle Desktop did not open a design canvas outside this machine."
                            .to_string()
                    }
                }
            }

            /// Whether the legacy canvas path stays in charge: the peer lacks
            /// the method or the channel has not probed yet. Those are silent;
            /// the `notify` text and `gentle_design` result keep working.
            pub(in crate::features::session) fn is_legacy(&self) -> bool {
                matches!(
                    self,
                    Self::Unsupported | Self::UnsupportedMethod | Self::NotReady
                )
            }
        }

        #[derive(Default)]
        pub(in crate::features::session) struct Design {
            capabilities: Option<Capabilities>,
            /// The seed was spent for this backend (asked, or known
            /// unsupported). A gap re-arms it.
            seeded: bool,
            /// Bumped per request, so an old timer cannot drop a newer one.
            generation: u64,
            /// The URL this slice last opened, so a re-seed or a repeated
            /// `open` does not reload the page. Never printed; the struct has
            /// no `Debug`.
            shown: Option<String>,
        }

        impl Design {
            pub(in crate::features::session) fn new() -> Self {
                Self::default()
            }

            /// Adopt the peer's capabilities once the probe completes.
            pub(in crate::features::session) fn set_capabilities(
                &mut self,
                capabilities: Capabilities,
            ) {
                self.capabilities = Some(capabilities);
            }

            /// Whether the peer advertised `design_canvas` events.
            pub(in crate::features::session) fn emits_events(&self) -> bool {
                self.capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.emits(DESIGN_CANVAS))
            }

            /// Ask for the canvas state once per backend, URL included: the
            /// interactive host is the trusted client the server hands it to.
            /// It waits for any in-flight request, so the probe and the other
            /// slices never issue two requests at once.
            pub(in crate::features::session) fn seed(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                if self.seeded {
                    return Applied::default();
                }
                let Some(capabilities) = &self.capabilities else {
                    return report(Report::NotReady);
                };
                if !capabilities.supports(DesktopMethod::GetDesignCanvas) {
                    // Said once; an old Shell keeps its legacy canvas path.
                    self.seeded = true;
                    return report(Report::Unsupported);
                }
                if session.pending().is_some() {
                    return Applied::default();
                }
                let request = match session.issue_get_design_canvas(Some(true)) {
                    Ok(request) => request,
                    Err(error) => return report(Report::Failure(error)),
                };
                self.seeded = true;
                self.generation += 1;
                Applied {
                    sends: vec![Send {
                        request,
                        generation: self.generation,
                    }],
                    ..Applied::default()
                }
            }

            /// Events were lost somewhere on the shared channel: ask again
            /// once the resync snapshot lands. A peer without the method stays
            /// spent, so its silent `Unsupported` is not repeated.
            pub(in crate::features::session) fn rearm(&mut self) {
                if self.capabilities.as_ref().is_some_and(|capabilities| {
                    capabilities.supports(DesktopMethod::GetDesignCanvas)
                }) {
                    self.seeded = false;
                }
            }

            /// Pi answered the outer `prompt` of the design request.
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
                report(Report::Failure(DesktopRpcError::UnexpectedDisposition {
                    disposition: None,
                }))
            }

            /// One incoming UI request; applies only the records this slice
            /// owns (the `get_design_canvas` answer and `design_canvas`).
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
                        Some(DesktopMethod::GetDesignCanvas) => self.apply_response(response),
                        _ => Applied::default(),
                    },
                    Ok(Some(DesktopRecord::Event(event))) => self.apply_event(event),
                    // Events were lost: the channel owns the get_snapshot
                    // resync, and the lost transition is re-read once it lands.
                    Err(DesktopRpcError::OutOfOrderSequence { .. }) => {
                        self.seeded = false;
                        report(Report::OutOfOrderSequence)
                    }
                    // Not this protocol's widget, or a late, repeated or
                    // malformed record: never a navigation.
                    _ => Applied::default(),
                }
            }

            /// The pending timeout for `generation` fired.
            pub(in crate::features::session) fn expire(
                &mut self,
                session: &mut DesktopSession,
                generation: u64,
            ) -> Applied {
                if generation != self.generation || !owns_pending(session) {
                    return Applied::default();
                }
                self.rollback(session);
                report(Report::Timeout)
            }

            /// The backend was replaced or exited: nothing it said or numbered
            /// applies, and the next probe re-arms one seed.
            pub(in crate::features::session) fn reset(&mut self, session: &mut DesktopSession) {
                session.reset();
                self.capabilities = None;
                self.seeded = false;
                self.generation += 1;
                self.shown = None;
            }

            /// Drop the request in flight. The generation moves, so a late
            /// timer or disposition for it is stale.
            fn rollback(&mut self, session: &mut DesktopSession) {
                session.abandon();
                self.generation += 1;
            }

            fn apply_response(&mut self, response: DesktopResponse) -> Applied {
                match response.design_canvas() {
                    Ok(canvas) => self.canvas_notice(None, &canvas),
                    Err(DesktopRpcError::RemoteError(info))
                        if info.code == "unsupported_method" =>
                    {
                        report(Report::UnsupportedMethod)
                    }
                    Err(error) => report(Report::Failure(error)),
                }
            }

            /// A `design_canvas` event, applied only when the peer advertised
            /// that exact name.
            fn apply_event(&mut self, event: DesktopEvent) -> Applied {
                if event.event != DESIGN_CANVAS || !self.emits_events() {
                    return Applied::default();
                }
                match event.design_canvas() {
                    Ok(event) => self.canvas_notice(Some(event.transition), &event.state),
                    Err(error) => report(Report::Failure(error)),
                }
            }

            /// The browser notice a canvas state means. `transition` is `None`
            /// for the seed, which never closes anything: `Stopped` closes
            /// design cards another tab may own, so only a `stop` event (or an
            /// event with the canvas not running) does. A running canvas opens
            /// its loopback URL, except the one already shown on a plain
            /// `open` or seed; a `refresh` always reloads. A running canvas
            /// without a URL navigates nowhere. Any other URL is refused, and
            /// the report does not repeat it.
            fn canvas_notice(
                &mut self,
                transition: Option<DesignTransition>,
                canvas: &DesignCanvas,
            ) -> Applied {
                if transition == Some(DesignTransition::Stop) || !canvas.running {
                    self.shown = None;
                    if transition.is_none() {
                        return Applied::default();
                    }
                    return Applied {
                        notice: Some(CanvasNotice::Stopped),
                        ..Applied::default()
                    };
                }
                let Some(url) = canvas.url.as_deref() else {
                    return Applied::default();
                };
                if !is_loopback(url) {
                    return report(Report::NotLoopback);
                }
                let refreshed = transition == Some(DesignTransition::Refresh);
                if !refreshed && self.shown.as_deref() == Some(url) {
                    return Applied::default();
                }
                self.shown = Some(url.to_string());
                Applied {
                    notice: Some(CanvasNotice::Open {
                        url: url.to_string(),
                        refreshed,
                    }),
                    ..Applied::default()
                }
            }
        }

        fn report(report: Report) -> Applied {
            Applied {
                report: Some(report),
                ..Applied::default()
            }
        }

        /// The browser's canvas rule, read back exactly so nothing around the
        /// URL (whitespace, a second URL) is taken instead; the loopback host
        /// parser also refuses a userinfo host such as `localhost:@host`.
        fn is_loopback(url: &str) -> bool {
            let browser = matches!(
                crate::features::browser::canvas_notice(&format!("Design canvas: {url}")),
                Some(CanvasNotice::Open { url: shown, .. }) if shown == url
            );
            browser && gentle_core::web::loopback_url(url).as_deref() == Some(url)
        }

        /// Whether the pending request is this slice's: the other slices
        /// share the session, so a stale design timer must not drop theirs.
        fn owns_pending(session: &DesktopSession) -> bool {
            session.pending().map(|(_, method)| method) == Some(DesktopMethod::GetDesignCanvas)
        }
    }

    /// Gentle Desktop's config stores in one tab.
    ///
    /// When the backend's `capabilities` advertise `get_config` and
    /// `set_config`, the Profiles screen reads and writes its stores through
    /// this slice by name, never by path: `profiles`, `profile_fallbacks` and
    /// `pins` are writable, `models`, `subagents` and `managed_assets` are
    /// read-only. The screen holds tickets; the slice queues its requests and
    /// sends one at a time on the shared [`DesktopSession`], so probe and the
    /// other slices keep one id space, one pending slot and one event
    /// baseline. Each ticket gets exactly one outcome: the answer, the failure,
    /// or `Dropped` when the backend went away first. `config_changed` events
    /// and gaps in the sequence become store changes the screen re-reads.
    ///
    /// The slice never retries a write and never touches a file: a
    /// `revision_conflict` is reported to the screen, which re-reads.
    pub(in crate::features::session) mod config {
        use std::collections::VecDeque;

        use pi_rpc::UiRequest;
        use pi_rpc::desktop_rpc::{
            Capabilities, ConfigStore, ConfigWrite, DesktopEvent, DesktopMethod, DesktopRecord,
            DesktopResponse, DesktopRpcError, DesktopSession,
        };

        use super::Send;
        use crate::features::profiles::{ConfigChange, ConfigFailure, ConfigOutcome, ConfigTicket};

        /// The wire name of the only event this slice applies.
        pub(in crate::features::session) const CONFIG_CHANGED: &str = "config_changed";

        /// What one decision produced: the requests to send, and a failure the
        /// tab shows through its report path. Outcomes for the screen are
        /// kept in the slice until it takes them.
        #[derive(Debug, Default)]
        pub(in crate::features::session) struct Applied {
            pub(in crate::features::session) sends: Vec<Send>,
            pub(in crate::features::session) report: Option<Report>,
        }

        /// Why a config request stopped, for the conversation. A documented
        /// config failure (`revision_conflict`, `invalid_data`…) is not here:
        /// it is the screen's to show, as an outcome. Asking before the probe
        /// (`NotReady`) or of a peer without the method (`Unsupported`) is
        /// refused at once, as the [`ConfigFailure`] the caller gets back.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(in crate::features::session) enum Report {
            /// The server answered `unsupported_method` for an advertised
            /// method.
            UnsupportedMethod,
            /// The request timed out before the server answered.
            Timeout,
            /// A transport, server or decoding failure.
            Failure(DesktopRpcError),
            /// A gap in the event sequence: the record was already accepted,
            /// so the wiring resyncs instead of accepting it again.
            OutOfOrderSequence,
        }

        impl Report {
            /// What the conversation says about it.
            pub(in crate::features::session) fn message(&self) -> String {
                match self {
                    Self::UnsupportedMethod => {
                        "This Gentle Shell does not serve its configuration to Desktop.".to_string()
                    }
                    Self::Timeout => "The configuration request timed out.".to_string(),
                    Self::Failure(error) => {
                        format!("The configuration request failed ({error}).")
                    }
                    Self::OutOfOrderSequence => {
                        "Gentle Desktop events were lost; re-reading the configuration.".to_string()
                    }
                }
            }

            /// Whether the Profiles screen keeps its direct-file path: the
            /// peer lacks the method. That is silent in the conversation.
            pub(in crate::features::session) fn is_legacy(&self) -> bool {
                matches!(self, Self::UnsupportedMethod)
            }
        }

        /// One queued request.
        #[derive(Debug)]
        enum Op {
            Get(ConfigStore),
            Set(ConfigWrite, String),
        }

        #[derive(Debug, Default)]
        pub(in crate::features::session) struct Config {
            capabilities: Option<Capabilities>,
            /// Requests waiting for the shared slot, in the order asked.
            queue: VecDeque<(ConfigTicket, Op)>,
            /// The ticket whose request is on the wire.
            in_flight: Option<ConfigTicket>,
            /// Never rewound, so a ticket from before a reset never matches.
            next_ticket: ConfigTicket,
            /// Bumped per request, so an old timer cannot drop a newer one.
            generation: u64,
            outcomes: Vec<(ConfigTicket, ConfigOutcome)>,
            changes: Vec<ConfigChange>,
        }

        impl Config {
            /// Adopt the peer's capabilities once the probe completes.
            pub(in crate::features::session) fn set_capabilities(
                &mut self,
                capabilities: Capabilities,
            ) {
                self.capabilities = Some(capabilities);
            }

            /// Whether the peer offers both config methods; `None` until the
            /// probe has said. Reading without writing cannot back an editor.
            pub(in crate::features::session) fn offers_config(&self) -> Option<bool> {
                self.capabilities.as_ref().map(|capabilities| {
                    capabilities.supports(DesktopMethod::GetConfig)
                        && capabilities.supports(DesktopMethod::SetConfig)
                })
            }

            /// Queue a read of one store.
            pub(in crate::features::session) fn get(
                &mut self,
                store: ConfigStore,
            ) -> Result<ConfigTicket, ConfigFailure> {
                self.check(DesktopMethod::GetConfig)?;
                Ok(self.enqueue(Op::Get(store)))
            }

            /// Queue a write guarded by `expected_revision`.
            pub(in crate::features::session) fn set(
                &mut self,
                write: ConfigWrite,
                expected_revision: String,
            ) -> Result<ConfigTicket, ConfigFailure> {
                self.check(DesktopMethod::SetConfig)?;
                Ok(self.enqueue(Op::Set(write, expected_revision)))
            }

            fn check(&self, method: DesktopMethod) -> Result<(), ConfigFailure> {
                match &self.capabilities {
                    None => Err(ConfigFailure::NotReady),
                    Some(capabilities) if !capabilities.supports(method) => {
                        Err(ConfigFailure::Unsupported)
                    }
                    Some(_) => Ok(()),
                }
            }

            fn enqueue(&mut self, op: Op) -> ConfigTicket {
                self.next_ticket += 1;
                self.queue.push_back((self.next_ticket, op));
                self.next_ticket
            }

            /// Send the next queued request when nothing is in flight on the
            /// shared session. A request that fails validation is answered at
            /// once and the next one is tried.
            pub(in crate::features::session) fn pump(
                &mut self,
                session: &mut DesktopSession,
            ) -> Applied {
                while self.in_flight.is_none() && session.pending().is_none() {
                    let Some((ticket, op)) = self.queue.pop_front() else {
                        break;
                    };
                    let request = match op {
                        Op::Get(store) => session.issue_get_config(store),
                        Op::Set(write, expected) => session.issue_set_config(write, expected),
                    };
                    match request {
                        Ok(request) => {
                            self.in_flight = Some(ticket);
                            self.generation += 1;
                            return Applied {
                                sends: vec![Send {
                                    request,
                                    generation: self.generation,
                                }],
                                report: None,
                            };
                        }
                        Err(error) => self.answer(
                            ticket,
                            ConfigOutcome::Failed(ConfigFailure::Failure(error.to_string())),
                        ),
                    }
                }
                Applied::default()
            }

            /// Pi answered the outer `prompt` of the config request.
            pub(in crate::features::session) fn outer(
                &mut self,
                session: &mut DesktopSession,
                generation: u64,
                handled: bool,
            ) -> Applied {
                if handled || generation != self.generation || !owns_pending(session) {
                    return Applied::default();
                }
                let error = DesktopRpcError::UnexpectedDisposition { disposition: None };
                self.rollback(
                    session,
                    ConfigOutcome::Failed(ConfigFailure::Failure(error.to_string())),
                );
                report(Report::Failure(error))
            }

            /// One incoming UI request; applies only the records this slice
            /// owns (the config answers and `config_changed`).
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
                        Some(method @ (DesktopMethod::GetConfig | DesktopMethod::SetConfig)) => {
                            self.apply_response(method, response)
                        }
                        _ => Applied::default(),
                    },
                    Ok(Some(DesktopRecord::Event(event))) => self.apply_event(event),
                    // Events were lost: the channel owns the get_snapshot
                    // resync, and every store is re-read behind it.
                    Err(DesktopRpcError::OutOfOrderSequence { .. }) => {
                        report(Report::OutOfOrderSequence)
                    }
                    // Not this protocol's widget, or a late, repeated or
                    // malformed record: never an outcome.
                    _ => Applied::default(),
                }
            }

            /// The pending timeout for `generation` fired.
            pub(in crate::features::session) fn expire(
                &mut self,
                session: &mut DesktopSession,
                generation: u64,
            ) -> Applied {
                if generation != self.generation || !owns_pending(session) {
                    return Applied::default();
                }
                self.rollback(session, ConfigOutcome::Failed(ConfigFailure::Timeout));
                report(Report::Timeout)
            }

            /// The backend was replaced or exited: every ticket still open is
            /// answered `Dropped`, and nothing it said or numbered applies.
            pub(in crate::features::session) fn reset(&mut self, session: &mut DesktopSession) {
                session.reset();
                self.capabilities = None;
                if let Some(ticket) = self.in_flight.take() {
                    self.answer(ticket, ConfigOutcome::Failed(ConfigFailure::Dropped));
                }
                self.drop_queued();
                self.changes.clear();
                self.generation += 1;
            }

            /// Answer every queued (not yet sent) ticket `Dropped`: the
            /// channel stopped and nothing will send them.
            pub(in crate::features::session) fn drop_queued(&mut self) {
                while let Some((ticket, _)) = self.queue.pop_front() {
                    self.answer(ticket, ConfigOutcome::Failed(ConfigFailure::Dropped));
                }
            }

            /// Events were lost somewhere on the shared channel: any store may
            /// have changed without its `config_changed`.
            pub(in crate::features::session) fn rearm(&mut self) {
                if self.offers_config() != Some(true) {
                    return;
                }
                for store in ConfigStore::ALL {
                    let change = ConfigChange {
                        store,
                        revision: None,
                    };
                    if !self.changes.contains(&change) {
                        self.changes.push(change);
                    }
                }
            }

            pub(in crate::features::session) fn take_outcomes(
                &mut self,
            ) -> Vec<(ConfigTicket, ConfigOutcome)> {
                std::mem::take(&mut self.outcomes)
            }

            pub(in crate::features::session) fn take_changes(&mut self) -> Vec<ConfigChange> {
                std::mem::take(&mut self.changes)
            }

            fn answer(&mut self, ticket: ConfigTicket, outcome: ConfigOutcome) {
                self.outcomes.push((ticket, outcome));
            }

            /// Drop the request in flight and answer its ticket. The
            /// generation moves, so a late timer or disposition is stale.
            fn rollback(&mut self, session: &mut DesktopSession, outcome: ConfigOutcome) {
                session.abandon();
                self.generation += 1;
                if let Some(ticket) = self.in_flight.take() {
                    self.answer(ticket, outcome);
                }
            }

            fn apply_response(
                &mut self,
                method: DesktopMethod,
                response: DesktopResponse,
            ) -> Applied {
                let Some(ticket) = self.in_flight.take() else {
                    return Applied::default();
                };
                let decoded = if method == DesktopMethod::GetConfig {
                    response.config().map(ConfigOutcome::Read)
                } else {
                    response.set_config().map(ConfigOutcome::Written)
                };
                let (outcome, reported) = match decoded {
                    Ok(outcome) => (outcome, None),
                    Err(DesktopRpcError::RemoteError(info))
                        if info.code == "unsupported_method" =>
                    {
                        (
                            ConfigOutcome::Failed(ConfigFailure::UnsupportedMethod),
                            Some(Report::UnsupportedMethod),
                        )
                    }
                    Err(DesktopRpcError::RemoteError(info)) if info.config_error().is_some() => {
                        let code = info.config_error().expect("checked by the guard");
                        (
                            ConfigOutcome::Failed(ConfigFailure::Rejected {
                                code,
                                message: info.message,
                            }),
                            None,
                        )
                    }
                    Err(error) => (
                        ConfigOutcome::Failed(ConfigFailure::Failure(error.to_string())),
                        Some(Report::Failure(error)),
                    ),
                };
                self.answer(ticket, outcome);
                Applied {
                    report: reported,
                    ..Applied::default()
                }
            }

            /// A `config_changed` event, applied only when the peer advertised
            /// that exact name.
            fn apply_event(&mut self, event: DesktopEvent) -> Applied {
                let advertised = self
                    .capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.emits(CONFIG_CHANGED));
                if event.event != CONFIG_CHANGED || !advertised {
                    return Applied::default();
                }
                match event.config_changed() {
                    Ok(changed) => {
                        self.changes.push(ConfigChange {
                            store: changed.store,
                            revision: Some(changed.revision),
                        });
                        Applied::default()
                    }
                    Err(error) => report(Report::Failure(error)),
                }
            }
        }

        fn report(report: Report) -> Applied {
            Applied {
                report: Some(report),
                ..Applied::default()
            }
        }

        /// Whether the pending request is this slice's: the other slices
        /// share the session, so a stale config timer must not drop theirs.
        fn owns_pending(session: &DesktopSession) -> bool {
            matches!(
                session.pending().map(|(_, method)| method),
                Some(DesktopMethod::GetConfig) | Some(DesktopMethod::SetConfig)
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use pi_rpc::UiMethod;
    use pi_rpc::UiRequest;
    use pi_rpc::desktop_rpc::{
        Capabilities, ConfigErrorCode, ConfigStore, ConfigWrite, DesktopMethod, DesktopSession,
        PROMPT_COMMAND, PermissionMode, ProfilesData, SCHEMA, UsageStatus, WIDGET_KEY,
    };
    use serde_json::{Value, json};

    use super::desktop::config::Report as ConfigReport;
    use super::desktop::design::{Applied as DesignApplied, Design, Report as DesignReport};
    use super::desktop::permission::{self, Permission, Report as PermissionReport};
    use super::desktop::subscription::{Report as SubscriptionReport, Subscription};
    use super::desktop::supervision::{Agents, Report as SupervisionReport};
    use super::desktop::{self, Desktop, Report, Step};
    use super::{ExtensionUi, ModelInfo, TodoIsland};
    use crate::features::browser::CanvasNotice;
    use crate::features::profiles::{ConfigChange, ConfigFailure, ConfigOutcome, ConfigSupport};
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

    // Gentle Desktop's subscription usage slice: providers and windows are
    // built directly, so no process runs and no file or network is touched.

    fn subscription_ready(events: &[&str]) -> (Subscription, DesktopSession) {
        let mut subscription = Subscription::new();
        subscription.set_capabilities(supervision_capabilities(
            &["get_subscription_usage", "refresh_subscription_usage"],
            events,
        ));
        (subscription, DesktopSession::new())
    }

    fn usage_response_lines(request_id: &str, providers: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": {"providers": providers},
        }))
    }

    fn provider_json(provider: &str, status: &str, limits: Value) -> Value {
        json!({ "provider": provider, "status": status, "limits": limits })
    }

    /// Claude's 5h and weekly windows, exactly as a `rate_limit` snapshot
    /// carries them from the response headers.
    fn claude_provider(status: &str) -> Value {
        json!({
            "provider": "claude",
            "status": status,
            "fetchedAt": 1788600000000u64,
            "limits": [{
                "name": "prompts",
                "limitReached": false,
                "windows": [
                    {"label": "5h", "usedPercent": 42.5, "windowSeconds": 18000, "resetAt": null},
                    {"label": "week", "usedPercent": 10.0, "windowSeconds": 604800, "resetAt": 1789200000000u64},
                ],
            }],
        })
    }

    fn rate_limit_lines(seq: u64, source: &str, snapshots: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": seq,
            "event": "rate_limit",
            "data": {"source": source, "snapshots": snapshots},
        }))
    }

    #[test]
    fn subscription_seed_issues_get_once_and_waits_for_pending() {
        let (mut subscription, mut session) = subscription_ready(&["rate_limit"]);

        // A request in flight (the probe's snapshot, say) defers the seed.
        let probe = session.issue(DesktopMethod::GetSnapshot).unwrap();
        assert!(subscription.seed(&mut session).sends.is_empty());
        session
            .accept(&desktop_widget(snapshot_lines(probe.request_id(), 4)))
            .unwrap();

        let seeded = subscription.seed(&mut session);
        assert_eq!(seeded.sends.len(), 1);
        assert_eq!(
            seeded.sends[0].request.method(),
            DesktopMethod::GetSubscriptionUsage
        );
        let payload: Value = serde_json::from_str(seeded.sends[0].request.payload()).unwrap();
        assert_eq!(payload["method"], "get_subscription_usage");
        assert_eq!(payload["params"], json!({}));

        // Seeded exactly once: a second seed sends nothing while it is pending
        // or after it is answered. The snapshot reports the in-flight request
        // so the screen stays on Loading rather than flashing the empty hint.
        assert!(subscription.seed(&mut session).sends.is_empty());
        assert!(subscription.snapshot().pending);
        assert!(subscription.snapshot().providers.is_empty());
        let id = seeded.sends[0].request.request_id().to_string();
        let applied = subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(
                &id,
                json!([provider_json("codex", "ok", json!([]))]),
            )),
        );
        assert!(applied.report.is_none());
        assert!(!subscription.snapshot().pending);
        assert_eq!(subscription.snapshot().providers.len(), 1);
        assert!(subscription.seed(&mut session).sends.is_empty());
    }

    #[test]
    fn subscription_refresh_uses_the_refresh_method() {
        let (mut subscription, mut session) = subscription_ready(&[]);

        let seeded = subscription.seed(&mut session);
        let id = seeded.sends[0].request.request_id().to_string();
        subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(&id, json!([]))),
        );

        let refreshed = subscription.refresh(&mut session);
        assert_eq!(refreshed.sends.len(), 1);
        assert_eq!(
            refreshed.sends[0].request.method(),
            DesktopMethod::RefreshSubscriptionUsage
        );
        let payload: Value = serde_json::from_str(refreshed.sends[0].request.payload()).unwrap();
        assert_eq!(payload["method"], "refresh_subscription_usage");
        assert_eq!(payload["params"], json!({}));
    }

    #[test]
    fn subscription_decodes_every_status_into_the_snapshot() {
        for status in UsageStatus::ALL {
            let (mut subscription, mut session) = subscription_ready(&[]);
            let seeded = subscription.seed(&mut session);
            let id = seeded.sends[0].request.request_id().to_string();
            let applied = subscription.apply(
                &mut session,
                &desktop_widget(usage_response_lines(
                    &id,
                    json!([provider_json("codex", status.as_str(), json!([]))]),
                )),
            );
            assert!(applied.report.is_none(), "{status:?} should decode");
            let snapshot = subscription.snapshot();
            assert_eq!(snapshot.providers.len(), 1);
            assert_eq!(snapshot.providers[0].status, status);
        }
    }

    #[test]
    fn subscription_keeps_last_good_limits_for_pending_and_stale() {
        let (mut subscription, mut session) = subscription_ready(&[]);
        let seeded = subscription.seed(&mut session);
        let id = seeded.sends[0].request.request_id().to_string();
        let window = json!([{
            "label": "5h", "usedPercent": 42.5, "windowSeconds": 18000, "resetAt": null,
        }]);
        let limit = json!([{ "name": "prompts", "limitReached": false, "windows": window }]);

        // A good read seeds the limits.
        subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(
                &id,
                json!([provider_json("codex", "ok", limit.clone())]),
            )),
        );
        assert_eq!(subscription.snapshot().providers[0].limits.len(), 1);

        // A stale read with no limits keeps the last good window, marked stale.
        let refreshed = subscription.refresh(&mut session);
        let id = refreshed.sends[0].request.request_id().to_string();
        subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(
                &id,
                json!([provider_json("codex", "stale", json!([]))]),
            )),
        );
        let snapshot = subscription.snapshot();
        assert_eq!(snapshot.providers[0].status, UsageStatus::Stale);
        assert_eq!(snapshot.providers[0].limits.len(), 1, "last good kept");

        // A pending read with no limits keeps it too.
        let refreshed = subscription.refresh(&mut session);
        let id = refreshed.sends[0].request.request_id().to_string();
        subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(
                &id,
                json!([provider_json("codex", "pending", json!([]))]),
            )),
        );
        assert_eq!(
            subscription.snapshot().providers[0].status,
            UsageStatus::Pending
        );
        assert_eq!(subscription.snapshot().providers[0].limits.len(), 1);

        // A failed read is shown honestly: no limits are invented.
        let refreshed = subscription.refresh(&mut session);
        let id = refreshed.sends[0].request.request_id().to_string();
        subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(
                &id,
                json!([provider_json("codex", "failed", json!([]))]),
            )),
        );
        assert_eq!(
            subscription.snapshot().providers[0].status,
            UsageStatus::Failed
        );
        assert!(subscription.snapshot().providers[0].limits.is_empty());
    }

    #[test]
    fn subscription_reconciles_claude_rate_limit_snapshots() {
        let (mut subscription, mut session) = subscription_ready(&["rate_limit"]);

        let applied = subscription.apply(
            &mut session,
            &desktop_widget(rate_limit_lines(
                1,
                "response",
                json!([claude_provider("ok")]),
            )),
        );
        assert!(applied.report.is_none());
        let snapshot = subscription.snapshot();
        assert_eq!(snapshot.providers.len(), 1);
        assert_eq!(snapshot.providers[0].provider, "claude");
        assert_eq!(snapshot.providers[0].limits.len(), 1);
        assert_eq!(snapshot.providers[0].limits[0].windows.len(), 2);
        assert_eq!(snapshot.providers[0].limits[0].windows[0].label, "5h");
        assert_eq!(snapshot.providers[0].limits[0].windows[1].label, "week");

        // A later event replaces the same provider by id (full projection).
        let applied = subscription.apply(
            &mut session,
            &desktop_widget(rate_limit_lines(
                2,
                "refresh",
                json!([provider_json("claude", "stale", json!([]))]),
            )),
        );
        assert!(applied.report.is_none());
        let snapshot = subscription.snapshot();
        assert_eq!(snapshot.providers.len(), 1);
        assert_eq!(snapshot.providers[0].status, UsageStatus::Stale);
        // Last good limits are kept across the event too.
        assert_eq!(snapshot.providers[0].limits.len(), 1);
    }

    #[test]
    fn subscription_foreign_and_unadvertised_events_do_not_touch_the_snapshot() {
        let (mut subscription, mut session) = subscription_ready(&["rate_limit"]);

        // A foreign event name is never applied, even when it fits the shape.
        let foreign = subscription.apply(
            &mut session,
            &desktop_widget(record(json!({
                "schema": SCHEMA, "kind": "event", "seq": 1,
                "event": "agents_update",
                "data": {"source": "refresh", "snapshots": []},
            }))),
        );
        assert!(foreign.report.is_none());
        assert!(subscription.snapshot().providers.is_empty());

        // A rate_limit event the peer never advertised is ignored too.
        let (mut unadvertised, mut unadvertised_session) = subscription_ready(&["state_changed"]);
        let applied = unadvertised.apply(
            &mut unadvertised_session,
            &desktop_widget(rate_limit_lines(
                1,
                "response",
                json!([claude_provider("ok")]),
            )),
        );
        assert!(applied.report.is_none());
        assert!(unadvertised.snapshot().providers.is_empty());
    }

    #[test]
    fn subscription_legacy_signal_is_unsupported_or_unsupported_method_only() {
        // Capabilities known but without the method: legacy, not an error.
        let mut missing = Subscription::new();
        missing.set_capabilities(supervision_capabilities(&["get_snapshot"], &[]));
        let mut missing_session = DesktopSession::new();
        let applied = missing.seed(&mut missing_session);
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(SubscriptionReport::Unsupported));
        let snapshot = missing.snapshot();
        assert!(snapshot.legacy);
        assert!(!snapshot.not_ready);
        assert!(snapshot.error.is_none());

        // The server answers unsupported_method for a method it advertised.
        let (mut claimed, mut claimed_session) = subscription_ready(&[]);
        let seeded = claimed.seed(&mut claimed_session);
        let id = seeded.sends[0].request.request_id().to_string();
        let applied = claimed.apply(
            &mut claimed_session,
            &desktop_widget(error_lines(&id, "unsupported_method", "nope")),
        );
        assert_eq!(applied.report, Some(SubscriptionReport::UnsupportedMethod));
        assert!(claimed.snapshot().legacy);
        assert!(claimed.snapshot().error.is_none());

        // Any other failure is neither legacy, and it surfaces as an error.
        let (mut failed, mut failed_session) = subscription_ready(&[]);
        let seeded = failed.seed(&mut failed_session);
        let id = seeded.sends[0].request.request_id().to_string();
        let applied = failed.apply(
            &mut failed_session,
            &desktop_widget(record(json!({
                "schema": SCHEMA, "kind": "response", "requestId": id, "ok": true,
                "data": {"providers": "nope"},
            }))),
        );
        assert!(matches!(
            applied.report,
            Some(SubscriptionReport::Failure(_))
        ));
        let snapshot = failed.snapshot();
        assert!(!snapshot.legacy);
        assert!(snapshot.error.is_some());
    }

    #[test]
    fn subscription_issues_only_rpc_until_unsupported_is_confirmed() {
        let (mut subscription, mut session) = subscription_ready(&[]);

        // Before any confirmation, the slice's only effect is the RPC get
        // request; it never issues a direct provider API call.
        let seeded = subscription.seed(&mut session);
        assert_eq!(seeded.sends.len(), 1);
        assert_eq!(
            seeded.sends[0].request.method(),
            DesktopMethod::GetSubscriptionUsage
        );
        assert!(!subscription.snapshot().legacy);
        let id = seeded.sends[0].request.request_id().to_string();

        // The unsupported_method answer is what flips the legacy signal; it is
        // not an error, so the Usage screen switches once and then stays there.
        let applied = subscription.apply(
            &mut session,
            &desktop_widget(error_lines(&id, "unsupported_method", "nope")),
        );
        assert_eq!(applied.report, Some(SubscriptionReport::UnsupportedMethod));
        let snapshot = subscription.snapshot();
        assert!(snapshot.legacy);
        assert!(snapshot.error.is_none());
    }

    #[test]
    fn subscription_without_capabilities_is_not_ready_not_legacy() {
        let mut subscription = Subscription::new();
        let mut session = DesktopSession::new();
        let applied = subscription.seed(&mut session);
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(SubscriptionReport::NotReady));
        let snapshot = subscription.snapshot();
        assert!(snapshot.not_ready);
        assert!(!snapshot.legacy);

        // Refresh without capabilities is also NotReady.
        let applied = subscription.refresh(&mut session);
        assert_eq!(applied.report, Some(SubscriptionReport::NotReady));
    }

    #[test]
    fn subscription_reset_forgets_state_and_a_late_response_is_benign() {
        let (mut subscription, mut session) = subscription_ready(&["rate_limit"]);
        let seeded = subscription.seed(&mut session);
        let id = seeded.sends[0].request.request_id().to_string();
        subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(&id, json!([claude_provider("ok")]))),
        );
        assert_eq!(subscription.snapshot().providers.len(), 1);

        subscription.reset(&mut session);
        assert!(subscription.snapshot().providers.is_empty());
        assert!(subscription.snapshot().not_ready);

        // The old answer arrives late: not applied, never reported.
        let applied = subscription.apply(
            &mut session,
            &desktop_widget(usage_response_lines(
                &id,
                json!([claude_provider("failed")]),
            )),
        );
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert!(subscription.snapshot().providers.is_empty());
    }

    #[test]
    fn subscription_timeout_and_unhandled_outer_roll_back() {
        let (mut subscription, mut session) = subscription_ready(&[]);
        let seeded = subscription.seed(&mut session);
        let generation = seeded.sends[0].generation;

        // A stale timer cannot drop a newer request.
        assert!(
            subscription
                .expire(&mut session, generation.wrapping_sub(1))
                .sends
                .is_empty()
        );

        // Its own timer drops the request and reports the timeout.
        let timed_out = subscription.expire(&mut session, generation);
        assert!(timed_out.sends.is_empty());
        assert_eq!(timed_out.report, Some(SubscriptionReport::Timeout));
        assert!(subscription.snapshot().error.is_some());

        // A rejected outer prompt rolls back and reports a transport failure.
        let refreshed = subscription.refresh(&mut session);
        let generation = refreshed.sends[0].generation;
        let applied = subscription.outer(&mut session, generation, false);
        assert!(matches!(
            applied.report,
            Some(SubscriptionReport::Failure(_))
        ));
    }

    #[test]
    fn subscription_refuses_a_second_request_while_one_is_in_flight() {
        let (mut subscription, mut session) = subscription_ready(&[]);
        let seeded = subscription.seed(&mut session);
        assert_eq!(seeded.sends.len(), 1);

        let second = subscription.refresh(&mut session);
        assert!(second.sends.is_empty());
        assert!(matches!(
            second.report,
            Some(SubscriptionReport::Failure(_))
        ));
    }

    #[test]
    fn subscription_gap_signals_out_of_order_without_fabricating_state() {
        let mut desktop = probed(4);
        let mut subscription = Subscription::new();
        subscription.set_capabilities(supervision_capabilities(
            &["get_subscription_usage", "refresh_subscription_usage"],
            &["rate_limit"],
        ));

        // An event that skipped a sequence surfaces the gap instead of applying.
        let applied = subscription.apply(
            desktop.session(),
            &desktop_widget(rate_limit_lines(
                6,
                "response",
                json!([claude_provider("ok")]),
            )),
        );
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(SubscriptionReport::OutOfOrderSequence));
        assert!(subscription.snapshot().providers.is_empty());

        // The channel owns the recovery: exactly one get_snapshot.
        let sends = desktop.resync();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        assert!(desktop.resync().is_empty());
    }

    // Gentle Desktop's design canvas slice: the canvas state is hand-built, so
    // no canvas server, browser or process runs.

    /// A tokenized loopback canvas URL, as the server hands it out.
    const CANVAS_URL: &str = "http://127.0.0.1:4100/tok-s3cret/d/landing/";

    fn design_ready(events: &[&str]) -> (Design, DesktopSession) {
        let mut design = Design::new();
        design.set_capabilities(supervision_capabilities(&["get_design_canvas"], events));
        (design, DesktopSession::new())
    }

    fn canvas_json(running: bool, url: Option<&str>) -> Value {
        let mut data = json!({
            "running": running,
            "slug": if running { json!("landing") } else { Value::Null },
            "connectedClients": 1,
            "designs": [{"slug": "landing", "title": "Landing", "mtime": 1.5}],
        });
        if let Some(url) = url {
            data["url"] = url.into();
        }
        data
    }

    fn design_response_lines(request_id: &str, data: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": data,
        }))
    }

    fn design_event_lines(seq: u64, transition: &str, mut data: Value) -> Vec<String> {
        data["transition"] = transition.into();
        record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": seq,
            "event": "design_canvas",
            "data": data,
        }))
    }

    /// Seed the slice and answer the seed with `data`.
    fn seed_design(
        design: &mut Design,
        session: &mut DesktopSession,
        data: Value,
    ) -> DesignApplied {
        let seeded = design.seed(session);
        assert_eq!(seeded.sends.len(), 1);
        let id = seeded.sends[0].request.request_id().to_string();
        design.apply(session, &desktop_widget(design_response_lines(&id, data)))
    }

    /// Whether `notice` opens `url`. A predicate rather than `assert_eq!`, so
    /// a failing test never prints the tokenized URL.
    fn opens(notice: &Option<CanvasNotice>, url: &str, refreshed: bool) -> bool {
        matches!(
            notice,
            Some(CanvasNotice::Open { url: shown, refreshed: shown_refreshed })
                if shown == url && *shown_refreshed == refreshed
        )
    }

    /// Whether `notice` closes the canvas, without printing it on failure.
    fn stops(notice: &Option<CanvasNotice>) -> bool {
        matches!(notice, Some(CanvasNotice::Stopped))
    }

    #[test]
    fn design_seed_asks_once_with_the_url_and_waits_for_pending() {
        let (mut design, mut session) = design_ready(&["design_canvas"]);

        // Another request in flight defers the seed without spending it.
        let probe = session.issue(DesktopMethod::GetSnapshot).unwrap();
        assert!(design.seed(&mut session).sends.is_empty());
        session
            .accept(&desktop_widget(snapshot_lines(probe.request_id(), 4)))
            .unwrap();

        let seeded = design.seed(&mut session);
        assert_eq!(seeded.sends.len(), 1);
        assert_eq!(
            seeded.sends[0].request.method(),
            DesktopMethod::GetDesignCanvas
        );
        let payload: Value = serde_json::from_str(seeded.sends[0].request.payload()).unwrap();
        assert_eq!(payload["method"], "get_design_canvas");
        assert_eq!(payload["params"], json!({"includeUrl": true}));

        // One in flight, and seeded exactly once per backend.
        assert!(design.seed(&mut session).sends.is_empty());
        let id = seeded.sends[0].request.request_id().to_string();
        let applied = design.apply(
            &mut session,
            &desktop_widget(design_response_lines(
                &id,
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(applied.report.is_none());
        assert!(design.seed(&mut session).sends.is_empty());
    }

    #[test]
    fn design_seed_state_maps_to_the_browser_notice() {
        // Running on loopback: open the canvas, not as a refresh.
        let (mut design, mut session) = design_ready(&[]);
        let applied = seed_design(
            &mut design,
            &mut session,
            canvas_json(true, Some(CANVAS_URL)),
        );
        assert!(opens(&applied.notice, CANVAS_URL, false));
        assert!(applied.report.is_none());

        // Not running: a seed never closes the canvas, because `Stopped`
        // closes design cards another tab may own. Only an event may stop it.
        let (mut design, mut session) = design_ready(&[]);
        let applied = seed_design(&mut design, &mut session, canvas_json(false, None));
        assert!(applied.notice.is_none());
        assert!(applied.report.is_none());

        // Running without a URL: nothing to navigate to, nothing to report.
        let (mut design, mut session) = design_ready(&[]);
        let applied = seed_design(&mut design, &mut session, canvas_json(true, None));
        assert!(applied.notice.is_none());
        assert!(applied.report.is_none());
    }

    #[test]
    fn design_events_open_refresh_and_stop_the_canvas() {
        let (mut design, mut session) = design_ready(&["design_canvas"]);
        assert!(design.emits_events());

        let opened = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                1,
                "open",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(opens(&opened.notice, CANVAS_URL, false));
        assert!(opened.report.is_none() && opened.sends.is_empty());

        let refreshed = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                2,
                "refresh",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(opens(&refreshed.notice, CANVAS_URL, true));

        let stopped = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(3, "stop", canvas_json(false, None))),
        );
        assert!(stops(&stopped.notice));

        // A stop wins over a URL the state still carries.
        let stale = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                4,
                "stop",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(stops(&stale.notice));

        // An open without a URL navigates nowhere.
        let bare = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(5, "open", canvas_json(true, None))),
        );
        assert!(bare.notice.is_none() && bare.report.is_none());
    }

    #[test]
    fn design_events_need_the_exact_advertised_name() {
        // Not advertised: accepted for sequencing, never shown.
        let (mut design, mut session) = design_ready(&[]);
        assert!(!design.emits_events());
        let applied = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                1,
                "open",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(applied.notice.is_none() && applied.report.is_none());

        // An unknown transition is a failure, not a navigation.
        let (mut design, mut session) = design_ready(&["design_canvas"]);
        let applied = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                1,
                "close",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(applied.notice.is_none());
        assert!(matches!(applied.report, Some(DesignReport::Failure(_))));
    }

    #[test]
    fn design_never_navigates_off_loopback() {
        for (case, url) in [
            "https://example.com/tok-s3cret/d/landing/",
            "http://10.0.0.2:4100/tok-s3cret/",
            "http://localhost:@example.com/tok-s3cret/",
            "http://127.0.0.1:4100/tok s3cret/",
            "file:///tmp/tok-s3cret",
        ]
        .into_iter()
        .enumerate()
        {
            let (mut design, mut session) = design_ready(&["design_canvas"]);
            let seeded = seed_design(&mut design, &mut session, canvas_json(true, Some(url)));
            assert!(seeded.notice.is_none(), "case {case}");
            assert_eq!(
                seeded.report,
                Some(DesignReport::NotLoopback),
                "case {case}"
            );

            let event = design.apply(
                &mut session,
                &desktop_widget(design_event_lines(
                    1,
                    "refresh",
                    canvas_json(true, Some(url)),
                )),
            );
            assert!(event.notice.is_none(), "case {case}");
            assert_eq!(event.report, Some(DesignReport::NotLoopback), "case {case}");
        }
    }

    #[test]
    fn design_reports_never_carry_the_canvas_url() {
        let reports = [
            DesignReport::Unsupported,
            DesignReport::UnsupportedMethod,
            DesignReport::NotReady,
            DesignReport::Timeout,
            DesignReport::Failure(pi_rpc::desktop_rpc::DesktopRpcError::MalformedPayload(
                "url must be a string".into(),
            )),
            DesignReport::OutOfOrderSequence,
            DesignReport::NotLoopback,
        ];
        for report in &reports {
            let message = report.message();
            assert!(!message.is_empty(), "{report:?}");
            assert!(!message.contains("tok-s3cret"), "{message}");
            assert!(!format!("{report:?}").contains("tok-s3cret"));
        }

        // What a refused or broken record actually reports omits it too.
        let (mut design, mut session) = design_ready(&["design_canvas"]);
        let refused = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                1,
                "open",
                canvas_json(true, Some("https://example.com/tok-s3cret/")),
            )),
        );
        let malformed = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(2, "open", json!({"url": CANVAS_URL}))),
        );
        for applied in [refused, malformed] {
            let report = applied.report.expect("a report");
            assert!(!report.message().contains("tok-s3cret"));
            assert!(!format!("{report:?}").contains("tok-s3cret"));
        }
    }

    #[test]
    fn design_unsupported_and_not_ready_stay_legacy_signals() {
        // Capabilities without the method: reported once, never asked.
        let mut design = Design::new();
        design.set_capabilities(supervision_capabilities(&["capabilities"], &[]));
        let mut session = DesktopSession::new();
        let applied = design.seed(&mut session);
        assert!(applied.sends.is_empty());
        assert_eq!(applied.report, Some(DesignReport::Unsupported));
        assert!(design.seed(&mut session).report.is_none());
        assert!(DesignReport::Unsupported.is_legacy());

        // No capabilities yet: not ready, and the seed is not spent.
        let mut early = Design::new();
        let applied = early.seed(&mut session);
        assert_eq!(applied.report, Some(DesignReport::NotReady));
        assert!(DesignReport::NotReady.is_legacy());

        // Advertised but refused by the server.
        let (mut claimed, mut claimed_session) = design_ready(&[]);
        let seeded = claimed.seed(&mut claimed_session);
        let id = seeded.sends[0].request.request_id().to_string();
        let applied = claimed.apply(
            &mut claimed_session,
            &desktop_widget(error_lines(&id, "unsupported_method", "nope")),
        );
        assert_eq!(applied.report, Some(DesignReport::UnsupportedMethod));
        assert!(applied.notice.is_none());
        assert!(DesignReport::UnsupportedMethod.is_legacy());
        assert!(!DesignReport::Timeout.is_legacy());
    }

    #[test]
    fn design_timeout_and_unhandled_outer_roll_back() {
        let (mut design, mut session) = design_ready(&[]);
        let seeded = design.seed(&mut session);
        let generation = seeded.sends[0].generation;
        let id = seeded.sends[0].request.request_id().to_string();

        // A stale timer drops nothing.
        let stale = design.expire(&mut session, generation.wrapping_sub(1));
        assert!(stale.report.is_none());
        assert!(session.pending().is_some());

        let timed_out = design.expire(&mut session, generation);
        assert_eq!(timed_out.report, Some(DesignReport::Timeout));
        assert!(session.pending().is_none());

        // The answer that arrives after the timeout navigates nowhere.
        let late = design.apply(
            &mut session,
            &desktop_widget(design_response_lines(
                &id,
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(late.notice.is_none() && late.report.is_none());

        // An unhandled outer prompt rolls back the same way.
        let (mut design, mut session) = design_ready(&[]);
        let seeded = design.seed(&mut session);
        let applied = design.outer(&mut session, seeded.sends[0].generation, false);
        assert!(matches!(applied.report, Some(DesignReport::Failure(_))));
        assert!(session.pending().is_none());

        // A design timer never drops another slice's request.
        let (mut design, mut session) = design_ready(&[]);
        let seeded = design.seed(&mut session);
        let generation = seeded.sends[0].generation;
        session.abandon();
        let _other = session.issue(DesktopMethod::GetSnapshot).unwrap();
        assert!(design.expire(&mut session, generation).report.is_none());
        assert_eq!(
            session.pending().map(|(_, method)| method),
            Some(DesktopMethod::GetSnapshot)
        );
    }

    #[test]
    fn design_does_not_reopen_the_url_it_already_shows() {
        let other = "http://127.0.0.1:4100/tok-s3cret/d/pricing/";
        let (mut design, mut session) = design_ready(&["design_canvas"]);
        let seeded = seed_design(
            &mut design,
            &mut session,
            canvas_json(true, Some(CANVAS_URL)),
        );
        assert!(opens(&seeded.notice, CANVAS_URL, false));

        // The same URL opened again would only reload the page.
        let again = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                1,
                "open",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(again.notice.is_none() && again.report.is_none());

        // A refresh is asked for: it still reloads.
        let refreshed = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                2,
                "refresh",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(opens(&refreshed.notice, CANVAS_URL, true));

        // Another design navigates.
        let moved = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                3,
                "open",
                canvas_json(true, Some(other)),
            )),
        );
        assert!(opens(&moved.notice, other, false));

        // After a stop, the same URL is a new open.
        let stopped = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(4, "stop", canvas_json(false, None))),
        );
        assert!(stops(&stopped.notice));
        let reopened = design.apply(
            &mut session,
            &desktop_widget(design_event_lines(
                5,
                "open",
                canvas_json(true, Some(other)),
            )),
        );
        assert!(opens(&reopened.notice, other, false));
    }

    #[test]
    fn design_reseed_keeps_the_page_and_reset_forgets_it() {
        let mut desktop = probed(4);
        let mut design = Design::new();
        let capabilities = supervision_capabilities(&["get_design_canvas"], &["design_canvas"]);
        design.set_capabilities(capabilities.clone());
        let seeded = seed_design(
            &mut design,
            desktop.session(),
            canvas_json(true, Some(CANVAS_URL)),
        );
        assert!(opens(&seeded.notice, CANVAS_URL, false));

        // A gap re-seeds; the same URL does not reload the page.
        let gap = design.apply(
            desktop.session(),
            &desktop_widget(design_event_lines(
                6,
                "open",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert_eq!(gap.report, Some(DesignReport::OutOfOrderSequence));
        let sends = desktop.resync();
        let snapshot_id = sends[0].request.request_id().to_string();
        desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, 6)));
        let reseeded = seed_design(
            &mut design,
            desktop.session(),
            canvas_json(true, Some(CANVAS_URL)),
        );
        assert!(reseeded.notice.is_none() && reseeded.report.is_none());

        // A new backend forgets what was shown: the same URL opens again.
        design.reset(desktop.session());
        design.set_capabilities(capabilities);
        let fresh = seed_design(
            &mut design,
            desktop.session(),
            canvas_json(true, Some(CANVAS_URL)),
        );
        assert!(opens(&fresh.notice, CANVAS_URL, false));
    }

    #[test]
    fn design_reset_reseeds_and_a_late_response_is_benign() {
        let (mut design, mut session) = design_ready(&["design_canvas"]);
        let seeded = design.seed(&mut session);
        let id = seeded.sends[0].request.request_id().to_string();

        design.reset(&mut session);
        assert!(!design.emits_events());
        let late = design.apply(
            &mut session,
            &desktop_widget(design_response_lines(
                &id,
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(late.notice.is_none() && late.report.is_none());

        // The new backend's probe re-arms one seed.
        design.set_capabilities(supervision_capabilities(
            &["get_design_canvas"],
            &["design_canvas"],
        ));
        assert_eq!(design.seed(&mut session).sends.len(), 1);
    }

    #[test]
    fn design_gap_reports_out_of_order_and_reseeds_after_resync() {
        let mut desktop = probed(4);
        let mut design = Design::new();
        design.set_capabilities(supervision_capabilities(
            &["get_design_canvas"],
            &["design_canvas"],
        ));
        let seeded = design.seed(desktop.session());
        let id = seeded.sends[0].request.request_id().to_string();
        design.apply(
            desktop.session(),
            &desktop_widget(design_response_lines(&id, canvas_json(false, None))),
        );

        // A skipped sequence surfaces the gap instead of navigating.
        let applied = design.apply(
            desktop.session(),
            &desktop_widget(design_event_lines(
                6,
                "open",
                canvas_json(true, Some(CANVAS_URL)),
            )),
        );
        assert!(applied.notice.is_none() && applied.sends.is_empty());
        assert_eq!(applied.report, Some(DesignReport::OutOfOrderSequence));

        // The channel owns the get_snapshot; the lost transition is re-read
        // once that lands.
        let sends = desktop.resync();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        assert!(design.seed(desktop.session()).sends.is_empty());
        let snapshot_id = sends[0].request.request_id().to_string();
        desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, 6)));
        let reseeded = design.seed(desktop.session());
        assert_eq!(reseeded.sends.len(), 1);
        assert_eq!(
            reseeded.sends[0].request.method(),
            DesktopMethod::GetDesignCanvas
        );
    }

    /// A Ready channel whose peer offers the design canvas, with its seed
    /// answered (not running), at sequence `sequence`.
    fn design_probed(sequence: u64) -> Desktop {
        let mut desktop = Desktop::new();
        let probe = desktop.ready(true);
        let _ = desktop.outer(probe.sends[0].generation, true);
        let applied = desktop.ui(&desktop_widget(record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": probe.sends[0].request.request_id(),
            "ok": true,
            "data": {
                "schema": SCHEMA,
                "command": PROMPT_COMMAND,
                "widget": WIDGET_KEY,
                "methods": ["capabilities", "get_snapshot", "get_design_canvas"],
                "events": ["state_changed", "design_canvas"],
            },
        }))));
        let snapshot_id = applied.sends[0].request.request_id().to_string();
        let _ = desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, sequence)));
        answer_design_seed(&mut desktop);
        assert!(desktop.design_seed().sends.is_empty());
        desktop
    }

    /// Issue the design seed through the wrapper and answer it.
    fn answer_design_seed(desktop: &mut Desktop) {
        let seeded = desktop.design_seed();
        assert_eq!(seeded.sends.len(), 1);
        assert_eq!(
            seeded.sends[0].request.method(),
            DesktopMethod::GetDesignCanvas
        );
        let id = seeded.sends[0].request.request_id().to_string();
        let _ = desktop.design_apply(&desktop_widget(design_response_lines(
            &id,
            canvas_json(false, None),
        )));
    }

    /// Answer the channel's pending resync snapshot at `sequence`.
    fn land_resync(desktop: &mut Desktop, sends: &[desktop::Send], sequence: u64) {
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        let id = sends[0].request.request_id().to_string();
        let _ = desktop.ui(&desktop_widget(snapshot_lines(&id, sequence)));
    }

    #[test]
    fn design_reseeds_once_after_a_gap_seen_by_another_record() {
        // The gap shows on a channel event: the channel resyncs, and the
        // design canvas re-seeds once behind the snapshot, not before.
        let mut desktop = design_probed(4);
        let gap = desktop.ui(&desktop_widget(event_lines(6)));
        assert!(desktop.design_seed().sends.is_empty());
        land_resync(&mut desktop, &gap.sends, 6);
        answer_design_seed(&mut desktop);
        assert!(desktop.design_seed().sends.is_empty());

        // A gap another slice saw (supervision, subscription, permission)
        // reaches the channel as `resync()`: the same single re-seed.
        let mut desktop = design_probed(4);
        let sends = desktop.resync();
        assert!(desktop.design_seed().sends.is_empty());
        land_resync(&mut desktop, &sends, 6);
        answer_design_seed(&mut desktop);
        assert!(desktop.design_seed().sends.is_empty());

        // A gap while the design seed is in flight defers the resync; the
        // design answer does not re-seed ahead of the snapshot.
        let mut desktop = design_probed(4);
        let sends = desktop.resync();
        land_resync(&mut desktop, &sends, 4);
        let seeded = desktop.design_seed();
        assert_eq!(seeded.sends.len(), 1);
        assert!(desktop.resync().is_empty());
        let id = seeded.sends[0].request.request_id().to_string();
        let _ = desktop.design_apply(&desktop_widget(design_response_lines(
            &id,
            canvas_json(false, None),
        )));
        assert!(desktop.design_seed().sends.is_empty());
        let deferred = desktop.flush_resync();
        land_resync(&mut desktop, &deferred, 4);
        answer_design_seed(&mut desktop);
        assert!(desktop.design_seed().sends.is_empty());
    }

    #[test]
    fn desktop_seeds_design_only_after_the_probe() {
        let mut desktop = Desktop::new();
        assert!(desktop.design_seed().sends.is_empty());
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
                "methods": ["capabilities", "get_snapshot", "get_design_canvas"],
                "events": ["design_canvas"],
            },
        }))));
        // Capabilities known, snapshot pending: neither seeded nor ready.
        assert!(desktop.design_seed().sends.is_empty());
        assert!(!desktop.design_events_ready());

        let snapshot_id = applied.sends[0].request.request_id().to_string();
        let _ = desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, 1)));
        assert!(desktop.design_events_ready());
        let seeded = desktop.design_seed();
        assert_eq!(seeded.sends.len(), 1);
        assert_eq!(
            seeded.sends[0].request.method(),
            DesktopMethod::GetDesignCanvas
        );

        // A backend reset forgets that design events were the source.
        desktop.reset();
        assert!(!desktop.design_events_ready());

        // A peer that does not advertise the event keeps the legacy path.
        let legacy = probed(1);
        assert!(!legacy.design_events_ready());
    }

    // ── Config ──────────────────────────────────────────────────────────────

    /// A Ready channel at `sequence` whose peer offers `methods` and `events`
    /// besides the probe's own.
    fn config_probed(sequence: u64, methods: &[&str], events: &[&str]) -> Desktop {
        let mut desktop = Desktop::new();
        let probe = desktop.ready(true);
        let _ = desktop.outer(probe.sends[0].generation, true);
        let mut all = vec!["capabilities", "get_snapshot"];
        all.extend_from_slice(methods);
        let applied = desktop.ui(&desktop_widget(record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": probe.sends[0].request.request_id(),
            "ok": true,
            "data": {
                "schema": SCHEMA,
                "command": PROMPT_COMMAND,
                "widget": WIDGET_KEY,
                "methods": all,
                "events": events,
            },
        }))));
        let snapshot_id = applied.sends[0].request.request_id().to_string();
        let _ = desktop.ui(&desktop_widget(snapshot_lines(&snapshot_id, sequence)));
        desktop
    }

    fn config_ready() -> Desktop {
        config_probed(1, &["get_config", "set_config"], &["config_changed"])
    }

    fn params(send: &desktop::Send) -> Value {
        serde_json::from_str::<Value>(send.request.payload()).unwrap()["params"].clone()
    }

    fn profiles_state_json(revision: &str, write_revision: &str) -> Value {
        json!({
            "store": "profiles",
            "scope": "global",
            "readOnly": false,
            "status": "ok",
            "revision": revision,
            "value": {"kind": "gentle-pi.agent_model_profiles", "version": 1, "profiles": {}},
            "layers": [{"source": "global", "status": "ok"}],
            "writeTargets": [{"target": "global", "status": "ok", "writeRevision": write_revision}],
        })
    }

    fn config_response_lines(request_id: &str, data: Value) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": true,
            "data": data,
        }))
    }

    fn config_error_lines(request_id: &str, code: &str) -> Vec<String> {
        record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": request_id,
            "ok": false,
            "error": {"code": code, "message": format!("{code} from the server")},
        }))
    }

    fn config_changed_lines(seq: u64, event: &str, store: &str, revision: &str) -> Vec<String> {
        let (scope, target) = match store {
            "pins" => ("local+repo", "local"),
            _ => ("global", "global"),
        };
        record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": seq,
            "event": event,
            "data": {
                "store": store,
                "scope": scope,
                "writeTarget": target,
                "revision": revision,
                "writeRevision": "sha256:w",
                "source": "desktop",
            },
        }))
    }

    fn profiles_write() -> ConfigWrite {
        ConfigWrite::Profiles(
            ProfilesData::new(json!({
                "kind": "gentle-pi.agent_model_profiles",
                "version": 1,
                "profiles": {"deep": {}},
            }))
            .unwrap(),
        )
    }

    #[test]
    fn config_support_follows_the_probe() {
        // Nothing probed yet: wait, never guess.
        let mut desktop = Desktop::new();
        assert_eq!(desktop.config_support(), ConfigSupport::NotReady);
        assert_eq!(
            desktop.config_get(ConfigStore::Profiles),
            Err(ConfigFailure::NotReady)
        );

        // No extension at all: the direct-file path.
        let _ = desktop.ready(false);
        assert_eq!(desktop.config_support(), ConfigSupport::Legacy);

        // Capabilities without the config methods confirm their absence.
        let mut old = probed(1);
        assert_eq!(old.config_support(), ConfigSupport::Legacy);
        assert_eq!(
            old.config_get(ConfigStore::Profiles),
            Err(ConfigFailure::Unsupported)
        );
        assert_eq!(
            old.config_set(profiles_write(), "sha256:a".into()),
            Err(ConfigFailure::Unsupported)
        );

        // Reading without writing is not enough for the screen to edit.
        let read_only = config_probed(1, &["get_config"], &[]);
        assert_eq!(read_only.config_support(), ConfigSupport::Legacy);

        assert_eq!(config_ready().config_support(), ConfigSupport::Rpc);
    }

    #[test]
    fn config_sends_one_request_at_a_time_in_order() {
        let mut desktop = config_ready();
        let first = desktop.config_get(ConfigStore::Profiles).unwrap();
        let second = desktop.config_get(ConfigStore::Pins).unwrap();
        assert_ne!(first, second);

        let pumped = desktop.config_pump();
        assert_eq!(pumped.sends.len(), 1);
        assert_eq!(pumped.sends[0].request.method(), DesktopMethod::GetConfig);
        assert_eq!(params(&pumped.sends[0]), json!({"store": "profiles"}));
        // One in flight: nothing more goes out until it is answered.
        assert!(desktop.config_pump().sends.is_empty());

        let id = pumped.sends[0].request.request_id().to_string();
        let applied = desktop.config_apply(&desktop_widget(config_response_lines(
            &id,
            profiles_state_json("sha256:r1", "sha256:w1"),
        )));
        assert!(applied.report.is_none());
        let outcomes = desktop.take_config_outcomes();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, first);
        assert!(matches!(
            &outcomes[0].1,
            ConfigOutcome::Read(state) if state.store == ConfigStore::Profiles && state.revision == "sha256:r1"
        ));
        assert!(desktop.take_config_outcomes().is_empty(), "drained once");

        let pumped = desktop.config_pump();
        assert_eq!(params(&pumped.sends[0]), json!({"store": "pins"}));
        // The other slices see the shared slot taken.
        assert!(desktop.session().pending().is_some());
    }

    #[test]
    fn config_set_carries_the_expected_revision_and_maps_failures() {
        let mut desktop = config_ready();
        let ticket = desktop
            .config_set(profiles_write(), "sha256:w1".into())
            .unwrap();
        let pumped = desktop.config_pump();
        assert_eq!(pumped.sends[0].request.method(), DesktopMethod::SetConfig);
        let sent = params(&pumped.sends[0]);
        assert_eq!(sent["store"], "profiles");
        assert_eq!(sent["expectedRevision"], "sha256:w1");
        assert_eq!(sent["data"]["profiles"], json!({"deep": {}}));

        let mut written = profiles_state_json("sha256:r2", "sha256:w2");
        written["writeTarget"] = json!("global");
        written["writeRevision"] = json!("sha256:w2");
        written["changed"] = json!(true);
        let id = pumped.sends[0].request.request_id().to_string();
        let _ = desktop.config_apply(&desktop_widget(config_response_lines(&id, written)));
        let outcomes = desktop.take_config_outcomes();
        assert!(matches!(
            &outcomes[..],
            [(t, ConfigOutcome::Written(result))] if *t == ticket && result.write_revision == "sha256:w2" && result.changed
        ));

        // Every documented config failure reaches the screen as itself.
        for (code, expected) in [
            ("revision_conflict", ConfigErrorCode::RevisionConflict),
            ("invalid_data", ConfigErrorCode::InvalidData),
            ("write_failed", ConfigErrorCode::WriteFailed),
            ("config_unavailable", ConfigErrorCode::ConfigUnavailable),
            ("read_only", ConfigErrorCode::ReadOnly),
        ] {
            let ticket = desktop
                .config_set(profiles_write(), "sha256:w2".into())
                .unwrap();
            let pumped = desktop.config_pump();
            let id = pumped.sends[0].request.request_id().to_string();
            let applied = desktop.config_apply(&desktop_widget(config_error_lines(&id, code)));
            assert!(applied.report.is_none(), "{code}: the screen reports it");
            let outcomes = desktop.take_config_outcomes();
            assert!(
                matches!(
                    &outcomes[..],
                    [(t, ConfigOutcome::Failed(ConfigFailure::Rejected { code: c, .. }))]
                        if *t == ticket && *c == expected
                ),
                "{code}"
            );
        }

        // `unsupported_method` is the legacy signal, not a rejection.
        desktop
            .config_set(profiles_write(), "sha256:w2".into())
            .unwrap();
        let pumped = desktop.config_pump();
        let id = pumped.sends[0].request.request_id().to_string();
        let applied = desktop.config_apply(&desktop_widget(config_error_lines(
            &id,
            "unsupported_method",
        )));
        assert!(applied.report.as_ref().is_none_or(ConfigReport::is_legacy));
        let outcomes = desktop.take_config_outcomes();
        assert!(matches!(
            &outcomes[..],
            [(_, ConfigOutcome::Failed(ConfigFailure::UnsupportedMethod))]
        ));
    }

    #[test]
    fn config_changed_needs_the_exact_advertised_event() {
        let mut desktop = config_ready();
        let applied = desktop.config_apply(&desktop_widget(config_changed_lines(
            2,
            "config_changed",
            "profiles",
            "sha256:r9",
        )));
        assert!(applied.sends.is_empty() && applied.report.is_none());
        assert_eq!(
            desktop.take_config_changes(),
            vec![ConfigChange {
                store: ConfigStore::Profiles,
                revision: Some("sha256:r9".into()),
            }]
        );
        assert!(desktop.take_config_changes().is_empty(), "drained once");

        // A peer that never advertised the event changes nothing.
        let mut quiet = config_probed(1, &["get_config", "set_config"], &[]);
        let _ = quiet.config_apply(&desktop_widget(config_changed_lines(
            2,
            "config_changed",
            "profiles",
            "sha256:r9",
        )));
        assert!(quiet.take_config_changes().is_empty());
    }

    #[test]
    fn config_timeout_disposition_and_reset_drop_their_tickets() {
        let mut desktop = config_ready();
        let ticket = desktop.config_get(ConfigStore::Models).unwrap();
        let pumped = desktop.config_pump();
        let generation = pumped.sends[0].generation;
        // A stale timer is not this request's.
        assert!(
            desktop
                .config_expire(generation.wrapping_sub(1))
                .report
                .is_none()
        );
        assert!(desktop.take_config_outcomes().is_empty());
        let expired = desktop.config_expire(generation);
        assert_eq!(expired.report, Some(ConfigReport::Timeout));
        assert_eq!(
            desktop.take_config_outcomes(),
            vec![(ticket, ConfigOutcome::Failed(ConfigFailure::Timeout))]
        );
        assert!(desktop.session().pending().is_none());

        // An outer prompt Pi did not handle drops the request too.
        let ticket = desktop.config_get(ConfigStore::Models).unwrap();
        let pumped = desktop.config_pump();
        let generation = pumped.sends[0].generation;
        assert!(desktop.config_outer(generation, true).report.is_none());
        let dropped = desktop.config_outer(generation, false);
        assert!(matches!(dropped.report, Some(ConfigReport::Failure(_))));
        assert!(matches!(
            &desktop.take_config_outcomes()[..],
            [(t, ConfigOutcome::Failed(ConfigFailure::Failure(_)))] if *t == ticket
        ));

        // A backend reset drops what was in flight and what was queued; a late
        // answer to the old request is benign.
        let in_flight = desktop.config_get(ConfigStore::Profiles).unwrap();
        let queued = desktop.config_get(ConfigStore::Pins).unwrap();
        let pumped = desktop.config_pump();
        let old_id = pumped.sends[0].request.request_id().to_string();
        desktop.reset();
        let outcomes = desktop.take_config_outcomes();
        assert_eq!(
            outcomes,
            vec![
                (in_flight, ConfigOutcome::Failed(ConfigFailure::Dropped)),
                (queued, ConfigOutcome::Failed(ConfigFailure::Dropped)),
            ]
        );
        assert_eq!(desktop.config_support(), ConfigSupport::NotReady);
        let late = desktop.config_apply(&desktop_widget(config_response_lines(
            &old_id,
            profiles_state_json("sha256:old", "sha256:old"),
        )));
        assert!(late.sends.is_empty());
        assert!(desktop.take_config_outcomes().is_empty());
    }

    #[test]
    fn config_gap_resyncs_once_and_marks_every_store_changed() {
        let mut desktop = config_ready();
        let gap = desktop.config_apply(&desktop_widget(config_changed_lines(
            5,
            "config_changed",
            "profiles",
            "sha256:r5",
        )));
        assert_eq!(gap.report, Some(ConfigReport::OutOfOrderSequence));
        // The wiring hands the gap to the shared resync.
        let sends = desktop.resync();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetSnapshot);
        let changes = desktop.take_config_changes();
        assert_eq!(changes.len(), ConfigStore::ALL.len());
        assert!(changes.iter().all(|change| change.revision.is_none()));

        // Config requests wait behind the snapshot.
        desktop.config_get(ConfigStore::Profiles).unwrap();
        assert!(desktop.config_pump().sends.is_empty());
        land_resync(&mut desktop, &sends, 5);
        assert_eq!(desktop.config_pump().sends.len(), 1);

        // A gap while a config request is in flight defers the snapshot until
        // that request settles.
        assert!(desktop.resync().is_empty());
        assert!(desktop.flush_resync().is_empty(), "still in flight");
    }

    #[test]
    fn config_requests_fail_instead_of_hanging_when_the_channel_stops() {
        let mut desktop = config_ready();
        let queued = desktop.config_get(ConfigStore::Profiles).unwrap();
        // A resync snapshot that times out leaves the channel idle: nothing
        // will pump the queue, so its tickets are answered now.
        let sends = desktop.resync();
        let _ = desktop.expire(sends[0].generation);
        assert_eq!(
            desktop.take_config_outcomes(),
            vec![(queued, ConfigOutcome::Failed(ConfigFailure::Dropped))]
        );
    }
}
