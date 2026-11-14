//! Where the screen reads and writes: Gentle Shell's homes, the installed
//! `gentle-pi` package and the agents discoverable from them, or Gentle
//! Shell's config stores over Desktop RPC ([`Remote`] through a
//! [`ConfigPort`]) when the tab's Shell serves them.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use gentle_core::profiles::{AgentEntry, AgentSource, parse_agent_name};
use pi_rpc::desktop_rpc::{
    ApplyProfileOutcome, ApplyProfileResult, ConfigErrorCode, ConfigState, ConfigStore,
    ConfigWrite, ProfileApplyErrorCode, ProfileApplyMode, ProfileNote, SetConfigResult,
};

/// `CORE_MODEL_AGENT_NAMES`: listed first, the rest alphabetically.
const CORE_AGENTS: [&str; 3] = ["jd-judge-a", "jd-judge-b", "jd-fix-agent"];

/// Every location the screen reads or writes, resolved once off the UI thread.
#[derive(Clone, Debug)]
pub struct Env {
    pub cwd: PathBuf,
    pub agent_home: PathBuf,
    pub config_home: PathBuf,
    pub home: PathBuf,
    /// The installed `gentle-pi` package (for built-in agent directories).
    pub package_root: Option<PathBuf>,
}

impl Env {
    pub(super) fn resolve(cwd: PathBuf) -> Self {
        let homes = pi_rpc::resolve_homes();
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let package_root = pi_rpc::resolve_backend(&cwd)
            .ok()
            .and_then(|spec| gentle_pi_root(&spec.program));
        Self {
            cwd,
            agent_home: homes.agent,
            config_home: homes.config,
            home,
            package_root,
        }
    }

    pub(super) fn profiles_path(&self) -> PathBuf {
        self.config_home.join("profiles.json")
    }

    pub(super) fn models_path(&self) -> PathBuf {
        self.config_home.join("models.json")
    }

    pub(super) fn settings_path(&self) -> PathBuf {
        self.agent_home.join("settings.json")
    }

    pub(super) fn subagents_path(&self, source: AgentSource) -> PathBuf {
        match source {
            AgentSource::Project => self.cwd.join(".pi").join("subagents.json"),
            _ => self.agent_home.join("subagents.json"),
        }
    }
}

/// `<gentle-pi>` from the launcher: `<gentle-pi>/bin/gentle-shell.mjs`, or the
/// Windows shim beside `node_modules/gentle-pi`.
pub(crate) fn gentle_pi_root(program: &Path) -> Option<PathBuf> {
    let launcher = dunce::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
    let mut roots = Vec::new();
    if let Some(root) = launcher.parent().and_then(Path::parent) {
        roots.push(root.to_path_buf());
    }
    if let Some(dir) = program.parent() {
        roots.push(dir.join("node_modules").join("gentle-pi"));
        roots.push(
            dir.join("..")
                .join("lib")
                .join("node_modules")
                .join("gentle-pi"),
        );
    }
    roots
        .into_iter()
        .find(|root| root.join("extensions").join("gentle-ai.ts").is_file())
}

/// `listAgentFilesRecursive`: `.md` but not `.chain.md`, skipping `skills/`.
fn list_agent_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            if name != "skills" {
                list_agent_files(&entry.path(), out);
            }
        } else if kind.is_file() && name.ends_with(".md") && !name.ends_with(".chain.md") {
            out.push(entry.path());
        }
    }
}

fn agents_in(dir: &Path, source: AgentSource) -> Vec<AgentEntry> {
    let mut files = Vec::new();
    list_agent_files(dir, &mut files);
    files
        .into_iter()
        .filter_map(|path| {
            let name = parse_agent_name(&fs::read_to_string(&path).ok()?)?;
            Some(AgentEntry { name, source, path })
        })
        .collect()
}

