//! Music: gentle-stream (the generative player behind Gentle Shell's
//! `/gentle:music`) in the title bar. A ♪ button appears once the player is
//! installed (Configuration → Music); it opens a small player with the piece
//! you hear, play/pause, next, volume and a mood field ("rainy night in a
//! small café") that recomposes the music at the next bar.
//!
//! Every control is one `gentle-stream ctl <action> --json` process
//! ([`client`]): `start` attaches to or starts the player, `status` polls it,
//! `volume` and `mood` steer it, `toggle`/`next`/`stop` control it. Starting
//! and reading a mood are gentle-stream's own job (its `start` may open
//! audio, its `mood` may invoke Gentle Shell's model), so the app never
//! re-implements startup or the model prompt.
//!
//! Cheap when idle: the state is polled every few seconds (to follow a player
//! started elsewhere); the popover polls a little faster while it is open.

mod client;
mod view;

pub use view::title_button;

use std::path::{Path, PathBuf};
use std::time::Duration;

use gentle_core::music::MusicState;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::*;

use crate::features::setup::music::{self as setup, MusicStatus};

/// Spectrum bars drawn in the player (the ctl snapshot carries none, so they
/// stay flat; kept so the popover's layout is unchanged).
const BANDS: usize = 40;
/// How often a window in the background checks whether it came back.
const INACTIVE_POLL: Duration = Duration::from_secs(2);
/// A status or control request answers in well under this.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(8);
/// Starting the player, or the model reading a mood, may take a while.
const SLOW_TIMEOUT: Duration = Duration::from_secs(120);

/// One ctl request to run off the main thread.
struct Request {
    action: &'static str,
    value: Option<String>,
    shell: Option<PathBuf>,
    timeout: Duration,
}

impl Request {
    fn slow(action: &'static str) -> Self {
        Self {
            action,
            value: None,
            shell: None,
            timeout: SLOW_TIMEOUT,
        }
    }
}

/// Run one ctl request against the resolved player. `Ok(None)` means nothing
/// is playing (or the player is gone), which callers take as cleanly stopped.
fn run_request(
    action: &str,
    value: Option<&str>,
    shell: Option<&Path>,
    timeout: Duration,
) -> Result<Option<MusicState>, String> {
    let Some(player) = setup::player() else {
        return Ok(None);
    };
    match client::run(&player, action, value, shell, timeout) {
        Ok(state) => Ok(state),
        Err(client::Error::NoPlayer) => Ok(None),
        Err(client::Error::Failed(error)) => Err(error),
    }
}

/// What the player is busy with.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Starting,
    Composing(String),
}

/// The title-bar button's look changed (installed, playing, paused).
pub struct MusicChanged;

