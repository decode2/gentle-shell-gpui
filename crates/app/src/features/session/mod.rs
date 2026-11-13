//! One tab: a `gentle-shell --mode rpc` backend and its conversation UI.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use pi_rpc::{UiAnswer, UiMethod};

use crate::shared::ui;
use gentle_core::transcript::{NoticeLevel, Transcript};

mod agents;
mod attachments;
mod builtins;
mod events;
mod lifecycle;
mod rpc;
mod state;
mod submit;
mod view;

pub use agents::AgentsPanel;

actions!(session, [Abort, Stop, ToggleAgents, NewSession]);

pub const CONTEXT: &str = "GentleSession";

/// Requests a tab makes of its window.
pub enum SessionEvent {
    OpenProviders,
    /// An extension's `notify`, for the window's notification center.
    /// Warnings and errors are also toasted by the tab itself.
    Notice {
        level: NoticeLevel,
        message: String,
    },
    /// Something the window chrome shows may have changed: [`SessionView::title`],
    /// [`SessionView::is_running`], [`SessionView::session_file`],
    /// [`SessionView::cwd`] or [`SessionView::model_choices`]. Streaming
    /// output does not emit it, so the window need not redraw per token.
    ChromeChanged,
    /// A Gentle Shell command whose UI is terminal-only was run (typed or
    /// from the palette): open the app's own screen for it instead.
    OpenScreen(gentle_core::commands::Screen),
    /// The thinking level was set to `max`: the window plays its surge.
    MaxEffort,
    /// Gentle Design's canvas opened, refreshed or stopped: the window keeps
    /// it in its instance tray (or reloads the card already showing it).
    DesignCanvas(crate::features::browser::CanvasNotice),
    /// A shell tool is starting an emulator or simulator: the window follows
    /// that device in its instance tray, without opening a card.
    Device(gentle_core::devices::LaunchIntent),
    /// A shell tool drove a device (adb, simctl, Maestro…): the tray shows it
    /// as what the agent is doing with that device.
    DeviceCommand(gentle_core::devices::DeviceCommand),
    /// The user asked for a page (`/web`): the window shows it in a card.
    OpenWeb(String),
    /// A tool printed a local web app's URL: the window keeps it in its
    /// instance tray until the user looks.
    AgentWeb(String),
}

/// What the window reads from a tab, to emit [`SessionEvent::ChromeChanged`]
/// only when it differs.
#[derive(PartialEq)]
struct Chrome {
    title: String,
    running: bool,
    session_file: Option<PathBuf>,
    models: u64,
}