/// `discoverableNonBuiltinAgentRoots`, deduplicated by canonical path with
/// later-root precedence (and the package-managed root's lexical path kept).
fn non_builtin_roots(env: &Env) -> Vec<(PathBuf, AgentSource)> {
    let roots = [
        (env.agent_home.join("agents"), AgentSource::User, true),
        (env.agent_home.join("subagents"), AgentSource::User, false),
        (env.home.join(".agents"), AgentSource::User, false),
        (env.cwd.join(".agents"), AgentSource::Project, false),
        (
            env.cwd.join(".pi").join("agents"),
            AgentSource::Project,
            false,
        ),
        (
            env.cwd.join(".pi").join("subagents"),
            AgentSource::Project,
            false,
        ),
    ];
    let mut unique: Vec<(PathBuf, (PathBuf, AgentSource, bool))> = Vec::new();
    for (dir, source, managed) in roots {
        let canonical = dunce::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if let Some(index) = unique.iter().position(|(c, _)| *c == canonical) {
            let (_, existing) = unique.remove(index);
            let managed_dir = if existing.2 {
                Some(existing.0)
            } else if managed {
                Some(dir.clone())
            } else {
                None
            };
            let is_managed = managed_dir.is_some();
            unique.push((canonical, (managed_dir.unwrap_or(dir), source, is_managed)));
        } else {
            unique.push((canonical, (dir, source, managed)));
        }
    }
    unique
        .into_iter()
        .map(|(_, (dir, source, _))| (dir, source))
        .collect()
}

fn builtin_dirs(env: &Env) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for package in ["pi-subagents-j0k3r", "pi-subagents"] {
        if let Some(root) = &env.package_root {
            dirs.push(root.join("..").join(package).join("agents"));
        }
        dirs.push(
            env.cwd
                .join(".pi")
                .join("npm")
                .join("node_modules")
                .join(package)
                .join("agents"),
        );
        dirs.push(
            env.home
                .join(".local")
                .join("lib")
                .join("node_modules")
                .join(package)
                .join("agents"),
        );
    }
    dirs
}

/// `listDiscoverableAgents`: built-ins, then user and project roots; a later
/// definition of the same name wins. Core agents first, the rest sorted.
pub fn discover_agents(env: &Env) -> Vec<AgentEntry> {
    let mut all = Vec::new();
    for dir in builtin_dirs(env) {
        all.extend(agents_in(&dir, AgentSource::Builtin));
    }
    for (dir, source) in non_builtin_roots(env) {
        all.extend(agents_in(&dir, source));
    }
    let mut by_name: Vec<AgentEntry> = Vec::new();
    for agent in all {
        match by_name.iter_mut().find(|a| a.name == agent.name) {
            Some(slot) => *slot = agent,
            None => by_name.push(agent),
        }
    }
    let mut ordered: Vec<AgentEntry> = CORE_AGENTS
        .iter()
        .filter_map(|core| by_name.iter().find(|a| a.name == *core).cloned())
        .collect();
    let mut rest: Vec<AgentEntry> = by_name
        .into_iter()
        .filter(|a| !CORE_AGENTS.contains(&a.name.as_str()))
        .collect();
    rest.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    ordered.extend(rest);
    ordered
}

// ---------------------------------------------------------------------------
// Gentle Shell's config stores over Desktop RPC
// ---------------------------------------------------------------------------

/// One config request the tab's session accepted on the screen's behalf.
pub(crate) type ConfigTicket = u64;

/// Where the screen's stores live on this tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSupport {
    /// The Shell has not reported its capabilities yet: wait, never guess.
    NotReady,
    /// The Shell confirms it lacks the config methods (or there is no Shell
    /// to ask): the direct-file path.
    Legacy,
    /// The Desktop channel failed on this backend: neither path is safe to
    /// pick, so the screen says so.
    Unavailable,
    /// `get_config` and `set_config` are served.
    Rpc,
}

/// Why a config request did not produce its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigFailure {
    /// The capabilities do not list the method.
    Unsupported,
    /// The server answered `unsupported_method` for it.
    UnsupportedMethod,
    /// Capabilities are not known yet.
    NotReady,
    /// The server did not answer in time.
    Timeout,
    /// The backend was replaced or the channel stopped before the answer.
    Dropped,
    /// A documented config failure (`revision_conflict`, `invalid_data`…).
    Rejected {
        code: ConfigErrorCode,
        message: String,
    },
    /// A transport, decoding or request failure.
    Failure(String),
    /// Make default is writing the files: no store write is sent until it
    /// finishes.
    Held,
    /// Gentle Shell is applying a profile (`apply_profile`): no store write
    /// is sent until it answers.
    Applying,
    /// A documented `apply_profile` failure (`unknown_profile`,
    /// `profile_apply_busy`, `profiles_unavailable`).
    ApplyRefused {
        code: ProfileApplyErrorCode,
        message: String,
    },
}