pub struct Music {
    status: Option<MusicStatus>,
    /// `None`: nothing is playing.
    state: Option<MusicState>,
    /// The spectrum as drawn; the snapshot carries none, so it stays flat.
    shown: Vec<f32>,
    job: Option<Job>,
    /// The last mood's result or the last error: text and whether it went well.
    note: Option<(String, bool)>,
    /// The player is open: poll faster.
    watching: bool,
    mood: Entity<InputState>,
    volume: Entity<SliderState>,
    /// The user is dragging the volume: the player's value doesn't move it.
    sliding: bool,
    /// This app started the player, so quitting the app stops it.
    started_here: bool,
    _poll: Task<()>,
    _job: Task<()>,
    _status: Task<()>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<MusicChanged> for Music {}

impl Music {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mood = cx.new(|cx| InputState::new(window, cx).placeholder("Describe a mood…"));
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(80.)
        });
        let subscriptions = [
            cx.subscribe_in(&mood, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_mood(window, cx);
                }
            }),
            cx.subscribe(&volume, |this, _, event: &SliderEvent, cx| {
                let (value, done) = match event {
                    SliderEvent::Change(value) => (value.start(), false),
                    SliderEvent::Release(value) => (value.start(), true),
                };
                this.sliding = !done;
                let percent = value.round().clamp(0., 100.) as u32;
                this.send(
                    "volume",
                    Some(percent.to_string()),
                    None,
                    CONTROL_TIMEOUT,
                    cx,
                );
            }),
        ];
        let mut this = Self {
            status: None,
            state: None,
            shown: Vec::new(),
            job: None,
            note: None,
            watching: false,
            mood,
            volume,
            sliding: false,
            started_here: false,
            _poll: Task::ready(()),
            _job: Task::ready(()),
            _status: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this._poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                // Nobody looks at a window in the background: check back
                // without asking the player.
                let Ok((wait, poll)) = this.update_in(cx, |this, window, _| {
                    if window.is_window_active() {
                        this.next_poll()
                    } else {
                        (INACTIVE_POLL, None)
                    }
                }) else {
                    return;
                };
                cx.background_executor().timer(wait).await;
                let Some(action) = poll else { continue };
                let result = cx
                    .background_executor()
                    .spawn(async move { run_request(action, None, None, CONTROL_TIMEOUT) })
                    .await;
                if this
                    .update_in(cx, |this, window, cx| this.apply(result, window, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        this.refresh(cx);
        this
    }

    /// Look for the player again (after Configuration installs or removes it).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self._status = cx.spawn(async move |this, cx| {
            let status = cx
                .background_executor()
                .spawn(async { setup::status() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !status.usable() {
                    this.state = None;
                }
                this.status = Some(status);
                cx.emit(MusicChanged);
                cx.notify();
            });
        });
    }

    /// The button shows: gentle-stream is installed and runs here.
    pub fn available(&self) -> bool {
        self.status.as_ref().is_some_and(MusicStatus::usable)
    }

    pub fn playing(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.playing)
    }

    /// The player popover opened or closed.
    pub fn set_watching(&mut self, watching: bool, cx: &mut Context<Self>) {
        self.watching = watching;
        if !watching {
            self.shown.clear();
        }
        cx.notify();
    }

    fn next_poll(&self) -> (Duration, Option<&'static str>) {
        if !self.available() {
            return (Duration::from_secs(5), None);
        }
        let wait = if self.watching {
            Duration::from_secs(1)
        } else if self.state.is_some() {
            Duration::from_millis(1500)
        } else {
            // A player started elsewhere (`/gentle:music` in a terminal) shows up.
            Duration::from_secs(4)
        };
        (wait, Some("status"))
    }

    fn apply(
        &mut self,
        result: Result<Option<MusicState>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = (self.state.is_some(), self.playing());
        let mut changed = false;
        match result {
            Ok(Some(state)) => {
                let volume = (state.volume * 100.).round();
                let slider = self.volume.read(cx).value().start();
                if !self.sliding && (slider - volume).abs() >= 1. {
                    self.volume
                        .update(cx, |s, cx| s.set_value(volume, window, cx));
                }
                changed |= self.state.as_ref() != Some(&state);
                self.state = Some(state);
            }
            Ok(None) => {
                changed = self.state.is_some() || !self.shown.is_empty();
                self.state = None;
                self.shown.clear();
                // The player this app started is gone (nothing is still
                // starting it): one that shows up later isn't ours to stop.
                if self.job.is_none() {
                    self.started_here = false;
                }
            }
            Err(error) => {
                let note = Some((error, false));
                changed = self.note != note;
                self.note = note;
            }
        }
        if before != (self.state.is_some(), self.playing()) {
            cx.emit(MusicChanged);
        }
        // Polls mostly bring the same state: redraw only for news.
        if changed {
            cx.notify();
        }
    }

    /// A control (`toggle`, `next`, `stop`, `volume`), answered with the new
    /// state.
    fn send(
        &mut self,
        action: &'static str,
        value: Option<String>,
        shell: Option<PathBuf>,
        timeout: Duration,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { run_request(action, value.as_deref(), shell.as_deref(), timeout) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| this.apply(result, window, cx));
        })
        .detach();
    }

    fn send_status(&mut self, cx: &mut Context<Self>) {
        self.send("status", None, None, CONTROL_TIMEOUT, cx);
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.send("toggle", None, None, CONTROL_TIMEOUT, cx);
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        self.send("next", None, None, CONTROL_TIMEOUT, cx);
    }

    /// The app is quitting: stop the music it started, even before a poll
    /// saw it play. A player started elsewhere (`/gentle:music` in a
    /// terminal) keeps playing. Blocking, but one bounded request.
    pub fn shutdown(&self) {
        if self.started_here {
            let _ = run_request("stop", None, None, CONTROL_TIMEOUT);
        }
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.note = None;
        self.send("stop", None, None, CONTROL_TIMEOUT, cx);
    }

    /// Send what the mood field says, and clear it.
    fn submit_mood(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.mood.read(cx).value().trim().to_string();
        if text.is_empty() || self.job.is_some() {
            return;
        }
        self.mood.update(cx, |s, cx| s.set_value("", window, cx));
        self.set_mood(text, cx);
    }

    /// Start the player (the default lo-fi piece).
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.run_job(Job::Starting, vec![Request::slow("start")], false, cx);
    }

    /// Recompose to `mood` (starting the player when nothing plays).
    pub fn set_mood(&mut self, mood: String, cx: &mut Context<Self>) {
        let Ok(spec) = pi_rpc::resolve_backend(&std::env::temp_dir()) else {
            self.note = Some((
                "Install Gentle Shell first: it reads the mood.".into(),
                false,
            ));
            cx.notify();
            return;
        };
        let mut requests = Vec::new();
        // A mood applied to nothing starts the player first: `ctl mood` only
        // attaches to a running one, while `ctl start` is the one that may
        // spawn.
        if self.state.is_none() {
            requests.push(Request::slow("start"));
        }
        requests.push(Request {
            action: "mood",
            value: Some(mood.clone()),
            shell: Some(spec.program),
            timeout: SLOW_TIMEOUT,
        });
        self.run_job(Job::Composing(mood), requests, true, cx);
    }

    fn run_job(
        &mut self,
        job: Job,
        requests: Vec<Request>,
        note_on_success: bool,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some() {
            return;
        }
        // Starting (or a mood with nothing playing) makes the player ours.
        if self.state.is_none() {
            self.started_here = true;
        }
        self.job = Some(job);
        self.note = None;
        cx.notify();
        self._job = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut last: Result<Option<MusicState>, String> = Ok(None);
                    for request in requests {
                        last = run_request(
                            request.action,
                            request.value.as_deref(),
                            request.shell.as_deref(),
                            request.timeout,
                        );
                        if last.is_err() {
                            break;
                        }
                    }
                    last
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.job = None;
                this.note = match &result {
                    Ok(Some(state)) if note_on_success => {
                        Some((format!("Now playing {}", state.name), true))
                    }
                    Ok(_) => None,
                    Err(error) => Some((error.clone(), false)),
                };
                // Show the new piece now rather than at the next slow poll.
                this.send_status(cx);
                cx.notify();
            });
        });
    }
}
