//! Live Desktop↔Shell maintainer probe for `pi-rpc`.
//!
//! This is the real counterpart of the hermetic `desktop_rpc.rs` suite: it
//! spawns the actual Pi RPC backend and drives the exact `PiProcess` /
//! `Incoming::Ui` / `DesktopSession` loop the app uses, against an isolated
//! home so no real configuration, auth, model, provider, agent, job, canvas or
//! audio is ever touched.
//!
//! It is `#[ignore]` by default: `cargo test -p pi-rpc --locked` only compiles
//! it, which keeps ordinary runs hermetic. To run it:
//!
//! ```text
//! GENTLE_PI_RUNTIME_CLI=.../node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js \
//!   cargo test -p pi-rpc --test desktop_rpc_live -- --ignored --nocapture
//! ```
//!
//! `GENTLE_PI_RUNTIME_CLI` selects the candidate Pi CLI bundle; the Gentle
//! extension files (`extensions/gentle-{desktop-rpc,ai,shell,design,agents}.ts`)
//! are resolved as absolute paths from the Gentle package root above that file.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, ensure};
use pi_rpc::desktop_rpc::{
    Capabilities, ConfigStatus, ConfigStore, ConfigWrite, ConfigWriteTarget, DesktopMethod,
    DesktopRecord, DesktopRequest, DesktopResponse, DesktopSession, PROMPT_COMMAND, PermissionMode,
    ProfilesData, SCHEMA, Snapshot, UsageRefresh, WIDGET_KEY,
};
use pi_rpc::{Incoming, LaunchSpec, PiProcess, UiMethod, UiRequest};
use serde_json::{Value, json};

/// How long one Desktop roundtrip may take before the probe gives up. The
/// first request also covers backend boot, so this is deliberately generous.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to watch for an event that must NOT arrive (a `rate_limit` after
/// an `unsupported` refresh). Short: its absence is immediate.
const NO_EVENT_WINDOW: Duration = Duration::from_secs(2);
/// Grace handed to `PiProcess::shutdown` before the process tree is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// The candidate extensions, in the order the desktop app loads them.
const EXTENSIONS: [&str; 5] = [
    "gentle-desktop-rpc",
    "gentle-ai",
    "gentle-shell",
    "gentle-design",
    "gentle-agents",
];

// ---------------------------------------------------------------------------
// Locating the runtime and extensions
// ---------------------------------------------------------------------------

/// The candidate Pi CLI bundle from `GENTLE_PI_RUNTIME_CLI`, or a clear skip.
fn locate_runtime() -> Option<PathBuf> {
    let path = match std::env::var_os("GENTLE_PI_RUNTIME_CLI") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            eprintln!(
                "desktop_rpc_live: GENTLE_PI_RUNTIME_CLI is not set; \
                 set it to .../pi-coding-agent/dist/bundle/cli.js to run this probe \
                 (it is #[ignore] by default)"
            );
            return None;
        }
    };
    let absolute = std::fs::canonicalize(&path).unwrap_or(path);
    if absolute.is_file() {
        Some(absolute)
    } else {
        eprintln!(
            "desktop_rpc_live: GENTLE_PI_RUNTIME_CLI={} is not a file; skipping live probe",
            absolute.display()
        );
        None
    }
}

/// The five extension files, resolved as absolute paths from the Gentle package
/// root that also owns the candidate Pi bundle. A missing set is a clear skip,
/// never a guess.
fn locate_extensions(runtime: &Path) -> Option<Vec<PathBuf>> {
    for ancestor in runtime.ancestors().skip(1) {
        let dir = ancestor.join("extensions");
        let all = EXTENSIONS
            .iter()
            .all(|name| dir.join(format!("{name}.ts")).is_file());
        if all {
            return Some(
                EXTENSIONS
                    .iter()
                    .map(|name| dir.join(format!("{name}.ts")))
                    .collect(),
            );
        }
    }
    eprintln!(
        "desktop_rpc_live: no extensions/gentle-*.ts set found above {}; skipping live probe",
        runtime.display()
    );
    None
}

/// `node` on the inherited PATH, searched directly (no login-shell merge).
fn find_node() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------
// Isolation
// ---------------------------------------------------------------------------

/// A unique temp root with every subdirectory the backend needs. Removed on
/// drop, after `Probe` has shut the backend down (field order guarantees the
/// process is gone first).
struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("desktop-rpc-live-{}-{nanos}", std::process::id()));
        for sub in ["home", "tmp", "config", "agent", "sessions", "workspace"] {
            std::fs::create_dir_all(path.join(sub)).expect("create probe temp subdir");
        }
        Self { path }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// The child environment: every state directory is inside the temp root, the