pub struct SessionView {
    cwd: PathBuf,
    backend: state::Backend,
    /// Gentle Desktop's RPC channel on this backend (see [`state::desktop`]).
    desktop: state::desktop::Desktop,
    /// Gentle Desktop's permission-mode state on this backend (see
    /// [`state::desktop::permission`]); it borrows `desktop`'s session.
    permission: state::desktop::permission::Permission,
    transcript: Transcript,
    /// Rendered Markdown per (item, block), with the byte length already pushed.
    markdown: HashMap<(usize, usize), (Entity<TextViewState>, usize)>,
    expanded: HashSet<usize>,
    composer: Entity<TextareaState>,
    /// The composer has the keyboard: its border takes the accent.
    composer_focused: bool,
    /// When the composer last took the keyboard: its pulse plays for
    /// `PULSE` from then (see `composer_shell`), a frame at a time.
    composer_pulse: Option<std::time::Instant>,
    /// Pixel kisses floating up from the caret as you type: when each was
    /// blown, where, and its number (for its own drift and size).
    kisses: Vec<(std::time::Instant, Point<Pixels>, usize)>,
    kisses_blown: usize,
    /// The composer's text length last time, to tell typing from deleting.
    composer_len: usize,
    /// Images waiting to go with the next prompt; `[Image #N]` is the N-th.
    attachments: Vec<attachments::Attachment>,
    /// Decoded conversation images, by data hash.
    image_cache: attachments::ImageCache,
    extension: state::ExtensionUi,
    dialog_input: Entity<TextareaState>,
    activity: state::TurnActivity,
    agents_panel: AgentsPanel,
    /// This session's live permission mode (Gentle Shell's `gentle:mode`
    /// status), and the default new sessions start in (`permission-mode.json`).
    mode: Option<gentle_core::gentle_state::PermissionMode>,
    default_mode: Option<gentle_core::gentle_state::PermissionMode>,
    _agents_ticker: Task<()>,
    todo: state::TodoIsland,
    git: state::GitStatus,
    models: state::Models,
    usage: state::Usage,
    commands: Vec<(String, String)>,
    session_name: Option<String>,
    session_file: Option<PathBuf>,
    title: Option<String>,
    active_shell: Option<usize>,
    banner: crate::shared::banner::BannerConfig,
    banner_reveal: f32,
    _banner_anim: Task<()>,
    /// The conversation's virtualized list and its per-item display caches.
    list: view::TranscriptList,
    /// What the window was last told about: see [`SessionEvent::ChromeChanged`].
    chrome: Option<Chrome>,
    /// The pane's own width, measured each frame; narrow layouts adapt to it.
    pane_width: std::rc::Rc<std::cell::Cell<f32>>,
    focus_handle: FocusHandle,
    _io: Task<()>,
    _subscriptions: Vec<Subscription>,
    /// View-only state: live timers, the prompt in flight, picker highlights.
    live: view::Live,
    model_search: Entity<InputState>,
    /// The `notify` output of the extension command last sent.
    capture: Option<builtins::Capture>,
    /// Numbers [`builtins::Capture`]s, to match Pi's replies.
    capture_seq: u64,
    /// This tab's window, for work that finishes outside a window update.
    window_handle: AnyWindowHandle,
}

impl EventEmitter<SessionEvent> for SessionView {}