impl ConfigFailure {
    /// Only these mean the direct-file path; every other failure is reported
    /// and never falls back to writing files.
    pub(crate) fn is_legacy(&self) -> bool {
        matches!(self, Self::Unsupported | Self::UnsupportedMethod)
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Unsupported | Self::UnsupportedMethod => {
                "This Gentle Shell does not serve its configuration to Desktop.".to_string()
            }
            Self::NotReady => {
                "Gentle Shell is still starting in this tab; try again in a moment.".to_string()
            }
            Self::Timeout => {
                "Gentle Shell did not answer in time; nothing is confirmed.".to_string()
            }
            Self::Dropped => {
                "Gentle Shell restarted before it answered; nothing is confirmed.".to_string()
            }
            Self::Rejected { code, message } => match code {
                ConfigErrorCode::RevisionConflict => {
                    "The configuration changed in Gentle Shell since it was read.".to_string()
                }
                ConfigErrorCode::InvalidData => {
                    format!("Gentle Shell refused the data ({message}).")
                }
                ConfigErrorCode::WriteFailed => {
                    format!("Gentle Shell could not write it ({message}).")
                }
                ConfigErrorCode::ConfigUnavailable => {
                    format!("Gentle Shell's configuration is unavailable ({message}).")
                }
                ConfigErrorCode::ProfileApplyBusy => {
                    "Gentle Shell is applying a profile right now, so nothing was written."
                        .to_string()
                }
                ConfigErrorCode::UnknownStore | ConfigErrorCode::ReadOnly => {
                    format!("Gentle Shell refused the request ({code}: {message}).")
                }
            },
            Self::Failure(reason) => format!("The configuration request failed ({reason})."),
            Self::Held => "Make default is still writing the files; wait for Make default to finish, then save again.".to_string(),
            Self::Applying => "Gentle Shell is still applying a profile; wait for it to finish, then try again.".to_string(),
            Self::ApplyRefused { code, message } => match code {
                ProfileApplyErrorCode::UnknownProfile => {
                    format!("Gentle Shell has no saved profile by that name ({message}).")
                }
                ProfileApplyErrorCode::ProfileApplyBusy => {
                    "Gentle Shell is already applying a profile; nothing was written. Try again once it finishes.".to_string()
                }
                ProfileApplyErrorCode::ProfilesUnavailable => {
                    format!("Gentle Shell's profiles are unavailable in this tab ({message}).")
                }
            },
        }
    }
}

/// What a ticket was answered with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConfigOutcome {
    Read(ConfigState),
    Written(SetConfigResult),
    Applied(ApplyProfileResult),
    Failed(ConfigFailure),
}

/// One `apply_profile` request: run the `/gentle:profiles` screen's Make
/// default (`Default`) or Use in this session (`Session`) on a saved
/// profile. `confirmation` is set only when resending the token of a dialog
/// the human approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileApply {
    pub(crate) profile: String,
    pub(crate) mode: ProfileApplyMode,
    pub(crate) confirmation: Option<String>,
}

impl ProfileApply {
    /// The first request, without a token.
    pub(crate) fn new(profile: impl Into<String>, mode: ProfileApplyMode) -> Self {
        Self {
            profile: profile.into(),
            mode,
            confirmation: None,
        }
    }

    /// The same request, resent with the token of the approved dialog.
    pub(crate) fn confirmed(&self, token: impl Into<String>) -> Self {
        Self {
            confirmation: Some(token.into()),
            ..self.clone()
        }
    }
}

/// What the screen does with Gentle Shell's answer to an apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ApplyAnswer {
    /// Show Gentle Shell's dialog exactly. Only the human's Apply sends
    /// `retry`; Cancel sends nothing. A confirmation is asked again (never
    /// answered here) even for a request that already carried a token: the
    /// text or the profile changed since it was shown.
    Confirm {
        title: String,
        message: String,
        retry: ProfileApply,
    },
    /// Finished. `applied` is whether Gentle Shell applied it; the notes
    /// say how, or why not.
    Done {
        applied: bool,
        notes: Vec<ProfileNote>,
    },
}