/// interactive-host marker is set, and there is no real auth or network.
fn isolation_env(root: &TempRoot) -> Vec<(String, OsString)> {
    let home = root.dir("home");
    let tmp = root.dir("tmp");
    vec![
        ("HOME".into(), home.as_os_str().to_os_string()),
        ("USERPROFILE".into(), home.as_os_str().to_os_string()),
        ("TMPDIR".into(), tmp.as_os_str().to_os_string()),
        ("TMP".into(), tmp.as_os_str().to_os_string()),
        ("TEMP".into(), tmp.as_os_str().to_os_string()),
        (
            "GENTLE_PI_CONFIG_HOME".into(),
            root.dir("config").as_os_str().to_os_string(),
        ),
        (
            "GENTLE_PI_AGENT_HOME".into(),
            root.dir("agent").as_os_str().to_os_string(),
        ),
        (
            "PI_CODING_AGENT_DIR".into(),
            root.dir("agent").as_os_str().to_os_string(),
        ),
        ("GENTLE_SHELL_INTERACTIVE_HOST".into(), "1".into()),
        ("PI_OFFLINE".into(), "1".into()),
        ("PI_NO_LOCAL_LLM".into(), "1".into()),
        ("NO_COLOR".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), null_device().into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        // Neutralize any inherited bundled-launcher markers so config/home
        // resolution cannot escape the isolated root.
        ("GENTLE_SHELL_PREFIX".into(), OsString::new()),
        ("GENTLE_SHELL_BIN".into(), OsString::new()),
        ("GENTLE_SHELL_CONFIG_HOME".into(), OsString::new()),
    ]
}

/// The launch spec: explicit `node <cli.js>`, no login-shell backend resolution.
fn launch_spec(runtime: &Path, node: &Path, root: &TempRoot, extensions: &[PathBuf]) -> LaunchSpec {
    let mut args: Vec<String> = vec![
        runtime.to_string_lossy().into_owned(),
        "--mode".into(),
        "rpc".into(),
        "--no-extensions".into(),
    ];
    for extension in extensions {
        args.push("-e".into());
        args.push(extension.to_string_lossy().into_owned());
    }
    args.extend([
        "--session-dir".into(),
        root.dir("sessions").to_string_lossy().into_owned(),
        "--offline".into(),
        "--no-mcp".into(),
        "--no-skills".into(),
        "--no-context-files".into(),
    ]);
    LaunchSpec {
        program: node.to_path_buf(),
        args,
        cwd: root.dir("workspace"),
        env: isolation_env(root),
    }
}

// ---------------------------------------------------------------------------
// Incoming pump (bounded record waits)
// ---------------------------------------------------------------------------

enum PumpItem {
    Message(Incoming),
    Closed,
    Timeout,
}

/// Forwards the async-channel stream into a `std::sync::mpsc` receiver so the
/// probe can wait with a hard deadline (`recv_timeout`) instead of sleeping.
struct IncomingPump {
    rx: std::sync::mpsc::Receiver<Incoming>,
}

impl IncomingPump {
    fn next(&self, timeout: Duration) -> PumpItem {
        match self.rx.recv_timeout(timeout) {
            Ok(item) => PumpItem::Message(item),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => PumpItem::Timeout,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => PumpItem::Closed,
        }
    }
}

/// A backend record other than stderr (which is collected) or exit (an error).
enum LiveRecord {
    Ui(UiRequest),
    Event,
}

// ---------------------------------------------------------------------------
// The probe itself
// ---------------------------------------------------------------------------

struct Probe {
    process: Option<Arc<PiProcess>>,
    incoming: IncomingPump,
    session: DesktopSession,
    /// Monotonic ids for raw (not yet typed in the adapter) Desktop methods.
    raw_ids: u64,
    stderr: Vec<String>,
    root: TempRoot,
}

impl Probe {
    fn process(&self) -> Result<&Arc<PiProcess>> {
        self.process
            .as_ref()
            .ok_or_else(|| anyhow!("backend process is gone"))
    }