impl SessionView {
    /// `start_now: false` defers the backend until [`Self::ensure_started`]
    /// (tabs that are not shown yet); `prompt` and a resumed session still
    /// run once it starts.
    pub fn new(
        cwd: PathBuf,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        start_now: bool,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder("What are we cooking?")
        });
        let dialog_input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 8));
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models…"));
        // GPUI keeps decoded images until told to drop them.
        cx.on_release(|this, cx| {
            this.image_cache.release_all(cx);
            for attachment in this.attachments.drain(..) {
                attachment.image.remove_asset(cx);
            }
        })
        .detach();
        let weak = cx.entity().downgrade();
        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                    InputEvent::Change => {
                        this.blow_kiss(cx);
                        cx.notify();
                    }
                    InputEvent::Focus | InputEvent::Blur => {
                        this.composer_focused = matches!(event, InputEvent::Focus);
                        if this.composer_focused {
                            this.start_composer_pulse(cx);
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &dialog_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } = event
                    {
                        let submits = match this.extension.dialogs.front().map(|d| &d.method) {
                            Some(UiMethod::Input { .. }) => true,
                            Some(UiMethod::Editor { .. }) => *secondary,
                            _ => false,
                        };
                        if submits {
                            let value = this.dialog_input.read(cx).value().to_string();
                            this.answer_dialog(UiAnswer::Value(value), window, cx);
                        }
                    }
                },
            ),
            cx.subscribe(&model_search, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.model_query_changed(cx);
                }
            }),
            // The model picker's and the `/` menu's ↑/↓/Enter must beat the
            // focused search field and composer.
            cx.intercept_keystrokes(move |event, window, cx| {
                let consumed = weak
                    .update(cx, |this, cx| {
                        this.model_picker_key(&event.keystroke, window, cx)
                            || this.slash_menu_key(&event.keystroke, window, cx)
                    })
                    .unwrap_or(false);
                if consumed {
                    cx.stop_propagation();
                }
            }),
        ];
        let mut this = Self {
            cwd,
            backend: state::Backend::new(prompt),
            desktop: state::desktop::Desktop::default(),
            permission: state::desktop::permission::Permission::default(),
            transcript: Transcript::default(),
            markdown: HashMap::new(),
            expanded: HashSet::new(),
            composer,
            composer_focused: false,
            composer_pulse: None,
            kisses: Vec::new(),
            kisses_blown: 0,
            composer_len: 0,
            attachments: Vec::new(),
            image_cache: attachments::ImageCache::default(),
            extension: state::ExtensionUi::default(),
            dialog_input,
            activity: state::TurnActivity::default(),
            agents_panel: AgentsPanel::default(),
            mode: None,
            default_mode: crate::shared::gentle_state::read_default_mode(
                &pi_rpc::resolve_homes().config,
            ),
            _agents_ticker: Task::ready(()),
            todo: state::TodoIsland::default(),
            git: state::GitStatus::default(),
            models: state::Models::default(),
            usage: state::Usage::default(),
            pane_width: std::rc::Rc::new(std::cell::Cell::new(10_000.)),
            commands: Vec::new(),
            session_name: None,
            session_file: None,
            title: None,
            active_shell: None,
            banner: crate::shared::banner::read_config(&pi_rpc::resolve_homes().config),
            banner_reveal: 0.0,
            _banner_anim: Task::ready(()),
            list: view::TranscriptList::new(),
            chrome: None,
            focus_handle: cx.focus_handle(),
            _io: Task::ready(()),
            _subscriptions: subscriptions,
            live: view::Live::default(),
            model_search,
            capture: None,
            capture_seq: 0,
            window_handle: window.window_handle(),
        };
        if start_now {
            this.ensure_started(window, cx);
        }
        this
    }

    pub fn title(&self) -> String {
        self.session_name
            .clone()
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| ui::split_path(&self.cwd).1)
    }

    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn is_running(&self) -> bool {
        self.transcript.running
    }

    pub fn composer(&self) -> Entity<TextareaState> {
        self.composer.clone()
    }

    /// Show an informational line in the conversation.
    pub fn note(&mut self, text: String, cx: &mut Context<Self>) {
        self.transcript.notice(NoticeLevel::Info, text);
        cx.notify();
    }

    /// Commands for the `/` menu and the palette: Pi's built-ins the app
    /// runs, then Gentle Shell's, without the ones that do nothing here.
    pub fn commands(&self) -> Vec<(String, String)> {
        builtins::BUILTINS
            .iter()
            .map(|(name, description)| (name.to_string(), description.to_string()))
            .chain(
                self.commands
                    .iter()
                    .filter(|(name, _)| !gentle_core::commands::hidden(name))
                    .cloned(),
            )
            .collect()
    }

    /// The conversation this tab shows: a resume still in flight (or waiting
    /// for the backend to start) wins over the live session file.
    pub fn session_file(&self) -> Option<PathBuf> {
        self.backend
            .pending_session
            .clone()
            .or_else(|| self.session_file.clone())
    }

    pub fn model_choices(&self) -> Vec<(String, String, String, bool)> {
        self.models
            .available
            .iter()
            .map(|m| {
                (
                    m.provider.clone(),
                    m.id.clone(),
                    m.name.clone(),
                    m.reasoning,
                )
            })
            .collect()
    }

    pub fn open_agents_panel(&mut self, cx: &mut Context<Self>) {
        self.open_agents(None, cx);
    }

    /// Tell the window when something it shows changed.
    fn sync_chrome(&mut self, cx: &mut Context<Self>) {
        let chrome = Chrome {
            title: self.title(),
            running: self.is_running(),
            session_file: self.session_file(),
            models: self.models.rev,
        };
        if self.chrome.as_ref() != Some(&chrome) {
            self.chrome = Some(chrome);
            cx.emit(SessionEvent::ChromeChanged);
        }
    }

    fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer
            .update(cx, |state, cx| state.focus(window, cx));
    }

    /// A warning toast, as extension warnings show.
    fn warn(&self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(Notification::warning(message), cx);
    }

    /// Put `text` on the clipboard and say so.
    fn copy_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.toast(Notification::success("Copied to the clipboard."), cx);
    }

    /// Run `f` with this tab's window, after the current update.
    fn in_window(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| f(this, window, cx));
            });
        })
        .detach();
    }

    /// A toast in this tab's window, from code without one at hand.
    fn toast(&self, note: Notification, cx: &mut Context<Self>) {
        self.in_window(cx, move |_, window, cx| window.push_notification(note, cx));
    }

    /// Whether this tab's window is frontmost and focused (false inside an
    /// update of that window: use `Window::is_window_active` there).
    fn window_active(&self, cx: &mut App) -> bool {
        cx.update_window(self.window_handle, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    }

    fn on_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session(window, cx);
    }

    fn toggle_agents(&mut self, _: &ToggleAgents, _: &mut Window, cx: &mut Context<Self>) {
        if self.agents_panel.open {
            self.close_agents(cx);
        } else {
            self.open_agents(None, cx);
        }
    }

    /// Esc: close what's open (the Agents panel, then a dialog). It never
    /// stops the turn: a few Esc presses to close things used to abort the
    /// agent once nothing was left open. Stopping is ⌘. or the Stop button.
    fn abort(&mut self, _: &Abort, window: &mut Window, cx: &mut Context<Self>) {
        if self.agents_panel.open {
            self.close_agents(cx);
            return;
        }
        if !self.extension.dialogs.is_empty() {
            self.answer_dialog(UiAnswer::Cancelled, window, cx);
            return;
        }
        cx.propagate();
    }

    /// ⌘. (Ctrl+. elsewhere): stop the running turn.
    fn on_stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            self.stop(cx);
        } else {
            cx.propagate();
        }
    }
}

