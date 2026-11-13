//! Profiles: the native `/gentle:profiles` models screen. Profiles are named
//! routing snapshots in `<config home>/profiles.json`; the table edits one
//! profile's orchestrator and agent routes, and the footer decides where it
//! applies. Semantics follow gentle-pi `lib/agent-profiles.ts`,
//! `lib/model-routing-authority.ts`, `lib/agent-profile-pin.ts`,
//! `lib/profiles-orchestrator.ts` and the `RoutingTablePanel` handler in
//! `extensions/gentle-ai.ts`.
//!
//! The pure model (routing entries, the store format, pins, agent
//! frontmatter) is `gentle_core::profiles`. Here, `env` resolves the homes
//! and discovers agents, `files` reads and writes the store, routing and Pi
//! settings, `pins` the repository pin files, and `apply` materializes routes
//! and runs Make default. `ProfilesView` keeps its state and queries here; its
//! draft edits (`draft`), store actions (`actions`) and render (`view`) live
//! in child modules.
//!
//! When the tab's Gentle Shell serves its config over Desktop RPC
//! (`get_config`/`set_config`), the `profiles`, `profile_fallbacks` and
//! `pins` stores are read and written through it by name ([`Backend::Rpc`]),
//! and `models`, `subagents` and `managed_assets` are shown read-only. The
//! files are written directly only when the Shell confirms it lacks those
//! methods. When the Shell also advertises `apply_profile`, Make default and
//! Use in this session are its own `/gentle:profiles` actions: the screen
//! saves the draft, sends the request, and shows the Shell's confirmation
//! and notes. An older Shell keeps the screen's own Make default (routing
//! materialization and Pi's settings on the files) and its `set_model`
//! switch.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gentle_core::profiles::{
    ORCHESTRATOR, PinStatus, ProfilesFile, RouteEntry, Routing, assignment_names, evaluate_pin,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;
use pi_rpc::desktop_rpc::{ConfigStore, ConfigWrite, SetConfigResult};

use crate::features::session::SessionView;

mod actions;
mod apply;
mod draft;
mod env;
mod files;
mod pins;
#[cfg(test)]
mod tests;
mod view;

pub(crate) use env::{
    ConfigChange, ConfigFailure, ConfigOutcome, ConfigSupport, ConfigTicket, ProfileApply,
    gentle_pi_root,
};
pub use gentle_core::profiles::ModelChoice;

use apply::DefaultPlan;
use env::{ConfigPort, Env, Landed, Remote, SCREEN_STORES, discover_agents};
use files::{Loaded, RpcLoad, load_screen, profiles_write, read_orchestrator_settings, rpc_load};

/// How long the screen waits for the tab's Shell to report whether it serves
/// the config stores before saying so instead of spinning.
const SUPPORT_DEADLINE: Duration = Duration::from_secs(15);

/// What the screen does once one of its config writes lands.
type Then = Box<dyn FnOnce(&mut ProfilesView, &SetConfigResult, &mut Context<ProfilesView>)>;

/// Where the screen's stores are read and written.
enum Backend {
    /// The tab's Shell has not said yet whether it serves them.
    Undecided,
    /// Directly in the files (`env`, `files`, `pins`).
    Legacy,
    /// Through the tab's Desktop RPC session.
    Rpc(Box<Remote<Then>>),
}

/// The tab's session as the screen's [`ConfigPort`].
struct SessionPort<'a> {
    session: &'a Entity<SessionView>,
    cx: &'a mut App,
}

impl ConfigPort for SessionPort<'_> {
    fn get(&mut self, store: ConfigStore) -> Result<ConfigTicket, ConfigFailure> {
        self.session
            .update(self.cx, |session, cx| session.config_get(store, cx))
    }

    fn set(
        &mut self,
        write: ConfigWrite,
        expected_revision: String,
    ) -> Result<ConfigTicket, ConfigFailure> {
        self.session.update(self.cx, |session, cx| {
            session.config_set(write, expected_revision, cx)
        })
    }

    fn apply(&mut self, request: ProfileApply) -> Result<ConfigTicket, ConfigFailure> {
        self.session.update(self.cx, |session, cx| {
            session.config_apply_profile(request, cx)
        })
    }
}

