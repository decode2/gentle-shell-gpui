//! gentle-stream, the generative music player behind Gentle Shell's
//! `/gentle:music`: finding it, installing it into the app's own toolchain
//! the way Gentle Shell's bundled install does (a pinned uv, a uv-managed
//! Python 3.13 and the same pinned gentle-stream archive), its optional
//! sampled instruments, and removing what the app installed. The player
//! itself is driven by `features::music` through its `ctl` CLI.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::probe::run_logged;
use super::{Log, fetch, refresh_paths, toolchain_dir, user_home};

/// The gentle-stream commit Gentle Shell's `/gentle:music` offer installs,
/// as a GitHub archive (no git needed).
const ARCHIVE: &str = "https://github.com/Gentleman-Programming/gentle-stream/archive/7538e09257999c01c60da35d2acbb8fd699d5148.tar.gz";
const PYTHON: &str = "3.13";

/// gentle-stream needs POSIX (Unix sockets, `fcntl` locks).
pub const SUPPORTED: bool = cfg!(unix);

fn spec(samples: bool) -> String {
    format!(
        "gentle-stream{} @ {ARCHIVE}",
        if samples { "[samples]" } else { "" }
    )
}

/// `<toolchain>/music`: uv, its Python, the player and uv's cache.
fn music_dir() -> Option<PathBuf> {
    toolchain_dir().map(|toolchain| toolchain.join("music"))
}

fn app_player() -> Option<PathBuf> {
    music_dir()
        .map(|dir| dir.join("bin").join("gentle-stream"))
        .filter(|path| path.is_file())
}

/// The player: the app's own, the one on the PATH (`uv tool install`), or
/// the one Gentle Shell's bundled install put in `~/.gentle-shell/music`.
pub fn player() -> Option<PathBuf> {
    pick_player(
        app_player(),
        pi_rpc::find_program("gentle-stream"),
        user_home().join(".gentle-shell/music/bin/gentle-stream"),
    )
}

/// The first usable player, in preference order: the app's own, the one on
/// the PATH, then Gentle Shell's bundled install (when its file exists).
fn pick_player(app: Option<PathBuf>, path: Option<PathBuf>, bundled: PathBuf) -> Option<PathBuf> {
    if let Some(player) = app {
        return Some(player);
    }
    if let Some(player) = path {
        return Some(player);
    }
    bundled.is_file().then_some(bundled)
}
/// What Settings and the title bar need to know.
#[derive(Clone, Debug, PartialEq)]
pub struct MusicStatus {
    pub player: Option<PathBuf>,
    /// The player is the app's own, so Remove can delete it.
    pub app_owned: bool,
    /// `gentle-stream setup-samples` has put the soundfont in place.
    pub samples: bool,
}

impl MusicStatus {
    /// The player can be started and steered.
    pub fn usable(&self) -> bool {
        SUPPORTED && self.player.is_some()
    }
}

fn samples_installed() -> bool {
    let cache =
        gentle_core::music::cache_dir(std::env::var_os("XDG_CACHE_HOME").as_deref(), &user_home());
    cache.join("FluidR3_GM.sf2").is_file()
}

/// Blocking: looks for the player and its soundfont.
pub fn status() -> MusicStatus {
    let player = if SUPPORTED { player() } else { None };
    MusicStatus {
        app_owned: player.is_some() && player == app_player(),
        samples: samples_installed(),
        player,
    }
}

/// uv's environment for the app's music folder: tools, their executables,
/// the managed Python and the cache all inside it; no system Python, no
/// uv.toml. Inherited `UV_*`, `PYTHON*`, `PIP_*`, `VIRTUAL_ENV` and
/// `CONDA_PREFIX` are dropped, as Gentle Shell's installer does.
fn uv_command(uv: &Path, music: &Path) -> Command {
    let mut command = pi_rpc::command(uv);
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_uppercase();
        if upper.starts_with("UV_")
            || upper.starts_with("PYTHON")
            || upper.starts_with("PIP_")
            || upper == "VIRTUAL_ENV"
            || upper == "CONDA_PREFIX"
        {
            command.env_remove(&key);
        }
    }
    command
        .current_dir(music)
        .env("UV_TOOL_DIR", music.join("tools"))
        .env("UV_TOOL_BIN_DIR", music.join("bin"))
        .env("UV_PYTHON_INSTALL_DIR", music.join("python"))
        .env("UV_PYTHON_BIN_DIR", music.join("python").join("bin"))
        .env("UV_CACHE_DIR", music.join("cache"))
        .env("UV_PYTHON", PYTHON)
        .env("UV_PYTHON_PREFERENCE", "only-managed")
        .env("UV_PYTHON_DOWNLOADS", "automatic")
        .env("UV_NO_CONFIG", "1");
    command
}