    /// Next `Ui` or `Event`, skipping stderr (collected) and failing on exit,
    /// channel close or timeout.
    fn next_live_record(&mut self, deadline: Instant) -> Result<LiveRecord> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "timed out waiting for backend output");
            match self.incoming.next(remaining) {
                PumpItem::Message(Incoming::Stderr(line)) => {
                    self.stderr.push(line.clone());
                    eprintln!("[backend stderr] {line}");
                }
                PumpItem::Message(Incoming::Exited(code)) => {
                    anyhow::bail!("backend exited ({code:?})")
                }
                PumpItem::Message(Incoming::Ui(ui)) => return Ok(LiveRecord::Ui(ui)),
                PumpItem::Message(Incoming::Event(_)) => return Ok(LiveRecord::Event),
                PumpItem::Closed => anyhow::bail!("backend closed"),
                PumpItem::Timeout => anyhow::bail!("timed out waiting for backend output"),
            }
        }
    }

    /// Next record the `DesktopSession` accepts, skipping unrelated widgets and
    /// plain session events.
    fn next_desktop_record_until(&mut self, deadline: Instant) -> Result<DesktopRecord> {
        loop {
            match self.next_live_record(deadline)? {
                LiveRecord::Ui(ui) => match self.session.accept(&ui) {
                    Ok(Some(record)) => return Ok(record),
                    Ok(None) => continue,
                    Err(error) => anyhow::bail!("Desktop session rejected a record: {error}"),
                },
                LiveRecord::Event => continue,
            }
        }
    }

    /// Issue a typed method, send its prompt, prove the outer `disposition` is
    /// `handled` (not the domain answer), then wait for the correlated widget
    /// response. Events seen before the response are consumed and discarded:
    /// they are unrelated startup events, and the session tracks their sequence.
    fn typed_roundtrip(&mut self, request: DesktopRequest) -> Result<DesktopResponse> {
        let request_id = request.request_id().to_string();
        let outer = pollster::block_on(self.process()?.request(request.prompt_command()))
            .map_err(|e| anyhow!("outer prompt for {request_id} failed: {e:#}"))?;
        let disposition = outer.data.get("disposition").and_then(Value::as_str);
        ensure!(
            disposition == Some("handled"),
            "outer prompt for {request_id} was not handled (disposition {disposition:?}); \
             the domain answer must arrive as a widget, not here"
        );
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            match self.next_desktop_record_until(deadline)? {
                DesktopRecord::Response(response) => {
                    ensure!(
                        response.request_id() == Some(request_id.as_str()),
                        "correlation mismatch: expected {request_id}, got {:?}",
                        response.request_id()
                    );
                    return Ok(response);
                }
                DesktopRecord::Event(_) => continue,
            }
        }
    }

    fn typed(&mut self, method: DesktopMethod) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue(method)
            .map_err(|e| anyhow!("issue {method}: {e}"))?;
        self.typed_roundtrip(request)
    }

    fn typed_set_permission(
        &mut self,
        mode: PermissionMode,
        persist: Option<bool>,
    ) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_set_permission_mode(mode, persist)
            .map_err(|e| anyhow!("issue set_permission_mode: {e}"))?;
        self.typed_roundtrip(request)
    }

    fn typed_get_config(&mut self, store: ConfigStore) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_get_config(store)
            .map_err(|e| anyhow!("issue get_config: {e}"))?;
        self.typed_roundtrip(request)
    }

    fn typed_set_config(
        &mut self,
        write: ConfigWrite,
        expected_revision: &str,
    ) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_set_config(write, expected_revision)
            .map_err(|e| anyhow!("issue set_config: {e}"))?;
        self.typed_roundtrip(request)
    }

    /// Issue `refresh_subscription_usage` for one provider and wait for its
    /// correlated response, failing if a `rate_limit` event appears either
    /// before or shortly after the answer. The child environment is fully
    /// offline, so no provider traffic happens.
    fn typed_refresh_subscription_usage(&mut self, provider: &str) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_refresh_subscription_usage(Some(provider))
            .map_err(|e| anyhow!("issue refresh_subscription_usage: {e}"))?;
        let request_id = request.request_id().to_string();
        let outer = pollster::block_on(self.process()?.request(request.prompt_command()))
            .map_err(|e| anyhow!("outer prompt for {request_id} failed: {e:#}"))?;
        let disposition = outer.data.get("disposition").and_then(Value::as_str);
        ensure!(
            disposition == Some("handled"),
            "outer prompt for {request_id} was not handled (disposition {disposition:?})"
        );
        let deadline = Instant::now() + STEP_TIMEOUT;
        let response = loop {
            match self.next_desktop_record_until(deadline)? {
                DesktopRecord::Response(response) => {
                    ensure!(
                        response.request_id() == Some(request_id.as_str()),
                        "correlation mismatch: expected {request_id}, got {:?}",
                        response.request_id()
                    );
                    break response;
                }
                DesktopRecord::Event(event) => {
                    ensure!(
                        event.event != "rate_limit",
                        "unexpected rate_limit event during refresh: {event:?}"
                    );
                }
            }
        };
        self.assert_no_rate_limit_event(Instant::now() + NO_EVENT_WINDOW)?;
        Ok(response)
    }

    /// Assert that no Desktop `rate_limit` event is published before
    /// `deadline`. Other records are consumed, and Desktop records still route
    /// through the session so its event sequence stays in step.
    fn assert_no_rate_limit_event(&mut self, deadline: Instant) -> Result<()> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            match self.incoming.next(remaining) {
                PumpItem::Message(Incoming::Stderr(line)) => {
                    self.stderr.push(line.clone());
                    eprintln!("[backend stderr] {line}");
                }
                PumpItem::Message(Incoming::Exited(code)) => {
                    anyhow::bail!("backend exited ({code:?})")
                }
                PumpItem::Message(Incoming::Ui(ui)) => match self.session.accept(&ui)? {
                    Some(DesktopRecord::Event(event)) => {
                        ensure!(
                            event.event != "rate_limit",
                            "unexpected rate_limit event: {event:?}"
                        );
                    }
                    Some(DesktopRecord::Response(_)) => {
                        anyhow::bail!("unexpected response while watching for rate_limit")
                    }
                    None => {}
                },
                PumpItem::Message(Incoming::Event(_)) => continue,
                PumpItem::Closed => anyhow::bail!("backend closed"),
                PumpItem::Timeout => return Ok(()),
            }
        }
    }

    /// A raw Desktop request (a method the typed adapter does not name yet),
    /// correlated by its own monotonic `probe-N` id.
    fn raw_roundtrip(&mut self, method: &str, params: Value) -> Result<RawDesktopAnswer> {
        self.raw_ids += 1;
        let request_id = format!("probe-{}", self.raw_ids);
        let payload = json!({
            "schema": SCHEMA,
            "requestId": request_id,
            "method": method,
            "params": params,
        })
        .to_string();
        let prompt = json!({
            "type": "prompt",
            "message": format!("{PROMPT_COMMAND} {payload}"),
        });
        let outer = pollster::block_on(self.process()?.request(prompt))
            .map_err(|e| anyhow!("outer prompt for {request_id} failed: {e:#}"))?;
        let disposition = outer.data.get("disposition").and_then(Value::as_str);
        ensure!(
            disposition == Some("handled"),
            "outer prompt for {request_id} was not handled (disposition {disposition:?})"
        );
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            match self.next_live_record(deadline)? {
                LiveRecord::Ui(ui) => {
                    let Some(line) = desktop_widget_line(&ui) else {
                        continue;
                    };
                    let value: Value = serde_json::from_str(line)
                        .map_err(|e| anyhow!("malformed Desktop widget JSON: {e}"))?;
                    if value.get("kind").and_then(Value::as_str) != Some("response") {
                        continue;
                    }
                    if value.get("requestId").and_then(Value::as_str) != Some(request_id.as_str()) {
                        continue;
                    }
                    return Ok(RawDesktopAnswer::from_value(&value));
                }
                LiveRecord::Event => continue,
            }
        }
    }

    /// A plain RPC command (`get_commands`, `get_messages`): the future
    /// resolves to its `data`.
    fn rpc_command(&mut self, command: Value) -> Result<Value> {
        let response = pollster::block_on(self.process()?.request(command))
            .map_err(|e| anyhow!("RPC command failed: {e:#}"))?;
        Ok(response.data)
    }

    fn run(&mut self) -> Result<()> {
        // 1. Discovery: the slash command must be registered before any prompt.
        let commands = self.rpc_command(json!({"type": "get_commands"}))?;
        assert_desktop_command_listed(&commands)?;

        // 2. Capabilities through the real DesktopSession.
        let capabilities = self
            .typed(DesktopMethod::Capabilities)?
            .capabilities()
            .map_err(|e| anyhow!("capabilities decode: {e}"))?;
        assert_capabilities(&capabilities)?;

        // 3. Snapshot: the config projection is present and profiles writable.
        let snapshot = self
            .typed(DesktopMethod::GetSnapshot)?
            .snapshot()
            .map_err(|e| anyhow!("snapshot decode: {e}"))?;
        assert_snapshot(&snapshot)?;

        // 4. Permission read: a fresh isolated session defaults to normal.
        let state = self
            .typed(DesktopMethod::GetPermissionMode)?
            .permission_mode()
            .map_err(|e| anyhow!("permission_mode decode: {e}"))?;
        ensure!(
            state.mode == PermissionMode::Normal && state.default_mode == PermissionMode::Normal,
            "fresh session must default to normal/normal, got {state:?}"
        );

        // 5. Permission set: normal -> auto, persist:false.
        let applied = self
            .typed_set_permission(PermissionMode::Auto, Some(false))?
            .set_permission_mode()
            .map_err(|e| anyhow!("set_permission_mode decode: {e}"))?;
        ensure!(
            applied.mode == PermissionMode::Auto,
            "live mode must become auto: {applied:?}"
        );
        ensure!(
            applied.default_mode == PermissionMode::Normal,
            "persist:false must leave the default normal: {applied:?}"
        );
        ensure!(
            !applied.persisted,
            "persist:false must not persist: {applied:?}"
        );
        ensure!(
            applied.changed,
            "normal -> auto must be a change: {applied:?}"
        );
        ensure!(
            applied.persist_error.is_none(),
            "no persist error expected: {applied:?}"
        );

        // The permission_mode_changed event follows the response, exactly once.
        let event = self.next_desktop_record_until(Instant::now() + STEP_TIMEOUT)?;
        let DesktopRecord::Event(event) = event else {
            anyhow::bail!("expected a permission event, got a response");
        };
        ensure!(
            event.event == "permission_mode_changed",
            "unexpected event: {event:?}"
        );
        let changed = event
            .permission_mode_changed()
            .map_err(|e| anyhow!("permission_mode_changed decode: {e}"))?;
        ensure!(
            changed.mode == PermissionMode::Auto
                && changed.previous_mode == PermissionMode::Normal
                && changed.default_mode == PermissionMode::Normal
                && !changed.persisted
                && changed.source == "desktop",
            "unexpected permission event payload: {changed:?}"
        );

        // No default file was written, and the same backend still answers (no
        // restart): persist:false changes only the live session.
        let permission_file = self.root.dir("config").join("permission-mode.json");
        ensure!(
            !permission_file.exists(),
            "persist:false must not write a default permission file"
        );
        let rechecked = self
            .typed(DesktopMethod::GetPermissionMode)?
            .permission_mode()
            .map_err(|e| anyhow!("permission_mode recheck decode: {e}"))?;
        ensure!(
            rechecked.mode == PermissionMode::Auto
                && rechecked.default_mode == PermissionMode::Normal,
            "live session must keep auto/normal after the set: {rechecked:?}"
        );

        // 6. History stays empty: Desktop prompts are handled by the extension.
        let messages = self.rpc_command(json!({"type": "get_messages"}))?;
        ensure!(
            messages
                .get("messages")
                .and_then(Value::as_array)
                .is_some_and(|messages| messages.is_empty()),
            "history must be empty after Desktop traffic, got {messages}"
        );

        // 7. Profiles read: capture the write revision from the missing store.
        let read = self
            .typed_get_config(ConfigStore::Profiles)?
            .config()
            .map_err(|e| anyhow!("get_config decode: {e}"))?;
        ensure!(
            read.store == ConfigStore::Profiles,
            "unexpected read: {read:?}"
        );
        ensure!(!read.read_only, "profiles must be writable: {read:?}");
        ensure!(
            read.status == ConfigStatus::Missing,
            "fresh profiles store: {read:?}"
        );
        let write_targets = read
            .write_targets
            .as_deref()
            .ok_or_else(|| anyhow!("profiles read has no writeTargets: {read:?}"))?;
        ensure!(
            write_targets.len() == 1,
            "one profiles write target: {read:?}"
        );
        let expected_revision = write_targets[0].write_revision.clone();
        ensure!(
            !expected_revision.is_empty(),
            "empty writeRevision: {read:?}"
        );

        // 7b. Profiles write: the canonical empty file, guarded by the revision.
        let profiles_data = ProfilesData::new(json!({
            "kind": "gentle-pi.agent_model_profiles",
            "version": 1,
            "profiles": {},
        }))
        .map_err(|e| anyhow!("profiles data: {e}"))?;
        let write = self
            .typed_set_config(ConfigWrite::Profiles(profiles_data), &expected_revision)?
            .set_config()
            .map_err(|e| anyhow!("set_config decode: {e}"))?;
        ensure!(write.changed, "write must change: {write:?}");
        ensure!(
            write.write_target == ConfigWriteTarget::Global,
            "profiles writes to the global target: {write:?}"
        );
        let new_write_revision = write.write_revision.clone();
        ensure!(
            !new_write_revision.is_empty() && new_write_revision != expected_revision,
            "writeRevision must advance after a write: {expected_revision} -> {new_write_revision}"
        );

        // The config_changed event follows the response (an event published
        // before it was discarded by the roundtrip, so this wait would time
        // out), carrying the same write revision.
        let deadline = Instant::now() + STEP_TIMEOUT;
        let config_event = loop {
            match self.next_desktop_record_until(deadline)? {
                DesktopRecord::Event(event) if event.event == "config_changed" => break event,
                DesktopRecord::Event(_) => continue,
                DesktopRecord::Response(response) => {
                    anyhow::bail!("expected config_changed, got a response: {response:?}")
                }
            }
        };
        let changed = config_event
            .config_changed()
            .map_err(|e| anyhow!("config_changed decode: {e}"))?;
        ensure!(
            changed.store == ConfigStore::Profiles,
            "config event store: {changed:?}"
        );
        ensure!(
            changed.write_target == ConfigWriteTarget::Global,
            "config event target: {changed:?}"
        );
        ensure!(
            changed.write_revision == new_write_revision,
            "config event writeRevision must match the response: {changed:?}"
        );
        ensure!(
            changed.revision == write.state.revision,
            "config event revision must match the response: {changed:?}"
        );
        ensure!(
            changed.source == "desktop",
            "config event source: {changed:?}"
        );

        // Disk bytes: the owner wrote exactly the canonical serialized file.
        let profiles_path = self.root.dir("config").join("profiles.json");
        let on_disk = std::fs::read_to_string(&profiles_path)
            .map_err(|e| anyhow!("read {}: {e}", profiles_path.display()))?;
        ensure!(
            on_disk == expected_profiles_bytes(),
            "profiles.json bytes mismatch:\n{on_disk:?}"
        );

        // 8. Offline usage: a supported provider with no data is `pending`.
        let usage =
            self.raw_roundtrip("get_subscription_usage", json!({"provider": "anthropic"}))?;
        let usage = usage.ensure_ok()?.clone();
        let providers = usage["providers"]
            .as_array()
            .ok_or_else(|| anyhow!("usage has no providers array: {usage}"))?;
        ensure!(
            providers.len() == 1,
            "exactly one provider requested: {usage}"
        );
        ensure!(
            providers[0]["provider"].as_str() == Some("anthropic"),
            "usage provider: {usage}"
        );
        ensure!(
            providers[0]["status"].as_str() == Some("pending"),
            "offline usage must be pending: {usage}"
        );
        ensure!(
            providers[0].get("limits").is_none(),
            "pending usage must carry no limits: {usage}"
        );

        // 8b. Refresh disposition: anthropic is offline/unsupported, so the
        // typed refresh reports `refresh: "unsupported"`, emits no rate_limit
        // event, and touches no network (the child env is fully offline).
        let refreshed = self
            .typed_refresh_subscription_usage("anthropic")?
            .subscription_usage()
            .map_err(|e| anyhow!("subscription_usage decode: {e}"))?;
        ensure!(
            refreshed.providers.len() == 1,
            "exactly one provider requested: {refreshed:?}"
        );
        ensure!(
            refreshed.providers[0].provider == "anthropic",
            "usage provider: {refreshed:?}"
        );
        ensure!(
            refreshed.providers[0].refresh == Some(UsageRefresh::Unsupported),
            "anthropic refresh must be unsupported: {refreshed:?}"
        );

        // 9. Stopped design canvas: no server, no designs, and no URL.
        let design = self.raw_roundtrip("get_design_canvas", json!({}))?;
        let design = design.ensure_ok()?.clone();
        ensure!(
            design["running"].as_bool() == Some(false),
            "canvas must be stopped: {design}"
        );
        ensure!(design["slug"].is_null(), "stopped canvas slug: {design}");
        ensure!(
            design["connectedClients"].as_i64() == Some(0),
            "stopped canvas clients: {design}"
        );
        ensure!(
            design["designs"].as_array().is_some_and(|d| d.is_empty()),
            "no designs in a fresh workspace: {design}"
        );
        ensure!(
            design.get("url").is_none(),
            "stopped canvas must not expose a URL: {design}"
        );

        // 10. Empty subagents and jobs.
        let agents = self
            .typed(DesktopMethod::ListSubagents)?
            .list_subagents()
            .map_err(|e| anyhow!("list_subagents decode: {e}"))?;
        ensure!(
            agents.agents.is_empty(),
            "no live subagents expected: {agents:?}"
        );

        let jobs = self
            .typed(DesktopMethod::ListJobs)?
            .list_jobs()
            .map_err(|e| anyhow!("list_jobs decode: {e}"))?;
        ensure!(jobs.jobs.is_empty(), "no live jobs expected: {jobs:?}");

        Ok(())
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        // Orderly shutdown first, then wait for the reaped child before the
        // temp root (dropped after this struct's fields) is removed. This also
        // runs on assertion failure, so the process tree is never leaked.
        process.shutdown(SHUTDOWN_GRACE);
        let deadline = Instant::now() + SHUTDOWN_GRACE + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.incoming.next(remaining) {
                PumpItem::Message(Incoming::Exited(_)) | PumpItem::Closed => break,
                PumpItem::Message(_) => continue,
                PumpItem::Timeout => break,
            }
        }
        // `process` (the last Arc) drops here, before the struct fields, so
        // `PiProcess`'s own Drop runs (a no-op after the explicit shutdown)
        // before `root` removes the temporary directory.
    }
}