impl ApplyAnswer {
    pub(super) fn new(request: &ProfileApply, result: ApplyProfileResult) -> Self {
        match result.outcome {
            ApplyProfileOutcome::ConfirmationRequired(confirmation) => Self::Confirm {
                title: confirmation.title,
                message: confirmation.message,
                retry: request.confirmed(confirmation.token),
            },
            ApplyProfileOutcome::Applied(notes) => Self::Done {
                applied: true,
                notes,
            },
            ApplyProfileOutcome::NotApplied(notes) => Self::Done {
                applied: false,
                notes,
            },
        }
    }
}

/// A store that changed on the server (`config_changed`, with its new read
/// revision), or that may have while events were lost (`revision: None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigChange {
    pub(crate) store: ConfigStore,
    pub(crate) revision: Option<String>,
}

/// Where the screen's config requests go: the tab's Desktop session in the
/// app, a recording double in tests.
pub(crate) trait ConfigPort {
    fn get(&mut self, store: ConfigStore) -> Result<ConfigTicket, ConfigFailure>;
    fn set(
        &mut self,
        write: ConfigWrite,
        expected_revision: String,
    ) -> Result<ConfigTicket, ConfigFailure>;
    fn apply(&mut self, request: ProfileApply) -> Result<ConfigTicket, ConfigFailure>;
}

/// Every store the screen reads: the three it writes and the three it only
/// shows.
pub(super) const SCREEN_STORES: [ConfigStore; 6] = ConfigStore::ALL;

/// What one answer meant for the screen. `K` is what the screen wants done
/// once its write lands; it comes back exactly once, or is dropped with the
/// write when the direct-file path takes over.
pub(super) enum Landed<K> {
    /// Not a ticket this screen holds (late, repeated, or another screen's).
    Ignored,
    /// A store read was adopted.
    Read(ConfigStore),
    /// The re-read after a revision conflict was adopted.
    Refreshed(ConfigStore),
    /// A store read failed; the last good state (if any) is kept.
    ReadFailed(ConfigStore, ConfigFailure),
    /// The write landed; its re-read state was adopted.
    Written(K, SetConfigResult),
    /// The store changed since it was read: it is being re-read, and the
    /// write is not retried.
    Conflict(K, ConfigStore),
    /// Gentle Shell is applying a profile (started anywhere) and wrote
    /// nothing; the write is not retried, and the edit stays to save again
    /// once the apply finishes.
    Busy(K, ConfigStore),
    /// The write was refused or lost; nothing was adopted.
    Rejected(K, ConfigFailure),
    /// The Shell lacks the methods: the screen switches to the direct-file
    /// path.
    Legacy(ConfigFailure),
    /// Gentle Shell answered the apply (a confirmation, applied, or not
    /// applied); the hold on writes is released.
    Applied(ProfileApply, ApplyProfileResult),
    /// The apply was refused or lost. Never a reason to switch to the files:
    /// the screen says why and nothing is retried.
    ApplyFailed(ProfileApply, ConfigFailure),
}

/// The screen's view of Gentle Shell's config stores: the last state read
/// for each, and the tickets still open. Pure on purpose: requests go out
/// through a [`ConfigPort`] and answers come back through [`Self::land`].
pub(super) struct Remote<K> {
    states: HashMap<ConfigStore, ConfigState>,
    failures: HashMap<ConfigStore, ConfigFailure>,
    reads: HashMap<ConfigTicket, ConfigStore>,
    writes: HashMap<ConfigTicket, (ConfigStore, K)>,
    /// Stores being re-read after a revision conflict.
    conflicted: HashSet<ConfigStore>,
    /// Stores asked for again while a read of them was open: one more read
    /// follows that one, so its answer cannot predate the request.
    dirty: HashSet<ConfigStore>,
    /// Make default is writing the files: every write is refused.
    held: bool,
    /// The `apply_profile` request Gentle Shell has not answered yet: every
    /// write is refused until it does.
    applying: Option<(ConfigTicket, ProfileApply)>,
}

impl<K> Remote<K> {
    pub(super) fn new() -> Self {
        Self {
            states: HashMap::new(),
            failures: HashMap::new(),
            reads: HashMap::new(),
            writes: HashMap::new(),
            conflicted: HashSet::new(),
            dirty: HashSet::new(),
            held: false,
            applying: None,
        }
    }

    /// Make default starts writing the files: refuse every store write
    /// until [`Self::release`].
    pub(super) fn hold(&mut self) {
        self.held = true;
    }

