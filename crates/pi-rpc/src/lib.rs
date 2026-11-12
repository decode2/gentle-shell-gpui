//! Client for Pi's RPC mode (`pi --mode rpc`, or `gentle-shell --mode rpc`).
//!
//! The protocol is strict JSONL over the child's stdin/stdout: commands go in,
//! `response` records (correlated by `id`), session events and
//! `extension_ui_request` records come out. Records are split on LF only, never
//! on Unicode line separators, which are valid inside JSON strings.

pub mod desktop_rpc;
pub mod file_cache;
mod home;
mod launch;
mod login;
mod record;

pub use home::{Homes, adopt_standalone_config, launcher_config_path, pi_package, resolve_homes};
pub use login::{AuthEvent, AuthPrompt, LoginHelper, LoginMessage, PromptOption, ProviderInfo};

pub use launch::{
    LaunchSpec, backend_version, child_path, command, find_program, forget_cached, forget_versions,
    resolve_backend, resolve_node, set_extra_path, set_system_prompt, version_output,
};
pub use record::{Record, Response, UiMethod, UiRequest, parse_record};

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Everything the child process reports that is not a correlated response.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// A session event (`agent_start`, `message_update`, `tool_execution_end`, …).
    Event(Value),
    /// An extension UI request (dialogs and fire-and-forget updates).
    Ui(UiRequest),
    /// One diagnostic line from stderr. Never protocol data.
    Stderr(String),
    /// The child exited; `None` when it was killed by a signal.
    Exited(Option<i32>),
}

type Pending = Arc<Mutex<HashMap<String, async_channel::Sender<Response>>>>;

/// A running Pi RPC child process.
pub struct PiProcess {
    stdin: Mutex<Option<ChildStdin>>,
    child: Arc<Mutex<Child>>,
    pending: Pending,
    next_id: AtomicU64,
    /// Set by the first [`Self::shutdown`], so Drop after an explicit
    /// shutdown doesn't signal and reap twice.
    stopping: AtomicBool,
}

impl PiProcess {
    /// Spawn the backend and start the reader threads. `incoming` receives
    /// every event, UI request, stderr line and finally `Exited`.
    pub fn spawn(spec: &LaunchSpec) -> Result<(Arc<Self>, async_channel::Receiver<Incoming>)> {
        let mut command = crate::command(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group, so the final kill in `shutdown` also reaches
        // Pi and the tool processes it started. Interrupts are RPC commands
        // (`abort`), not signals, and the launcher forwards SIGTERM by pid.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start {}", spec.program.display()))?;
        let stdin = child.stdin.take().context("child stdin unavailable")?;
        let stdout = child.stdout.take().context("child stdout unavailable")?;
        let stderr = child.stderr.take().context("child stderr unavailable")?;

        let (tx, rx) = async_channel::unbounded();
        let pending: Pending = Arc::default();
        let child = Arc::new(Mutex::new(child));

        {
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("pi-rpc-stderr".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines() {
                        let Ok(line) = line else { break };
                        if tx.send_blocking(Incoming::Stderr(line)).is_err() {
                            break;
                        }
                    }
                })?;
        }
        {
            let pending = pending.clone();
            let child = child.clone();
            std::thread::Builder::new()
                .name("pi-rpc-stdout".into())
                .spawn(move || {
                    read_records(stdout, &pending, &tx);
                    // Unblock every caller still waiting for a response.
                    pending.lock().unwrap().clear();
                    let code = wait_child(&child);
                    let _ = tx.send_blocking(Incoming::Exited(code));
                })?;
        }

        let process = Arc::new(Self {
            stdin: Mutex::new(Some(stdin)),
            child,
            pending,
            next_id: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
        });
        Ok((process, rx))
    }

    /// Send a command; the future resolves to its correlated response.
    /// `command` must be a JSON object with a `type`; an `id` is assigned
    /// here. The command is written now and the future holds only the
    /// response channel, so a request still waiting (a quiet `bash`, a
    /// compaction) never keeps the process alive.
    pub fn request(&self, command: Value) -> impl Future<Output = Result<Response>> + use<> {
        let sent = self.send(command);
        async move {
            let response = sent?
                .recv()
                .await
                .map_err(|_| anyhow!("gentle-shell exited before answering"))?;
            if response.success {
                Ok(response)
            } else {
                Err(anyhow!(
                    "{}",
                    response.error.unwrap_or_else(|| "command failed".into())
                ))
            }
        }
    }