// ---------------------------------------------------------------------------
// Raw answer parsing and small pure helpers
// ---------------------------------------------------------------------------

struct RawDesktopAnswer {
    request_id: Option<String>,
    ok: bool,
    data: Value,
    error_code: Option<String>,
    error_message: Option<String>,
}

impl RawDesktopAnswer {
    fn from_value(value: &Value) -> Self {
        let request_id = value
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let error = value.get("error").cloned().unwrap_or(Value::Null);
        let (data, error_code, error_message) = if ok {
            (
                value.get("data").cloned().unwrap_or(Value::Null),
                None,
                None,
            )
        } else {
            (
                Value::Null,
                error
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        };
        Self {
            request_id,
            ok,
            data,
            error_code,
            error_message,
        }
    }

    fn ensure_ok(&self) -> Result<&Value> {
        ensure!(
            self.ok,
            "Desktop request failed: {} {}",
            self.error_code.as_deref().unwrap_or(""),
            self.error_message.as_deref().unwrap_or("")
        );
        Ok(&self.data)
    }
}

fn desktop_widget_line(ui: &UiRequest) -> Option<&str> {
    match &ui.method {
        UiMethod::SetWidget { key, lines, .. } if key == WIDGET_KEY => {
            lines.as_ref()?.first().map(String::as_str)
        }
        _ => None,
    }
}

fn assert_desktop_command_listed(data: &Value) -> Result<()> {
    let commands = data
        .get("commands")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("get_commands response has no commands array: {data}"))?;
    let listed = commands
        .iter()
        .filter_map(|command| command.get("name").and_then(Value::as_str))
        .any(|name| name.trim_start_matches('/') == PROMPT_COMMAND.trim_start_matches('/'));
    ensure!(
        listed,
        "gentle:desktop command not listed by get_commands: {data}"
    );
    Ok(())
}