/// uv, Python 3.13 and gentle-stream into `<toolchain>/music`. The sampled
/// instruments' Python binding needs the system FluidSynth library; without
/// it, gentle-stream installs without them (its instruments are synthesized).
pub fn install(log: &Log) -> Result<(), String> {
    if !SUPPORTED {
        return Err("gentle-stream runs on macOS and Linux only.".into());
    }
    let music = music_dir().ok_or("This system has no app data folder.")?;
    std::fs::create_dir_all(&music)
        .map_err(|e| format!("Couldn't create {}: {e}", music.display()))?;
    let uv = music.join("uv");
    fetch::install_uv(&uv, log)?;
    log(format!(
        "Installing Python {PYTHON} and gentle-stream; this can take a few minutes…"
    ));
    let install = |samples: bool| {
        let mut command = uv_command(&uv, &music);
        command.args([
            "tool",
            "install",
            "--force",
            "--python",
            PYTHON,
            &spec(samples),
        ]);
        run_logged(command, "uv tool install", log)
    };
    if install(true).is_err() {
        log(
            "The sampled instruments need the system FluidSynth library; installing without them…"
                .into(),
        );
        install(false)?;
    }
    let mut clean = uv_command(&uv, &music);
    clean.args(["cache", "clean"]);
    let _ = run_logged(clean, "uv cache clean", &|_| {});
    refresh_paths();
    if app_player().is_none() {
        return Err("uv finished, but gentle-stream isn't where it should be.".into());
    }
    log("Done. The ♪ button in the title bar plays music.".into());
    Ok(())
}

/// `gentle-stream setup-samples`: the FluidR3_GM soundfont (about 120 MB,
/// once) for sampled instruments.
pub fn install_samples(log: &Log) -> Result<(), String> {
    let player = player().ok_or("Install gentle-stream first.")?;
    log("Downloading the FluidR3_GM soundfont (about 120 MB)…".into());
    let mut command = pi_rpc::command(&player);
    command.arg("setup-samples");
    run_logged(command, "gentle-stream setup-samples", log)?;
    log("Sampled instruments are in place; the next piece uses them.".into());
    Ok(())
}

/// Stop the music, then delete what the app installed. A gentle-stream you
/// installed yourself is left alone.
pub fn remove(log: &Log) -> Result<(), String> {
    if let Some(player) = app_player() {
        log("Stopping the music…".into());
        let mut stop = pi_rpc::command(&player);
        stop.args(["ctl", "stop"]);
        let _ = run_logged(stop, "gentle-stream ctl stop", &|_| {});
    }
    let music = music_dir().ok_or("This system has no app data folder.")?;
    if music.exists() {
        log(format!("Deleting {}…", music.display()));
        std::fs::remove_dir_all(&music)
            .map_err(|e| format!("Couldn't delete {}: {e}", music.display()))?;
    }
    refresh_paths();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_player_in_preference_order() {
        let app = PathBuf::from("/tc/music/bin/gentle-stream");
        let path = PathBuf::from("/usr/local/bin/gentle-stream");
        let bundled =
            std::env::temp_dir().join(format!("gentle-stream-pick-{}", std::process::id()));

        // The app's own player wins over the PATH and the bundled install.
        assert_eq!(
            pick_player(Some(app.clone()), Some(path.clone()), bundled.clone()),
            Some(app)
        );
        // The PATH wins when there is no app player.
        assert_eq!(
            pick_player(None, Some(path.clone()), bundled.clone()),
            Some(path)
        );
        // A bundled install that isn't there is not a player.
        assert_eq!(pick_player(None, None, bundled), None);
    }

    #[test]
    fn accepts_the_bundled_player_when_its_file_exists() {
        let dir =
            std::env::temp_dir().join(format!("gentle-stream-pick-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bundled = dir.join("gentle-stream");
        std::fs::write(&bundled, b"#!/bin/sh\n").unwrap();
        assert_eq!(pick_player(None, None, bundled.clone()), Some(bundled));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
