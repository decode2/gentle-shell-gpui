//! Repository pin files: reading and writing the clone pin and the
//! committed declaration, the Git worktree they belong to, and following a
//! renamed profile. Over Desktop RPC the `pins` store reports the same two
//! layers without their paths, and the clone layer is written by name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use gentle_core::profiles::{PinRead, PinStatus, is_valid_profile_name, parse_pin, serialize_pin};
use pi_rpc::desktop_rpc::{ConfigLayerSource, ConfigState, ConfigStatus, ConfigWrite, PinSource};
use serde_json::Value;

use super::files::{restore_bytes, write_atomic};

fn read_pin(path: &Path) -> PinRead {
    match fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => PinRead::Missing,
        Err(_) => PinRead::Invalid,
        Ok(text) => parse_pin(&text).map_or(PinRead::Invalid, PinRead::Valid),
    }
}

pub(super) fn write_pin(path: &Path, profile: &str) -> Result<(), String> {
    if !is_valid_profile_name(profile) {
        return Err(format!("Invalid profile name: {profile:?}."));
    }
    write_atomic(path, &serialize_pin(profile)).map_err(|e| e.to_string())
}

pub(super) fn clear_pin(path: &Path) -> io::Result<()> {
    restore_bytes(path, None)
}

/// `resolveSessionWorktreeWithGit`: worktree root and Git common directory.
fn worktree_identity(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let git = |arg: &str| -> Option<PathBuf> {
        let output = pi_rpc::command("git")
            .args(["--no-optional-locks", "-C"])
            .arg(cwd)
            .args(["rev-parse", "--path-format=absolute", arg])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        dunce::canonicalize(text.trim_end_matches(['\n', '\r'])).ok()
    };
    Some((git("--show-toplevel")?, git("--git-common-dir")?))
}

pub fn read_pin_status(cwd: &Path) -> Option<PinStatus> {
    let (root, common) = worktree_identity(cwd)?;
    Some(pin_status_at(root, &common))
}

pub(super) fn pin_status_at(root: PathBuf, common: &Path) -> PinStatus {
    let local_path = common.join("gentle-ai").join("profile-pin.json");
    let repo_path = root.join(".pi").join("gentle-ai").join("profile.json");
    PinStatus {
        local: read_pin(&local_path),
        repo: read_pin(&repo_path),
        local_path,
        repo_path,
    }
}

/// Rename follows the clone pin; a committed declaration is only reported.
pub(super) fn follow_renamed_pin(cwd: &Path, from: &str, to: &str) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Some(status) = read_pin_status(cwd) else {
        return (Vec::new(), Vec::new());
    };
    let mut followed = Vec::new();
    let mut declared = Vec::new();
    if status.local == PinRead::Valid(from.to_string()) && write_pin(&status.local_path, to).is_ok()
    {
        followed.push(status.local_path.clone());
    }
    if status.repo == PinRead::Valid(from.to_string()) {
        declared.push(status.repo_path.clone());
    }
    (followed, declared)
}

/// Where the clone pin lives, as the screen names it over Desktop RPC: the
/// Shell resolves the file, Desktop never does.
const LOCAL_PIN_LABEL: &str = "<Git common dir>/gentle-ai/profile-pin.json";
/// The committed declaration, relative to the worktree root.
const REPO_PIN_LABEL: &str = ".pi/gentle-ai/profile.json";

/// The `pins` store as the screen's pin status: `None` outside a Git
/// worktree. The store names a layer's profile only when it wins or is
/// stale, so a declaration shadowed by a winning clone pin is reported as a
/// note instead of being shown as absent.
pub(super) fn rpc_pin_status(state: &ConfigState) -> (Option<PinStatus>, Option<String>) {
    let value = state.value.as_ref();
    if state.layers.is_empty() || value.and_then(|v| v.get("reason")).is_some() {
        return (None, None);
    }
    let named = |source: &str| -> Option<String> {
        let value = value?;
        let names = |entry: &Value| {
            (entry.get("source").and_then(Value::as_str) == Some(source))
                .then(|| entry.get("profile").and_then(Value::as_str))
                .flatten()
                .map(str::to_string)
        };
        value
            .get("winner")
            .and_then(names)
            .or_else(|| value.get("stale")?.as_array()?.iter().find_map(names))
    };
    let mut shadowed = false;
    let mut layer = |source: ConfigLayerSource| {
        let status = state
            .layers
            .iter()
            .find(|layer| layer.source == source)
            .map_or(ConfigStatus::Missing, |layer| layer.status);
        match status {
            ConfigStatus::Missing => PinRead::Missing,
            ConfigStatus::Invalid => PinRead::Invalid,
            ConfigStatus::Ok => named(source.as_str()).map_or_else(
                || {
                    shadowed = true;
                    PinRead::Missing
                },
                PinRead::Valid,
            ),
        }
    };
    let status = PinStatus {
        local: layer(ConfigLayerSource::Local),
        repo: layer(ConfigLayerSource::Repo),
        local_path: PathBuf::from(LOCAL_PIN_LABEL),
        repo_path: PathBuf::from(REPO_PIN_LABEL),
    };
    let notice = shadowed.then(|| {
        format!(
            "This repository also has a committed declaration ({REPO_PIN_LABEL}); the clone pin takes precedence, so Gentle Shell does not report which profile it declares."
        )
    });
    (Some(status), notice)
}

/// "Use in this repo" over Desktop RPC: the clone layer is cleared when it
/// already pins `name`, else pinned to it. The flag says which.
pub(super) fn local_pin_write(
    status: Option<&PinStatus>,
    name: &str,
) -> Result<(ConfigWrite, bool), String> {
    let Some(status) = status else {
        return Err("This folder is not inside a Git worktree, so nothing was pinned.".to_string());
    };
    let pinned = status.local == PinRead::Valid(name.to_string());
    let write = if pinned {
        ConfigWrite::clear_pin(PinSource::Local)
    } else {
        ConfigWrite::pin(PinSource::Local, name)
    };
    Ok((write, pinned))
}

/// A rename follows the clone pin over Desktop RPC; the committed
/// declaration is only reported, like the direct-file path.
pub(super) fn renamed_pin_write(
    status: Option<&PinStatus>,
    from: &str,
    to: &str,
) -> Option<ConfigWrite> {
    (status?.local == PinRead::Valid(from.to_string()))
        .then(|| ConfigWrite::pin(PinSource::Local, to))
}