fn assert_capabilities(capabilities: &Capabilities) -> Result<()> {
    for method in [
        DesktopMethod::Capabilities,
        DesktopMethod::GetSnapshot,
        DesktopMethod::GetPermissionMode,
        DesktopMethod::SetPermissionMode,
        DesktopMethod::ListSubagents,
        DesktopMethod::CancelSubagent,
        DesktopMethod::ListJobs,
        DesktopMethod::CancelJob,
        DesktopMethod::GetSubscriptionUsage,
        DesktopMethod::RefreshSubscriptionUsage,
        DesktopMethod::GetConfig,
        DesktopMethod::SetConfig,
        DesktopMethod::GetDesignCanvas,
    ] {
        ensure!(
            capabilities.supports(method),
            "capabilities does not advertise {method}: {capabilities:?}"
        );
    }
    for event in [
        "config_changed",
        "permission_mode_changed",
        "rate_limit",
        "design_canvas",
        "agents_update",
        "jobs_update",
    ] {
        ensure!(
            capabilities.emits(event),
            "capabilities does not emit {event}"
        );
    }
    Ok(())
}

fn assert_snapshot(snapshot: &Snapshot) -> Result<()> {
    let stores = snapshot
        .state
        .get("config")
        .and_then(|config| config.get("stores"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!(
                "snapshot state has no config.stores array: {:?}",
                snapshot.state
            )
        })?;
    let profiles = stores
        .iter()
        .find(|store| store.get("store").and_then(Value::as_str) == Some("profiles"))
        .ok_or_else(|| anyhow!("snapshot has no profiles store"))?;
    ensure!(
        profiles.get("readOnly").and_then(Value::as_bool) == Some(false),
        "profiles must be writable: {profiles}"
    );
    Ok(())
}

