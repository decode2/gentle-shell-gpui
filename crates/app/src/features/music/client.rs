//! Runs gentle-stream's `ctl --json` CLI to control the background player.
//! Blocking; callers run it off the main thread. Every request is one short
//! process (no socket, no key), started with `pi_rpc::command` and the app's
//! combined PATH.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use gentle_core::music::{self, MusicState};

/// Why a request didn't get an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The player answered `nothing_playing`: this action needs one.
    NoPlayer,
    Failed(String),
}

/// The ctl arguments for one action: `--gentle-shell <shell> ctl <action>
/// [value] --json`. The launcher precedes `ctl` (only `mood` passes one), and
/// `value` is one argument (`volume`'s percent, or `mood`'s text, spaces
/// included).
pub fn args(action: &str, value: Option<&str>, shell: Option<&Path>) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();
    if let Some(shell) = shell {
        out.push("--gentle-shell".into());
        out.push(shell.as_os_str().to_os_string());
    }
    out.push("ctl".into());
    out.push(action.into());
    if let Some(value) = value {
        out.push(value.into());
    }
    out.push("--json".into());
    out
}

/// One ctl request against `program`, bounded by `timeout`.
pub fn run(
    program: &Path,
    action: &str,
    value: Option<&str>,
    shell: Option<&Path>,
    timeout: Duration,
) -> Result<Option<MusicState>, Error> {
    run_with(program, action, value, shell, timeout, spawn)
}

/// What the process runner reports back.
#[derive(Clone, Debug)]
struct Output {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run `program args…` with the app's combined PATH, reading both pipes to
/// the end and killing the child past `timeout`.
fn spawn(program: &Path, args: &[OsString], timeout: Duration) -> Result<Output, String> {
    let mut child = pi_rpc::command(program)
        .args(args)
        .env("PATH", pi_rpc::child_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("gentle-stream didn't start: {e}"))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("gentle-stream didn't answer in time.".into());
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(Output {
        success,
        stdout,
        stderr,
    })
}

/// The process runner, injectable so tests can drive [`run_with`] without a
/// real player, socket or network.
type Runner = fn(&Path, &[OsString], Duration) -> Result<Output, String>;

fn run_with(
    program: &Path,
    action: &str,
    value: Option<&str>,
    shell: Option<&Path>,
    timeout: Duration,
    runner: Runner,
) -> Result<Option<MusicState>, Error> {
    let args = args(action, value, shell);
    let output = runner(program, &args, timeout).map_err(Error::Failed)?;
    interpret(&output)
}

fn interpret(output: &Output) -> Result<Option<MusicState>, Error> {
    match music::parse_ctl(&String::from_utf8_lossy(&output.stdout)) {
        Ok(state) => Ok(state),
        Err(music::CtlError::NothingPlaying) => Err(Error::NoPlayer),
        Err(music::CtlError::InvalidRequest(message)) => Err(Error::Failed(message)),
        Err(music::CtlError::Unavailable(message)) => Err(Error::Failed(message)),
        Err(music::CtlError::Malformed) => Err(Error::Failed(last_stderr(output))),
    }
}

fn last_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if output.success {
                "gentle-stream didn't answer".into()
            } else {
                "gentle-stream failed".into()
            }
        })
}

