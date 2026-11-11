//! Sidecar: make sure the orchestrator daemon is running before the Fleet
//! panel talks to it. The daemon is a separate Go binary (`orchestratord`);
//! this module finds it and spawns it detached, reading its configuration
//! from the app's own `orchestrator.json` (repo, repoPath, listen).

use std::path::Path;
use std::process::Child;
use std::sync::{Mutex, OnceLock};

/// The running daemon handle, so the app never spawns it twice.
static DAEMON: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Child>> {
    DAEMON.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Default, serde::Deserialize)]
struct DaemonConfig {
    #[serde(default)]
    repo: Option<String>,
    #[serde(rename = "repoPath", default)]
    repo_path: Option<String>,
    #[serde(default)]
    listen: Option<String>,
}

/// Resolves the daemon binary: `ORCHESTRATORD_BIN`, else `orchestratord` on
/// the PATH of the environment this app runs with.
fn daemon_binary() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("ORCHESTRATORD_BIN")
        && !explicit.trim().is_empty()
    {
        return Some(std::path::PathBuf::from(explicit));
    }
    which("orchestratord")
}

/// Minimal PATH lookup without the `which` crate: checks the PATH entries
/// for an executable file (plus .exe on Windows).
fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe_suffix = if cfg!(windows) { ".exe" } else { "" };
    for dir in std::env::split_paths(&path) {
        let candidate = dir
            .join(name)
            .with_extension(exe_suffix.trim_start_matches('.'));
        let candidate = if exe_suffix.is_empty() {
            dir.join(name)
        } else {
            candidate
        };
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Builds the daemon's arguments from its config file: `repo` becomes
/// `--repo`, `repoPath` becomes `--repo-path`, `listen` becomes `--listen`.
fn daemon_args(config_dir: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(config_dir.join("orchestrator.json")).unwrap_or_default();
    let Ok(config) = serde_json::from_str::<DaemonConfig>(&raw) else {
        return Vec::new();
    };
    let mut args = Vec::new();
    if let Some(repo) = config.repo.filter(|r| !r.trim().is_empty()) {
        args.push("--repo".into());
        args.push(repo);
    }
    if let Some(repo_path) = config.repo_path.filter(|r| !r.trim().is_empty()) {
        args.push("--repo-path".into());
        args.push(repo_path);
    }
    if let Some(listen) = config.listen.filter(|l| !l.trim().is_empty()) {
        args.push("--listen".into());
        args.push(listen);
    }
    args
}

/// Spawns the daemon if it is not already running under this app. Spawning
/// without a repo is fine: the daemon serves the board without polling. The
/// daemon never opens a console window (its binary links a GUI subsystem on
/// Windows and this spawn passes CREATE_NO_WINDOW).
pub fn ensure_daemon(config_dir: &Path) -> Result<(), String> {
    let mut slot_guard = slot().lock().map_err(|_| "daemon slot poisoned")?;
    let already_running = match slot_guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    };
    if already_running {
        return Ok(());
    }

    let binary = daemon_binary().ok_or_else(|| {
		"orchestratord not found: install the Gentle-AI orchestrator daemon or set ORCHESTRATORD_BIN".to_string()
	})?;
    let args = daemon_args(config_dir);

    let mut command = std::process::Command::new(&binary);
    command.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let child = command
        .spawn()
        .map_err(|e| format!("could not spawn {}: {e}", binary.display()))?;
    *slot_guard = Some(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_args_map_the_config_fields() {
        let dir = std::env::temp_dir().join("orch-sidecar-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("orchestrator.json"),
            r#"{"repo":"owner/repo","repoPath":"/tmp/repo","listen":"127.0.0.1:8095"}"#,
        )
        .unwrap();
        let args = daemon_args(&dir);
        assert_eq!(
            args,
            vec![
                "--repo",
                "owner/repo",
                "--repo-path",
                "/tmp/repo",
                "--listen",
                "127.0.0.1:8095"
            ]
        );
    }

    #[test]
    fn daemon_args_tolerate_a_missing_or_invalid_config() {
        assert!(daemon_args(Path::new("/definitely/not/here")).is_empty());
        let dir = std::env::temp_dir().join("orch-sidecar-test-bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("orchestrator.json"), "{not-json").unwrap();
        assert!(daemon_args(&dir).is_empty());
    }
}