    /// Make default finished (or failed): writes may go out again.
    pub(super) fn release(&mut self) {
        self.held = false;
    }

    pub(super) fn held(&self) -> bool {
        self.held
    }

    /// No read or write of `store` is open or owed: its state is the
    /// freshest this screen can know.
    pub(super) fn settled(&self, store: ConfigStore) -> bool {
        !self.reads.values().any(|reading| *reading == store)
            && !self.writes.values().any(|(writing, _)| *writing == store)
            && !self.dirty.contains(&store)
    }

    /// The last state read for `store`.
    pub(super) fn state(&self, store: ConfigStore) -> Option<&ConfigState> {
        self.states.get(&store)
    }

    /// Why the last read of `store` failed, while it has not succeeded since.
    pub(super) fn failure(&self, store: ConfigStore) -> Option<&ConfigFailure> {
        self.failures.get(&store)
    }

    /// Reads are still open.
    pub(super) fn loading(&self) -> bool {
        !self.reads.is_empty()
    }

    /// Writes are still open.
    pub(super) fn writing(&self) -> bool {
        !self.writes.is_empty()
    }

    /// An `apply_profile` request is still open.
    pub(super) fn applying(&self) -> bool {
        self.applying.is_some()
    }

    /// Ask Gentle Shell to apply a saved profile. It starts only when no
    /// store write, Make default or other apply is open, and holds every
    /// store write until Gentle Shell answers, like Make default's files job.
    pub(super) fn apply(
        &mut self,
        port: &mut dyn ConfigPort,
        request: ProfileApply,
    ) -> Result<ConfigTicket, ConfigFailure> {
        if self.held {
            return Err(ConfigFailure::Held);
        }
        if self.applying.is_some() {
            return Err(ConfigFailure::Applying);
        }
        if self.writing() {
            return Err(ConfigFailure::Failure(
                "a profile change is still being saved".to_string(),
            ));
        }
        let ticket = port.apply(request.clone())?;
        self.applying = Some((ticket, request));
        Ok(ticket)
    }

    /// Read `stores`. A store already being read is not asked twice at
    /// once: it is marked dirty, and exactly one more read follows the open
    /// one.
    pub(super) fn read(
        &mut self,
        port: &mut dyn ConfigPort,
        stores: &[ConfigStore],
    ) -> Result<(), ConfigFailure> {
        for &store in stores {
            if self.reads.values().any(|reading| *reading == store) {
                self.dirty.insert(store);
                continue;
            }
            let ticket = port.get(store)?;
            self.reads.insert(ticket, store);
        }
        Ok(())
    }

    /// Send `write` guarded by the `writeRevision` its target had when last
    /// read. A store without that target (a read-only store, or pins outside
    /// a worktree) is refused here, and one write per store is open at a
    /// time.
    pub(super) fn write(
        &mut self,
        port: &mut dyn ConfigPort,
        write: ConfigWrite,
        then: K,
    ) -> Result<ConfigTicket, ConfigFailure> {
        if self.held {
            return Err(ConfigFailure::Held);
        }
        if self.applying.is_some() {
            return Err(ConfigFailure::Applying);
        }
        let store = write.store();
        if self.writes.values().any(|(writing, _)| *writing == store) {
            return Err(ConfigFailure::Failure(format!(
                "another change to {store} is still being saved"
            )));
        }
        let target = write.write_target();
        let Some(expected) = self
            .states
            .get(&store)
            .and_then(|state| state.write_revision(target))
        else {
            return Err(ConfigFailure::Failure(format!(
                "{store} offers no {target} file to write here"
            )));
        };
        let ticket = port.set(write, expected.to_string())?;
        self.writes.insert(ticket, (store, then));
        Ok(ticket)
    }

