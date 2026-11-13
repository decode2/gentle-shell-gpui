//! gentle-stream's `ctl --json` envelope and the state it reports: the
//! background player behind Gentle Shell's `/gentle:music`.
//!
//! The app drives the player with `gentle-stream ctl <action> --json`. A
//! successful answer is one machine envelope with the fields the player can
//! always give:
//!
//! ```json
//! {"ok": true, "running": true, "playing": true, "volume": 80,
//!  "name": "Cafe Rain", "genre": "jazz", "mood": "rainy cafe"}
//! ```
//!
//! `running: false` means nothing is playing (`status` with no player, or
//! `stop`). Failures are structured too:
//!
//! ```json
//! {"ok": false, "error": {"code": "nothing_playing", "message": "…"}}
//! ```
//!
//! Only `ok`, `running`, `playing`, `volume` (a whole percent 0–100), `name`,
//! `genre` and `mood` are guaranteed. The snapshot never carries a key, mode,
//! bpm or spectrum, so the desktop degrades those displays to empty.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// What the player is doing: the piece you hear, not the bar being rendered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MusicState {
    pub playing: bool,
    /// 0–1.
    pub volume: f32,
    pub name: String,
    /// The words that steered it ("rainy night in a small café"), if any.
    pub mood: String,
    pub genre: String,
    /// Not part of the ctl snapshot; kept so the UI degrades to empty.
    pub key: String,
    pub mode: String,
    pub bpm: f32,
    /// Not part of the ctl snapshot; stays empty.
    pub spectrum: Vec<f32>,
}

impl MusicState {
    /// The snapshot only guarantees the genre; key, mode and bpm are never
    /// present, so they are not invented here.
    pub fn details(&self) -> String {
        let mut parts = Vec::new();
        if !self.genre.is_empty() {
            parts.push(self.genre.clone());
        }
        parts.join(" · ")
    }
}

/// A ctl failure, from its structured error envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CtlError {
    /// `nothing_playing`: the action needs a running player.
    NothingPlaying,
    /// `invalid_request`: bad volume or mood text.
    InvalidRequest(String),
    /// `unavailable`: the player or the model could not do it.
    Unavailable(String),
    /// Not a machine envelope at all (empty, non-JSON, or JSON without `ok`).
    Malformed,
}

/// Parse one `ctl --json` answer into the player's state.
///
/// `Ok(None)` means nothing is playing (`running: false`). The
/// `nothing_playing` error is returned as [`CtlError::NothingPlaying`] so the
/// caller can treat an action against an absent player as cleanly stopped.
pub fn parse_ctl(stdout: &str) -> Result<Option<MusicState>, CtlError> {
    let value: Value = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .ok_or(CtlError::Malformed)?;
    match value["ok"].as_bool() {
        Some(false) => Err(error_of(&value)),
        Some(true) => {
            if value["running"] != true {
                return Ok(None);
            }
            Ok(Some(state_of(&value)))
        }
        None => Err(CtlError::Malformed),
    }
}

fn error_of(value: &Value) -> CtlError {
    let message = value["error"]["message"]
        .as_str()
        .unwrap_or("gentle-stream refused the request")
        .to_string();
    match value["error"]["code"].as_str() {
        Some("nothing_playing") => CtlError::NothingPlaying,
        Some("invalid_request") => CtlError::InvalidRequest(message),
        _ => CtlError::Unavailable(message),
    }
}

fn state_of(value: &Value) -> MusicState {
    let volume = value["volume"].as_f64().unwrap_or(0.) as f32;
    MusicState {
        playing: value["playing"].as_bool().unwrap_or(false),
        volume: volume / 100.,
        name: value["name"].as_str().unwrap_or_default().to_string(),
        mood: value["mood"].as_str().unwrap_or_default().to_string(),
        genre: value["genre"].as_str().unwrap_or_default().to_string(),
        // The snapshot carries no key/mode/bpm/spectrum: degrade, never invent.
        key: String::new(),
        mode: String::new(),
        bpm: 0.,
        spectrum: Vec::new(),
    }
}

/// gentle-stream's cache (`$XDG_CACHE_HOME` or `~/.cache`, then
/// `gentle-stream`), where `setup-samples` puts the soundfont.
pub fn cache_dir(cache: Option<&OsStr>, home: &Path) -> PathBuf {
    cache
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("gentle-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_running_snapshot_and_clears_unsupported_fields() {
        let out = r#"{"ok": true, "running": true, "playing": true, "volume": 80, "name": "Cafe Rain", "genre": "jazz", "mood": "rainy cafe"}"#;
        let state = parse_ctl(out).unwrap().unwrap();
        assert!(state.playing);
        assert_eq!(state.volume, 0.8);
        assert_eq!(state.name, "Cafe Rain");
        assert_eq!(state.genre, "jazz");
        assert_eq!(state.mood, "rainy cafe");
        // Not guaranteed by the snapshot; never invented.
        assert_eq!(state.key, "");
        assert_eq!(state.mode, "");
        assert_eq!(state.bpm, 0.);
        assert!(state.spectrum.is_empty());
        assert_eq!(state.details(), "jazz");
    }

    #[test]
    fn a_mood_snapshot_reports_the_queued_piece() {
        let out = r#"{"ok": true, "running": true, "playing": true, "volume": 55, "name": "Summer Storm", "genre": "jazz", "mood": "rainy night"}"#;
        let state = parse_ctl(out).unwrap().unwrap();
        assert_eq!(state.name, "Summer Storm");
        assert_eq!(state.mood, "rainy night");
        assert_eq!(state.volume, 0.55);
    }

    #[test]
    fn status_without_a_player_is_ok_none() {
        assert_eq!(parse_ctl(r#"{"ok": true, "running": false}"#), Ok(None));
    }

    #[test]
    fn stop_is_a_clean_stopped_state() {
        assert_eq!(parse_ctl(r#"{"ok": true, "running": false}"#), Ok(None));
    }

    #[test]
    fn structured_errors_are_reported_by_code() {
        assert_eq!(
            parse_ctl(
                r#"{"ok": false, "error": {"code": "nothing_playing", "message": "nothing is playing"}}"#
            ),
            Err(CtlError::NothingPlaying)
        );
        assert_eq!(
            parse_ctl(
                r#"{"ok": false, "error": {"code": "invalid_request", "message": "volume must be a whole number from 0 to 100"}}"#
            ),
            Err(CtlError::InvalidRequest(
                "volume must be a whole number from 0 to 100".into()
            ))
        );
        assert_eq!(
            parse_ctl(
                r#"{"ok": false, "error": {"code": "unavailable", "message": "the player is unavailable"}}"#
            ),
            Err(CtlError::Unavailable("the player is unavailable".into()))
        );
    }

    #[test]
    fn malformed_output_is_an_error() {
        assert_eq!(parse_ctl(""), Err(CtlError::Malformed));
        assert_eq!(
            parse_ctl("Traceback (most recent call last): …"),
            Err(CtlError::Malformed)
        );
        assert_eq!(parse_ctl(r#"{"no": "ok"}"#), Err(CtlError::Malformed));
    }

    #[test]
    fn cache_dir_follows_gentle_stream() {
        let home = Path::new("/home/u");
        assert_eq!(
            cache_dir(Some(OsStr::new("/c")), home),
            Path::new("/c/gentle-stream")
        );
        assert_eq!(
            cache_dir(Some(OsStr::new("")), home),
            Path::new("/home/u/.cache/gentle-stream")
        );
        assert_eq!(
            cache_dir(None, home),
            Path::new("/home/u/.cache/gentle-stream")
        );
    }
}