/// What `serializeProfilesFile({kind, version, profiles: {}})` writes on disk.
fn expected_profiles_bytes() -> &'static str {
    "{\n  \"kind\": \"gentle-pi.agent_model_profiles\",\n  \"version\": 1,\n  \"profiles\": {}\n}\n"
}

fn spawn_probe(spec: LaunchSpec, root: TempRoot) -> Result<Probe> {
    let (process, incoming) =
        PiProcess::spawn(&spec).map_err(|e| anyhow!("spawn backend: {e:#}"))?;
    let (tx, rx) = std::sync::mpsc::sync_channel::<Incoming>(256);
    std::thread::Builder::new()
        .name("desktop-rpc-live-pump".into())
        .spawn(move || {
            while let Ok(item) = incoming.recv_blocking() {
                if tx.send(item).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| anyhow!("spawn pump thread: {e}"))?;
    Ok(Probe {
        process: Some(process),
        incoming: IncomingPump { rx },
        session: DesktopSession::new(),
        raw_ids: 0,
        stderr: Vec::new(),
        root,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live backend probe: set GENTLE_PI_RUNTIME_CLI to .../pi-coding-agent/dist/bundle/cli.js, then run with --ignored"]
fn live_desktop_shell_roundtrip() {
    let Some(runtime) = locate_runtime() else {
        return;
    };
    let Some(node) = find_node() else {
        eprintln!("desktop_rpc_live: node was not found on PATH; skipping live probe");
        return;
    };
    let Some(extensions) = locate_extensions(&runtime) else {
        return;
    };
    let root = TempRoot::new();
    let spec = launch_spec(&runtime, &node, &root, &extensions);
    let mut probe = spawn_probe(spec, root).unwrap_or_else(|error| panic!("{error:#}"));
    if let Err(error) = probe.run() {
        panic!(
            "live probe failed: {error:#}\n--- backend stderr ---\n{}",
            probe.stderr.join("\n")
        );
    }
    eprintln!("desktop_rpc_live: Desktop↔Shell roundtrip passed");
}

// --- Hermetic checks of the probe's own parsing (no backend required) --------

#[test]
fn desktop_widget_line_recognizes_only_the_desktop_key() {
    let desktop = UiRequest {
        id: "u1".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: WIDGET_KEY.into(),
            lines: Some(vec!["{}".into()]),
            placement: None,
        },
    };
    assert_eq!(desktop_widget_line(&desktop), Some("{}"));

    let other = UiRequest {
        id: "u2".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: "gentle-agents".into(),
            lines: Some(vec!["[]".into()]),
            placement: None,
        },
    };
    assert_eq!(desktop_widget_line(&other), None);

    let cleared = UiRequest {
        id: "u3".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: WIDGET_KEY.into(),
            lines: None,
            placement: None,
        },
    };
    assert_eq!(desktop_widget_line(&cleared), None);
}

#[test]
fn raw_answer_distinguishes_ok_from_error() {
    let ok = RawDesktopAnswer::from_value(&json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": "probe-1",
        "ok": true,
        "data": {"store": "profiles"},
    }));
    assert!(ok.ok);
    assert_eq!(ok.request_id.as_deref(), Some("probe-1"));
    assert_eq!(ok.data["store"], "profiles");
    assert_eq!(ok.error_code, None);
    assert!(ok.ensure_ok().is_ok());

    let error = RawDesktopAnswer::from_value(&json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": "probe-2",
        "ok": false,
        "error": {"code": "read_only", "message": "nope"},
    }));
    assert!(!error.ok);
    assert_eq!(error.error_code.as_deref(), Some("read_only"));
    assert_eq!(error.error_message.as_deref(), Some("nope"));
    assert!(error.ensure_ok().is_err());
}

#[test]
fn profiles_disk_bytes_match_the_owner_serializer() {
    assert_eq!(
        expected_profiles_bytes(),
        "{\n  \"kind\": \"gentle-pi.agent_model_profiles\",\n  \"version\": 1,\n  \"profiles\": {}\n}\n"
    );
}