/// The wire permission mode for the picker's mode.
pub(in crate::features::session) fn to_desktop_mode(
    mode: gentle_core::gentle_state::PermissionMode,
) -> pi_rpc::desktop_rpc::PermissionMode {
    match mode {
        gentle_core::gentle_state::PermissionMode::Normal => {
            pi_rpc::desktop_rpc::PermissionMode::Normal
        }
        gentle_core::gentle_state::PermissionMode::Auto => {
            pi_rpc::desktop_rpc::PermissionMode::Auto
        }
        gentle_core::gentle_state::PermissionMode::Yolo => {
            pi_rpc::desktop_rpc::PermissionMode::Yolo
        }
        gentle_core::gentle_state::PermissionMode::SuperYolo => {
            pi_rpc::desktop_rpc::PermissionMode::SuperYolo
        }
    }
}

/// The picker's mode for a server-reported wire mode.
pub(in crate::features::session) fn from_desktop_mode(
    mode: pi_rpc::desktop_rpc::PermissionMode,
) -> gentle_core::gentle_state::PermissionMode {
    match mode {
        pi_rpc::desktop_rpc::PermissionMode::Normal => {
            gentle_core::gentle_state::PermissionMode::Normal
        }
        pi_rpc::desktop_rpc::PermissionMode::Auto => {
            gentle_core::gentle_state::PermissionMode::Auto
        }
        pi_rpc::desktop_rpc::PermissionMode::Yolo => {
            gentle_core::gentle_state::PermissionMode::Yolo
        }
        pi_rpc::desktop_rpc::PermissionMode::SuperYolo => {
            gentle_core::gentle_state::PermissionMode::SuperYolo
        }
    }
}

#[cfg(test)]
mod tests {
    use gentle_core::gentle_state::PermissionMode;

    use super::{from_desktop_mode, to_desktop_mode};

    #[test]
    fn desktop_permission_modes_round_trip() {
        for mode in PermissionMode::ALL {
            assert_eq!(from_desktop_mode(to_desktop_mode(mode)), mode);
        }
    }
}