/// Read a child's pipe to the end on its own thread, so neither pipe fills
/// while the other is read.
fn drain(stream: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut bytes);
        }
        bytes
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static RUNS: RefCell<Vec<RunCall>> = const { RefCell::new(Vec::new()) };
        static REPLY: RefCell<Result<Output, String>> = const { RefCell::new(Ok(Output {
            success: true,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })) };
    }

    #[derive(Clone, Debug)]
    struct RunCall {
        program: PathBuf,
        args: Vec<OsString>,
        timeout: Duration,
    }

    fn fake(program: &Path, args: &[OsString], timeout: Duration) -> Result<Output, String> {
        RUNS.with(|runs| {
            runs.borrow_mut().push(RunCall {
                program: program.to_path_buf(),
                args: args.to_vec(),
                timeout,
            })
        });
        REPLY.with(|reply| reply.borrow().clone())
    }

    fn reply_ok(stdout: &str) {
        REPLY.with(|reply| {
            *reply.borrow_mut() = Ok(Output {
                success: true,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        });
    }

    fn reply_err(stderr: &str) {
        REPLY.with(|reply| {
            *reply.borrow_mut() = Err(stderr.to_string());
        })
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn builds_exact_args_for_every_action() {
        assert_eq!(args("start", None, None), os(&["ctl", "start", "--json"]));
        assert_eq!(args("status", None, None), os(&["ctl", "status", "--json"]));
        assert_eq!(args("toggle", None, None), os(&["ctl", "toggle", "--json"]));
        assert_eq!(args("next", None, None), os(&["ctl", "next", "--json"]));
        assert_eq!(args("stop", None, None), os(&["ctl", "stop", "--json"]));
        assert_eq!(
            args("volume", Some("60"), None),
            os(&["ctl", "volume", "60", "--json"])
        );
        // The launcher precedes `ctl`, and the mood text is one argument.
        assert_eq!(
            args(
                "mood",
                Some("rainy night"),
                Some(Path::new("/bin/gentle-shell"))
            ),
            os(&[
                "--gentle-shell",
                "/bin/gentle-shell",
                "ctl",
                "mood",
                "rainy night",
                "--json"
            ])
        );
    }

    #[test]
    fn runs_the_resolved_binary_with_the_given_timeout() {
        reply_ok(r#"{"ok": true, "running": false}"#);
        let program = Path::new("/tc/music/bin/gentle-stream");
        let result = run_with(program, "status", None, None, Duration::from_secs(5), fake);
        assert_eq!(result, Ok(None));
        let run = RUNS.with(|runs| runs.borrow()[0].clone());
        assert_eq!(run.program, program);
        assert_eq!(run.args, os(&["ctl", "status", "--json"]));
        assert_eq!(run.timeout, Duration::from_secs(5));
    }

    #[test]
    fn maps_a_snapshot_and_nothing_playing() {
        reply_ok(
            r#"{"ok": true, "running": true, "playing": true, "volume": 80, "name": "Cafe Rain", "genre": "jazz", "mood": "rainy cafe"}"#,
        );
        let state = run_with(
            Path::new("g"),
            "status",
            None,
            None,
            Duration::from_secs(1),
            fake,
        )
        .unwrap()
        .unwrap();
        assert_eq!(state.name, "Cafe Rain");
        assert_eq!(state.volume, 0.8);

        reply_ok(
            r#"{"ok": false, "error": {"code": "nothing_playing", "message": "nothing is playing"}}"#,
        );
        assert_eq!(
            run_with(
                Path::new("g"),
                "toggle",
                None,
                None,
                Duration::from_secs(1),
                fake
            ),
            Err(Error::NoPlayer)
        );
    }

    #[test]
    fn maps_structured_and_process_failures() {
        reply_ok(
            r#"{"ok": false, "error": {"code": "unavailable", "message": "the player is unavailable"}}"#,
        );
        assert_eq!(
            run_with(
                Path::new("g"),
                "start",
                None,
                None,
                Duration::from_secs(1),
                fake
            ),
            Err(Error::Failed("the player is unavailable".into()))
        );

        reply_err("gentle-stream didn't start: no such file");
        assert_eq!(
            run_with(
                Path::new("g"),
                "status",
                None,
                None,
                Duration::from_secs(1),
                fake
            ),
            Err(Error::Failed(
                "gentle-stream didn't start: no such file".into()
            ))
        );
    }

    #[test]
    fn malformed_output_falls_back_to_stderr() {
        REPLY.with(|reply| {
            *reply.borrow_mut() = Ok(Output {
                success: false,
                stdout: b"not json".to_vec(),
                stderr: b"boom\n".to_vec(),
            })
        });
        assert_eq!(
            run_with(
                Path::new("g"),
                "status",
                None,
                None,
                Duration::from_secs(1),
                fake
            ),
            Err(Error::Failed("boom".into()))
        );
    }
}