pub enum ProfilesEvent {
    Close,
    /// Apply the profile to the running session: send RPC `set_model` with
    /// `model` (provider, id) and then `set_thinking_level` with `thinking`.
    /// `None` means the profile leaves the orchestrator untouched. The draft
    /// was saved to `profiles.json` first when it had edits. Only for a Shell
    /// without `apply_profile`, which otherwise switches the session itself.
    UseInSession {
        profile: String,
        model: Option<(String, String)>,
        thinking: Option<String>,
    },
    /// Make default finished outside a pinned repository: the TUI also moves
    /// the running session to the new default orchestrator. Send RPC
    /// `set_model` (provider, id) and, when `thinking` is set,
    /// `set_thinking_level`. Files were already written (`Changed` follows).
    /// Only for a Shell without `apply_profile`.
    SwitchOrchestrator {
        model: (String, String),
        thinking: Option<String>,
    },
    /// Send this slash command as a prompt (`/gentle:profiles clear`,
    /// `/gentle:profiles publish <name>`).
    RunCommand(String),
    /// `profiles.json`, routing, pins or Pi settings were written.
    Changed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Info,
    Good,
    Bad,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NameMode {
    New,
    Duplicate,
    Rename,
}

#[derive(Clone, PartialEq, Eq)]
enum Leave {
    Switch(String),
    New,
    Duplicate,
    Close,
}

enum Confirm {
    Unsaved(Leave),
    Delete(String),
    MakeDefault(DefaultPlan),
    /// Gentle Shell's own dialog for an `apply_profile`, shown exactly:
    /// Apply resends `retry` (the same request with the token), Cancel sends
    /// nothing.
    ApplyProfile {
        title: String,
        message: String,
        retry: ProfileApply,
    },
}

/// Rows: the orchestrator, "every agent", then each assignment name.
const ALL_ROW: &str = "__all";

pub struct ProfilesView {
    env: Option<Env>,
    cwd: PathBuf,
    /// The tab whose Gentle Shell owns the config, when there is one.
    session: Option<Entity<SessionView>>,
    backend: Backend,
    /// The agent names and Pi's default were read (they are not config
    /// stores, so they come from the files on either path).
    local_loaded: bool,
    /// A seed for a missing `profiles` store is being written.
    seeding: bool,
    /// Background file jobs still running.
    io_jobs: usize,
    models: Rc<Vec<ModelChoice>>,
    loading: bool,
    error: Option<String>,
    file: ProfilesFile,
    names: Vec<String>,
    pin: Option<PinStatus>,
    notices: Vec<String>,
    /// Pi's current `settings.json` default orchestrator.
    pi_default: Option<RouteEntry>,
    selected: Option<String>,
    stored: Routing,
    draft: Routing,
    status: Option<(String, Tone)>,
    busy: bool,
    name_mode: Option<NameMode>,
    name_input: Entity<InputState>,
    custom_target: Option<String>,
    custom_input: Entity<InputState>,
    confirm: Option<Confirm>,
    _subscriptions: Vec<Subscription>,
    _deadline: Task<()>,
}

impl EventEmitter<ProfilesEvent> for ProfilesView {}

impl ProfilesView {
    pub fn new(
        cwd: PathBuf,
        models: Vec<ModelChoice>,
        session: Option<Entity<SessionView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("profile-name"));
        let custom_input = cx.new(|cx| InputState::new(window, cx).placeholder("provider/model"));
        let mut subscriptions = vec![
            cx.subscribe_in(
                &name_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit_name(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &custom_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit_custom(window, cx);
                    }
                },
            ),
        ];
        if let Some(session) = &session {
            subscriptions.push(cx.observe(session, |this, _, cx| this.session_changed(cx)));
        }
        let mut this = Self {
            env: None,
            cwd,
            session,
            backend: Backend::Undecided,
            local_loaded: false,
            seeding: false,
            io_jobs: 0,
            models: Rc::new(models),
            loading: true,
            error: None,
            file: ProfilesFile::default(),
            names: Vec::new(),
            pin: None,
            notices: Vec::new(),
            pi_default: None,
            selected: None,
            stored: Routing::default(),
            draft: Routing::default(),
            status: None,
            busy: false,
            name_mode: None,
            name_input,
            custom_target: None,
            custom_input,
            confirm: None,
            _subscriptions: subscriptions,
            _deadline: Task::ready(()),
        };
        this.reload(cx);
        this
    }

    /// Run blocking file work off the UI thread, then apply its result. The
    /// task is detached so a write already under way finishes even if the
    /// screen closes (the result is then dropped).
    fn run_io<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        self.io_jobs += 1;
        self.sync_busy();
        let job = cx.background_executor().spawn(async move { work() });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.io_jobs -= 1;
                this.sync_busy();
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Busy while a file job runs or a config request is open.
    fn sync_busy(&mut self) {
        let remote = match &self.backend {
            Backend::Rpc(remote) => remote.writing() || remote.loading() || remote.applying(),
            _ => false,
        };
        self.busy = self.io_jobs > 0 || remote;
    }

    /// Re-read the stores, agents and pins; unsaved edits to the shown
    /// profile survive when that profile still exists. Which path reads them
    /// is decided here, once, from what the tab's Shell reports.
    fn reload(&mut self, cx: &mut Context<Self>) {
        match self.backend {
            Backend::Legacy => return self.reload_files(cx),
            Backend::Rpc(_) => return self.reload_rpc(cx),
            Backend::Undecided => {}
        }
        let support = match &self.session {
            Some(session) => session.read(cx).config_support(),
            None => ConfigSupport::Legacy,
        };
        match support {
            ConfigSupport::Legacy => {
                self.backend = Backend::Legacy;
                self.reload_files(cx);
            }
            ConfigSupport::Rpc => {
                self.backend = Backend::Rpc(Box::new(Remote::new()));
                self.reload_rpc(cx);
            }
            ConfigSupport::NotReady => {
                self.loading = true;
                self.error = None;
                self._deadline = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SUPPORT_DEADLINE).await;
                    let _ = this.update(cx, |this, cx| {
                        if matches!(this.backend, Backend::Undecided) {
                            this.loading = false;
                            this.error = Some(
                                "Gentle Shell has not reported yet whether it serves its configuration to Desktop. Retry once the tab has finished starting."
                                    .into(),
                            );
                            cx.notify();
                        }
                    });
                });
            }
            ConfigSupport::Unavailable => {
                self.loading = false;
                self.error = Some(
                    "Gentle Desktop's channel failed in this tab, so Profiles cannot tell whether Gentle Shell owns the configuration. Restart the tab's Gentle Shell, then open Profiles again."
                        .into(),
                );
            }
        }
        cx.notify();
    }

    /// The tab's session changed: decide the path once its Shell has said,
    /// and on the RPC path take the answers and store changes meant for the
    /// screen.
    fn session_changed(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        match self.backend {
            Backend::Legacy => {}
            Backend::Undecided => {
                if session.read(cx).config_support() != ConfigSupport::NotReady {
                    self.reload(cx);
                }
            }
            Backend::Rpc(_) => {
                let (outcomes, changes) = session.update(cx, |session, _| {
                    (
                        session.take_config_outcomes(),
                        session.take_config_changes(),
                    )
                });
                for (ticket, outcome) in outcomes {
                    self.land(ticket, outcome, cx);
                }
                if !changes.is_empty() {
                    // A `config_changed` (or a gap) reloads what moved.
                    let read = self.with_remote(cx, |remote, port| remote.changed(port, &changes));
                    if let Some(Err(failure)) = read {
                        self.say(failure.message(), Tone::Bad);
                    }
                }
                self.sync_busy();
                cx.notify();
            }
        }
    }

    /// Run `f` with the RPC stores and the tab's session as their port; `None`
    /// off the RPC path.
    fn with_remote<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Remote<Then>, &mut dyn ConfigPort) -> R,
    ) -> Option<R> {
        let session = self.session.clone()?;
        let Backend::Rpc(remote) = &mut self.backend else {
            return None;
        };
        let mut port = SessionPort {
            session: &session,
            cx,
        };
        Some(f(remote, &mut port))
    }

    fn remote(&self) -> Option<&Remote<Then>> {
        match &self.backend {
            Backend::Rpc(remote) => Some(remote),
            _ => None,
        }
    }

    fn is_rpc(&self) -> bool {
        matches!(self.backend, Backend::Rpc(_))
    }

    /// Whether Make default and Use in this session are Gentle Shell's own
    /// actions (`apply_profile`). Only on the RPC path: a Shell that does not
    /// serve its config cannot apply it either.
    fn shell_applies(&self, cx: &App) -> bool {
        self.is_rpc()
            && self
                .session
                .as_ref()
                .is_some_and(|session| session.read(cx).profile_apply_supported())
    }

    /// Read every store over RPC, and the agent names and Pi's default from
    /// the files (neither is a config store).
    fn reload_rpc(&mut self, cx: &mut Context<Self>) {
        self.reread(&SCREEN_STORES, cx);
        let env = self.env.clone();
        let cwd = self.cwd.clone();
        self.run_io(
            cx,
            move || {
                let env = env.unwrap_or_else(|| Env::resolve(cwd));
                let names = assignment_names(&discover_agents(&env));
                let pi_default = read_orchestrator_settings(&env.settings_path());
                (env, names, pi_default)
            },
            |this, (env, names, pi_default), cx| {
                this.env = Some(env);
                this.names = names;
                this.pi_default = pi_default;
                this.local_loaded = true;
                this.adopt_rpc(true, cx);
            },
        );
    }

    /// Ask for `stores` again over RPC.
    fn reread(&mut self, stores: &[ConfigStore], cx: &mut Context<Self>) {
        let read = self.with_remote(cx, |remote, port| remote.read(port, stores));
        match read {
            Some(Err(failure)) if failure.is_legacy() => self.fall_back_to_files(&failure, cx),
            Some(Err(failure)) => {
                if self.loading {
                    self.loading = false;
                    self.error = Some(format!("Cannot open agent profiles: {}", failure.message()));
                } else {
                    self.say(failure.message(), Tone::Bad);
                }
            }
            _ => {}
        }
        self.sync_busy();
        cx.notify();
    }

    /// Send one store write over RPC; `then` runs once it lands. Nothing is
    /// written to a file here, whatever the answer. Whether it went out.
    fn rpc_write(
        &mut self,
        write: ConfigWrite,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut ProfilesView, &SetConfigResult, &mut Context<ProfilesView>) + 'static,
    ) -> bool {
        let then: Then = Box::new(then);
        let sent = self.with_remote(cx, |remote, port| remote.write(port, write, then));
        match &sent {
            Some(Err(failure)) if failure.is_legacy() => self.fall_back_to_files(failure, cx),
            Some(Err(failure)) => self.say(format!("Not saved: {}", failure.message()), Tone::Bad),
            _ => {}
        }
        self.sync_busy();
        cx.notify();
        matches!(sent, Some(Ok(_)))
    }

    /// One config ticket was answered.
    fn land(&mut self, ticket: ConfigTicket, outcome: ConfigOutcome, cx: &mut Context<Self>) {
        let Some(landed) = self.with_remote(cx, |remote, port| remote.land(port, ticket, outcome))
        else {
            return;
        };
        match landed {
            Landed::Ignored => {}
            Landed::Read(store) | Landed::Refreshed(store) => {
                self.adopt_rpc(store == ConfigStore::Profiles, cx);
            }
            Landed::ReadFailed(store, failure) => {
                // While loading, the load says it; afterwards the last good
                // state stays shown and the failure is said once.
                if !self.loading {
                    self.say(
                        format!(
                            "The {store} store could not be re-read: {}",
                            failure.message()
                        ),
                        Tone::Bad,
                    );
                }
                self.adopt_rpc(store == ConfigStore::Profiles, cx);
            }
            Landed::Written(then, result) => {
                then(self, &result, cx);
                cx.emit(ProfilesEvent::Changed);
                self.adopt_rpc(false, cx);
            }
            Landed::Conflict(_, store) => {
                self.seeding = false;
                self.say(
                    format!(
                        "Not saved: the {store} store changed in Gentle Shell since Profiles read it. Nothing was overwritten; the latest is being read now, and your unsaved edits stay so you can review them and save again."
                    ),
                    Tone::Bad,
                );
            }
            Landed::Busy(_, store) => {
                if std::mem::take(&mut self.seeding) {
                    self.loading = false;
                    self.error = Some(
                        "Could not create the profiles store: Gentle Shell is applying a profile right now. Open Profiles again once it finishes."
                            .into(),
                    );
                } else {
                    self.say(
                        format!(
                            "Not saved: Gentle Shell is applying a profile right now, so the {store} store was not written. Your unsaved edits stay; save again once the apply finishes."
                        ),
                        Tone::Bad,
                    );
                }
            }
            Landed::Rejected(_, failure) => {
                if std::mem::take(&mut self.seeding) {
                    self.loading = false;
                    self.error = Some(format!(
                        "Could not create the profiles store: {}",
                        failure.message()
                    ));
                } else {
                    self.say(format!("Not saved: {}", failure.message()), Tone::Bad);
                }
            }
            Landed::Legacy(failure) => self.fall_back_to_files(&failure, cx),
            Landed::Applied(request, result) => self.apply_answered(request, result, cx),
            // A timeout or a restart abandons the wait, not Shell's apply: it may
            // still finish, so the outcome is unknown rather than "not applied".
            Landed::ApplyFailed(request, failure) => {
                let verdict = if matches!(failure, ConfigFailure::Timeout | ConfigFailure::Dropped)
                {
                    "is not confirmed"
                } else {
                    "was not applied"
                };
                self.say(
                    format!("\"{}\" {verdict}: {}", request.profile, failure.message()),
                    Tone::Bad,
                );
            }
        }
        self.sync_busy();
    }

    /// The Shell confirmed it lacks the config methods: forget the open
    /// tickets and edit the files directly from here on.
    fn fall_back_to_files(&mut self, failure: &ConfigFailure, cx: &mut Context<Self>) {
        if let Backend::Rpc(remote) = &mut self.backend {
            remote.forget();
        }
        self.backend = Backend::Legacy;
        self.seeding = false;
        self.say(
            format!(
                "{} Profiles edits the files directly from here on; a change Gentle Shell had not confirmed was not saved, so check it and save again.",
                failure.message()
            ),
            Tone::Info,
        );
        self.reload_files(cx);
    }

    /// Show the stores last read over RPC once every read has landed.
    /// `full` re-selects like a reload (the `profiles` store moved); otherwise
    /// only the pin and the notes follow.
    fn adopt_rpc(&mut self, full: bool, cx: &mut Context<Self>) {
        let Some(remote) = self.remote() else { return };
        if remote.loading() || !self.local_loaded {
            return;
        }
        match rpc_load(remote) {
            Err(error) => {
                if full || self.loading {
                    self.loading = false;
                    self.error = Some(error);
                }
            }
            Ok(RpcLoad::Seed(file)) => {
                if self.seeding {
                    return;
                }
                match profiles_write(&file) {
                    Err(error) => {
                        self.loading = false;
                        self.error = Some(format!("Could not create the profiles store: {error}"));
                    }
                    Ok(write) => {
                        self.seeding = true;
                        let sent = self.rpc_write(write, cx, |this, _, _| {
                            this.seeding = false;
                            this.say(
                                "Seeded the \"current\" profile in the profiles store from the routing currently in effect.",
                                Tone::Info,
                            );
                        });
                        if !sent && self.is_rpc() {
                            self.seeding = false;
                            self.loading = false;
                            self.error = Some(
                                "Could not create the profiles store; Gentle Shell did not take the request."
                                    .into(),
                            );
                        }
                    }
                }
            }
            Ok(RpcLoad::Ready { file, pin, notices }) => {
                if full || self.loading {
                    let loaded = Loaded {
                        file,
                        names: self.names.clone(),
                        pin,
                        notices,
                        pi_default: self.pi_default.clone(),
                        status: None,
                    };
                    self.apply_loaded(loaded, cx);
                } else {
                    self.pin = pin;
                    self.notices = notices;
                }
            }
        }
        cx.notify();
    }

    /// Re-read the store, agents and pins from the files.
    fn reload_files(&mut self, cx: &mut Context<Self>) {
        let env = self.env.clone();
        let cwd = self.cwd.clone();
        self.run_io(
            cx,
            move || {
                let env = env.unwrap_or_else(|| Env::resolve(cwd));
                let loaded = load_screen(&env);
                (env, loaded)
            },
            |this, (env, loaded), cx| {
                this.env = Some(env);
                match loaded {
                    Err(error) => {
                        this.loading = false;
                        this.error = Some(error);
                    }
                    Ok(loaded) => this.apply_loaded(loaded, cx),
                }
            },
        );
    }

    /// Show what a load read, from either path. The shown profile stays
    /// selected when it still exists, and its unsaved edits survive against
    /// the fresh store.
    fn apply_loaded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        self.loading = false;
        self.error = None;
        let dirty = self.change_count() > 0;
        let pin_winner = evaluate_pin(loaded.pin.as_ref(), &loaded.file)
            .winner
            .map(|w| w.1);
        self.file = loaded.file;
        self.names = loaded.names;
        self.pin = loaded.pin;
        self.notices = loaded.notices;
        self.pi_default = loaded.pi_default;
        if let Some(status) = loaded.status {
            self.say(status, Tone::Info);
            cx.emit(ProfilesEvent::Changed);
        }
        let keep = self.selected.clone().filter(|s| self.file.has(s));
        let target = keep
            .clone()
            .or(pin_winner)
            .or_else(|| self.file.active.clone().filter(|a| self.file.has(a)))
            .or_else(|| self.file.names().next().map(str::to_string));
        let keep_draft = dirty && keep.is_some() && keep == target;
        self.select(target, keep_draft);
    }

    fn select(&mut self, name: Option<String>, keep_draft: bool) {
        self.stored = name
            .as_deref()
            .and_then(|n| self.file.get(n))
            .cloned()
            .unwrap_or_default();
        if !keep_draft {
            self.draft = self.stored.clone();
        }
        self.selected = name;
        self.custom_target = None;
    }

    fn say(&mut self, message: impl Into<String>, tone: Tone) {
        self.status = Some((message.into(), tone));
    }

    fn pin_winner(&self) -> Option<String> {
        evaluate_pin(self.pin.as_ref(), &self.file)
            .winner
            .map(|w| w.1)
    }

    fn catalog_model(&self, model: &str) -> Option<&ModelChoice> {
        self.models.iter().find(|m| m.route_id() == model)
    }

    fn fast_capable(&self, model: Option<&str>) -> bool {
        model
            .and_then(|m| self.catalog_model(m))
            .is_some_and(ModelChoice::supports_fast)
    }

    /// The levels a model supports: a catalog model without reasoning offers
    /// only `off`; reasoning or unknown models offer every level.
    fn supported_levels(&self, model: Option<&str>) -> Option<Vec<&'static str>> {
        let choice = self.catalog_model(model?)?;
        (!choice.reasoning).then(|| vec!["off"])
    }

    fn row_keys(&self) -> Vec<String> {
        let mut keys = vec![ORCHESTRATOR.to_string()];
        keys.extend(self.names.iter().cloned());
        keys
    }

    fn change_count(&self) -> usize {
        self.row_keys()
            .iter()
            .map(|key| {
                let (d, s) = (self.draft.get(key), self.stored.get(key));
                let field = |f: fn(&RouteEntry) -> &Option<String>| {
                    d.and_then(|e| f(e).as_ref()) != s.and_then(|e| f(e).as_ref())
                };
                usize::from(field(|e| &e.model))
                    + usize::from(field(|e| &e.thinking))
                    + usize::from(field(|e| &e.tier))
            })
            .sum()
    }

    fn target_keys(&self, key: &str) -> Vec<String> {
        if key == ALL_ROW {
            self.names.clone()
        } else {
            vec![key.to_string()]
        }
    }

    fn row_label(key: &str) -> &str {
        match key {
            ORCHESTRATOR => "the orchestrator",
            ALL_ROW => "every agent",
            other => other,
        }
    }
}