    /// A ticket was answered.
    pub(super) fn land(
        &mut self,
        port: &mut dyn ConfigPort,
        ticket: ConfigTicket,
        outcome: ConfigOutcome,
    ) -> Landed<K> {
        if let Some((_, request)) = self.applying.take_if(|(open, _)| *open == ticket) {
            return match outcome {
                ConfigOutcome::Applied(result)
                    if result.profile == request.profile && result.mode == request.mode =>
                {
                    Landed::Applied(request, result)
                }
                ConfigOutcome::Failed(failure) => Landed::ApplyFailed(request, failure),
                _ => Landed::ApplyFailed(
                    request,
                    ConfigFailure::Failure("the answer to the apply did not match it".to_string()),
                ),
            };
        }
        if let Some(store) = self.reads.remove(&ticket) {
            let landed = self.land_read(store, outcome);
            // Asked again while this read was open: one follow-up read, and
            // the screen keeps waiting for it.
            if self.dirty.remove(&store)
                && let Err(failure) = self.read(port, &[store])
            {
                self.conflicted.remove(&store);
                self.failures.insert(store, failure.clone());
                return Landed::ReadFailed(store, failure);
            }
            return landed;
        }
        let Some((store, then)) = self.writes.remove(&ticket) else {
            return Landed::Ignored;
        };
        match outcome {
            ConfigOutcome::Written(result) if result.state.store == store => {
                self.states.insert(store, result.state.clone());
                self.failures.remove(&store);
                Landed::Written(then, result)
            }
            ConfigOutcome::Written(_) | ConfigOutcome::Read(_) | ConfigOutcome::Applied(_) => {
                Landed::Rejected(
                    then,
                    ConfigFailure::Failure(format!(
                        "the answer to a {store} write did not match it"
                    )),
                )
            }
            ConfigOutcome::Failed(ConfigFailure::Rejected {
                code: ConfigErrorCode::RevisionConflict,
                ..
            }) => {
                // Never a retry: the fresh state is read, and the screen
                // decides again from it.
                self.conflicted.insert(store);
                if let Err(failure) = self.read(port, &[store]) {
                    self.conflicted.remove(&store);
                    self.failures.insert(store, failure);
                }
                Landed::Conflict(then, store)
            }
            ConfigOutcome::Failed(ConfigFailure::Rejected {
                code: ConfigErrorCode::ProfileApplyBusy,
                ..
            }) => Landed::Busy(then, store),
            ConfigOutcome::Failed(failure) if failure.is_legacy() => Landed::Legacy(failure),
            ConfigOutcome::Failed(failure) => Landed::Rejected(then, failure),
        }
    }

    fn land_read(&mut self, store: ConfigStore, outcome: ConfigOutcome) -> Landed<K> {
        let failure = match outcome {
            ConfigOutcome::Read(state) if state.store == store => {
                self.states.insert(store, state);
                self.failures.remove(&store);
                if self.dirty.contains(&store) {
                    // A follow-up read is owed; that one completes it.
                    return Landed::Read(store);
                }
                return if self.conflicted.remove(&store) {
                    Landed::Refreshed(store)
                } else {
                    Landed::Read(store)
                };
            }
            ConfigOutcome::Read(state) => ConfigFailure::Failure(format!(
                "asked for {store}, Gentle Shell answered {}",
                state.store
            )),
            ConfigOutcome::Written(_) => {
                ConfigFailure::Failure(format!("a {store} read was answered as a write"))
            }
            ConfigOutcome::Applied(_) => {
                ConfigFailure::Failure(format!("a {store} read was answered as an apply"))
            }
            ConfigOutcome::Failed(failure) if failure.is_legacy() => {
                return Landed::Legacy(failure);
            }
            ConfigOutcome::Failed(failure) => failure,
        };
        self.conflicted.remove(&store);
        self.failures.insert(store, failure.clone());
        Landed::ReadFailed(store, failure)
    }

    /// Stores changed on the server: re-read each one whose revision differs
    /// from the one held (a write's own echo does not), or every one named
    /// after a gap.
    pub(super) fn changed(
        &mut self,
        port: &mut dyn ConfigPort,
        changes: &[ConfigChange],
    ) -> Result<(), ConfigFailure> {
        let mut stale: Vec<ConfigStore> = Vec::new();
        for change in changes {
            let held = self
                .states
                .get(&change.store)
                .map(|state| state.revision.as_str());
            let moved = change.revision.is_none() || change.revision.as_deref() != held;
            if moved && !stale.contains(&change.store) {
                stale.push(change.store);
            }
        }
        self.read(port, &stale)
    }

    /// Forget every open ticket (the session went away or the screen
    /// switched to files): their answers are ignored from here on.
    pub(super) fn forget(&mut self) {
        self.reads.clear();
        self.writes.clear();
        self.conflicted.clear();
        self.dirty.clear();
        self.applying = None;
    }
}