    /// Write `command` with a fresh `id`; the receiver gets its response.
    fn send(&self, mut command: Value) -> Result<async_channel::Receiver<Response>> {
        let id = format!("gs-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let object = command
            .as_object_mut()
            .ok_or_else(|| anyhow!("RPC command must be a JSON object"))?;
        object.insert("id".into(), Value::String(id.clone()));
        let (tx, rx) = async_channel::bounded(1);
        self.pending.lock().unwrap().insert(id.clone(), tx);
        if let Err(error) = self.write(&command) {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        Ok(rx)
    }

    /// Answer an `extension_ui_request` dialog.
    pub fn respond_ui(&self, id: &str, answer: UiAnswer) -> Result<()> {
        let record = match answer {
            UiAnswer::Value(value) => {
                json!({"type": "extension_ui_response", "id": id, "value": value})
            }
            UiAnswer::Confirmed(confirmed) => {
                json!({"type": "extension_ui_response", "id": id, "confirmed": confirmed})
            }
            UiAnswer::Cancelled => {
                json!({"type": "extension_ui_response", "id": id, "cancelled": true})
            }
        };
        self.write(&record)
    }

    fn write(&self, record: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard
            .as_mut()
            .ok_or_else(|| anyhow!("gentle-shell is shutting down"))?;
        stdin.write_all(&line)?;
        stdin.flush()?;
        Ok(())
    }

    /// Request an orderly shutdown and return at once. On Unix it sends
    /// SIGTERM: the launcher forwards it to Pi, whose handler stops the
    /// detached processes its agents started. Elsewhere it closes stdin.
    /// Once the backend exits, or after `grace`, whatever is left of it is
    /// killed (see [`kill_tree`]) and the child reaped. Only the first call
    /// acts.
    pub fn shutdown(&self, grace: Duration) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        // Taken now so nothing more is written.
        let stdin = self.stdin.lock().unwrap().take();
        let child = self.child.clone();
        // Under the lock nothing reaps the child, so its pid (and group id)
        // is still its own when signalled. One that already exited was just
        // reaped here; its pid may be reused, so nothing is signalled.
        let mut guard = child.lock().unwrap();
        if !matches!(guard.try_wait(), Ok(None)) {
            return;
        }
        #[cfg(unix)]
        let term = signal("-TERM", &guard.id().to_string());
        // On stdin EOF Pi drops its signal handler and exits without stopping
        // its detached processes, so stdin stays open while SIGTERM does
        // that; it closes with the exit, or after `grace`. Without a signal,
        // EOF is the request.
        #[cfg(unix)]
        let stdin = stdin.filter(|_| term.is_some());
        #[cfg(not(unix))]
        let stdin: Option<ChildStdin> = {
            drop(stdin);
            None
        };
        drop(guard);
        let _ = std::thread::Builder::new()
            .name("pi-rpc-shutdown".into())
            .spawn(move || {
                #[cfg(unix)]
                if let Some(mut term) = term {
                    let _ = term.wait();
                }
                let deadline = Instant::now() + grace;
                while Instant::now() < deadline
                    && matches!(child.lock().unwrap().try_wait(), Ok(None))
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                drop(stdin);
                kill_tree(&mut child.lock().unwrap());
            });
    }
}

impl Drop for PiProcess {
    fn drop(&mut self) {
        self.shutdown(Duration::from_secs(3));
    }
}

/// The answer to a dialog request.
#[derive(Debug, Clone)]
pub enum UiAnswer {
    Value(String),
    Confirmed(bool),
    Cancelled,
}

fn read_records(stdout: impl Read, pending: &Pending, tx: &async_channel::Sender<Incoming>) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::with_capacity(4096);
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let incoming = match parse_record(&line) {
            Ok(Record::Response(response)) => {
                let sender = response
                    .id
                    .as_ref()
                    .and_then(|id| pending.lock().unwrap().remove(id));
                if let Some(sender) = sender {
                    let _ = sender.send_blocking(response);
                } else if !response.success {
                    let error = response.error.unwrap_or_default();
                    let _ = tx.send_blocking(Incoming::Stderr(format!(
                        "{} failed: {error}",
                        response.command
                    )));
                }
                continue;
            }
            Ok(Record::Ui(request)) => Incoming::Ui(request),
            Ok(Record::Event(event)) => Incoming::Event(event),
            Ok(Record::Empty) => continue,
            Err(error) => Incoming::Stderr(format!("unparseable RPC record: {error}")),
        };
        if tx.send_blocking(incoming).is_err() {
            return;
        }
    }
}

fn wait_child(child: &Mutex<Child>) -> Option<i32> {
    loop {
        match child.lock().unwrap().try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {}
            Err(_) => return None,
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kill what is left of a backend and reap it: on Unix its whole process
/// group (the launcher, Pi and the tool processes still in it; a leader
/// that exited moments ago keeps its group id while members remain), on
/// Windows its process tree while it runs (an exited one's pid is not
/// looked up). Then the child itself, in case that failed.
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    #[cfg(unix)]
    if let Some(mut kill) = signal("-KILL", &format!("-{pid}")) {
        let _ = kill.wait();
    }
    #[cfg(windows)]
    if matches!(child.try_wait(), Ok(None)) {
        let _ = crate::command("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Start `kill <signal> -- <target>` (a negative target is a process
/// group); the caller reaps it. Spawned rather than waited so app quit
/// doesn't wait on it: it delivers the signal even after the app exits.
#[cfg(unix)]
fn signal(signal: &str, target: &str) -> Option<Child> {
    crate::command("kill")
        .args([signal, "--", target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}
