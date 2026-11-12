//! Requests a tab makes of its backend: the request/response plumbing, the
//! state it reads back, and the commands the UI and window run.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pi_rpc::PiProcess;
use pi_rpc::desktop_rpc::DesktopMethod;
use serde_json::{Value, json};

use super::state::desktop::permission::Applied as PermissionApplied;
use super::state::desktop::subscription::Applied as SubscriptionApplied;
use super::state::desktop::supervision::Applied as SupervisionApplied;
use super::state::desktop::{self, Applied as DesktopApplied, Send as DesktopSend};
use super::state::{BackendStatus, ModelInfo};
use super::{SessionView, attachments, builtins};
use gentle_core::transcript::NoticeLevel;

/// How long a Desktop request may stay unanswered before the tab drops it.
/// The extension answers from the same session, so this only fires when
/// something is genuinely wrong.
const DESKTOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether Pi's `get_commands` answer lists the Gentle Desktop extension. A
/// failed request lists nothing, so its prompt is never sent.
fn discovers_desktop(result: &Result<Value>) -> bool {
    result
        .as_ref()
        .is_ok_and(|data| desktop::registers_desktop(&parse_commands(data)))
}

/// Pi's `get_commands` data as `(name, description)`, skipping anything that is
/// not a named command.
fn parse_commands(data: &Value) -> Vec<(String, String)> {
    data["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|command| {
            Some((
                command["name"].as_str()?.to_string(),
                command["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect()
}

impl SessionView {
    pub(super) fn process(&self) -> Option<Arc<PiProcess>> {
        match &self.backend.status {
            BackendStatus::Ready(process) => Some(process.clone()),
            _ => None,
        }
    }

    /// Send `command`; `then` runs on the UI thread with the response data.
    pub(super) fn request(
        &self,
        command: Value,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<Value>, &mut Context<Self>) + 'static,
    ) {
        let Some(process) = self.process() else {
            return;
        };
        // The task holds the response, not the process: closing the tab still
        // stops the backend with a quiet request (`!` command, compaction) in
        // flight.
        let response = process.request(command);
        cx.spawn(async move |this, cx| {
            let result = response.await.map(|r| r.data);
            let _ = this.update(cx, |this, cx| {
                then(this, result, cx);
                this.sync_view(cx);
                this.sync_chrome(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The response data, or `None` with the error shown in the conversation.
    pub(super) fn report(&mut self, result: Result<Value>) -> Option<Value> {
        match result {
            Ok(data) => Some(data),
            Err(error) => {
                self.transcript
                    .notice(NoticeLevel::Error, format!("{error:#}"));
                None
            }
        }
    }

    /// Act on a Desktop decision: say what failed through the tab's report
    /// path, then send whatever the channel asked for.
    pub(super) fn apply_desktop(&mut self, applied: DesktopApplied, cx: &mut Context<Self>) {
        if let Some(report) = applied.report {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
    }

    /// Act on a supervision decision: say what failed through the tab's report
    /// path, then send whatever the subagent/job channel asked for.
    pub(super) fn apply_supervision(
        &mut self,
        applied: SupervisionApplied,
        cx: &mut Context<Self>,
    ) {
        if let Some(report) = applied.report {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
    }

    /// Act on a subscription decision: say what failed through the tab's report
    /// path (legacy signals are the Usage screen's concern, not a session
    /// error), then send whatever the usage slice asked for.
    pub(super) fn apply_subscription(
        &mut self,
        applied: SubscriptionApplied,
        cx: &mut Context<Self>,
    ) {
        if let Some(report) = &applied.report
            && !report.is_legacy()
        {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
    }

    /// The Usage screen's snapshot of this session's subscription usage. Pure
    /// read: the screen decides how to render, never this session.
    pub(crate) fn subscription_snapshot(&self) -> super::state::desktop::subscription::Snapshot {
        self.desktop.subscription_snapshot()
    }

    /// Ask the server for the whole-account subscription usage once (the Usage
    /// screen opened). Idempotent: a later call is a no-op.
    pub(crate) fn subscription_seed(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.subscription_seed();
        self.send_desktop(applied.sends, cx);
    }

    /// Refresh the whole-account subscription usage (the Usage screen's
    /// refresh button).
    pub(crate) fn subscription_refresh(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.subscription_refresh();
        self.send_desktop(applied.sends, cx);
    }

    /// Reset the Gentle Desktop channel when the conversation changes on the
    /// same backend (new/open session). It forgets the old session's pending
    /// request and event baseline (without rewinding request ids) and returns
    /// the channel to Idle, so the `refresh_state` that follows re-probes
    /// capabilities and the supervision lists re-seed exactly once behind it.
    /// The permission slice keeps its confirmed mode; its capabilities are
    /// refreshed by the same re-probe. The subscription slice is reset with
    /// the channel.
    fn reset_desktop_channel(&mut self) {
        self.desktop.reset();
    }

    /// Cancel one running background job over `cancel_job`. There is no job
    /// start method and no restart: only the running job the user picked is
    /// affected, and the server's answer is the truth.
    pub(super) fn cancel_running_job(&mut self, id: String, cx: &mut Context<Self>) {
        let applied = self.desktop.cancel_job(id);
        self.apply_supervision(applied, cx);
    }

    /// Act on a permission decision: say what failed or warn about a default
    /// that could not be saved, then send whatever the permission slice asked
    /// for and reflect the server-confirmed mode into the picker.
    pub(super) fn apply_permission(&mut self, applied: PermissionApplied, cx: &mut Context<Self>) {
        if let Some(report) = applied.report {
            self.transcript
                .notice(NoticeLevel::Warning, report.message());
            cx.notify();
        }
        self.send_desktop(applied.sends, cx);
        // A gap seen while this request was in flight is due now that it
        // settled or rolled back: ask the channel for a fresh snapshot once.
        let resync = self.desktop.flush_resync();
        self.send_desktop(resync, cx);
        self.sync_permission_ui(cx);
    }

    /// The server's confirmed permission state is truth: move the picker's
    /// live and default modes only from it, never optimistically from the
    /// request the user just picked.
    fn sync_permission_ui(&mut self, cx: &mut Context<Self>) {
        let confirmed = self.permission.confirmed();
        let mut changed = false;
        if let Some(mode) = confirmed.mode.map(super::from_desktop_mode)
            && self.mode != Some(mode)
        {
            self.mode = Some(mode);
            changed = true;
        }
        if let Some(default_mode) = confirmed.default_mode.map(super::from_desktop_mode)
            && self.default_mode != Some(default_mode)
        {
            self.default_mode = Some(default_mode);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// Hand Gentle Desktop requests to the backend. Each one is a `prompt` the
    /// transport answers with a disposition, and the domain answer arrives
    /// later as the Desktop widget; a guarded timer drops one that never comes.
    pub(super) fn send_desktop(&mut self, sends: Vec<DesktopSend>, cx: &mut Context<Self>) {
        for send in sends {
            self.send_one_desktop(send, cx);
        }
    }

    fn send_one_desktop(&mut self, send: DesktopSend, cx: &mut Context<Self>) {
        let generation = send.generation;
        let method = send.request.method();
        if self.process().is_none() {
            // No backend to answer: drop it like a timeout, silently, in the
            // slice that owns the request.
            self.expire_desktop(generation, method, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DESKTOP_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                // Only its own request: a newer one has another generation.
                this.expire_desktop(generation, method, cx);
            });
        })
        .detach();
        self.request(
            send.request.prompt_command(),
            cx,
            move |this, result, cx| {
                // `handled` only means the prompt was accepted; nothing but that
                // keeps the request waiting for its widget record.
                let handled = result.is_ok_and(|data| data["disposition"] == "handled");
                this.outer_desktop(generation, method, handled, cx);
            },
        );
    }

    /// A pending timeout fired: hand it to the slice that issued the request,
    /// so a probe timer never abandons a permission request and vice versa.
    fn expire_desktop(&mut self, generation: u64, method: DesktopMethod, cx: &mut Context<Self>) {
        match method {
            DesktopMethod::Capabilities | DesktopMethod::GetSnapshot => {
                let sends = self.desktop.expire(generation);
                self.send_desktop(sends, cx);
            }
            DesktopMethod::GetPermissionMode | DesktopMethod::SetPermissionMode => {
                let applied = self.permission.expire(self.desktop.session(), generation);
                self.apply_permission(applied, cx);
            }
            DesktopMethod::ListSubagents
            | DesktopMethod::ListJobs
            | DesktopMethod::CancelSubagent
            | DesktopMethod::CancelJob => {
                let applied = self.desktop.supervision_expire(generation);
                self.apply_supervision(applied, cx);
            }
            DesktopMethod::GetSubscriptionUsage | DesktopMethod::RefreshSubscriptionUsage => {
                let applied = self.desktop.subscription_expire(generation);
                self.apply_subscription(applied, cx);
            }
        }
    }

    /// Pi answered the outer prompt: hand the disposition to the slice that
    /// issued the request.
    fn outer_desktop(
        &mut self,
        generation: u64,
        method: DesktopMethod,
        handled: bool,
        cx: &mut Context<Self>,
    ) {
        match method {
            DesktopMethod::Capabilities | DesktopMethod::GetSnapshot => {
                let applied = self.desktop.outer(generation, handled);
                self.apply_desktop(applied, cx);
            }
            DesktopMethod::GetPermissionMode | DesktopMethod::SetPermissionMode => {
                let applied = self
                    .permission
                    .outer(self.desktop.session(), generation, handled);
                self.apply_permission(applied, cx);
            }
            DesktopMethod::ListSubagents
            | DesktopMethod::ListJobs
            | DesktopMethod::CancelSubagent
            | DesktopMethod::CancelJob => {
                let applied = self.desktop.supervision_outer(generation, handled);
                self.apply_supervision(applied, cx);
            }
            DesktopMethod::GetSubscriptionUsage | DesktopMethod::RefreshSubscriptionUsage => {
                let applied = self.desktop.subscription_outer(generation, handled);
                self.apply_subscription(applied, cx);
            }
        }
    }

    /// Pi's command list: what the `/` menu offers, and the discovery that
    /// decides whether Gentle Desktop's extension is there at all. The prompt
    /// carrying a Desktop record must never be sent on a guess, because a Shell
    /// without the command would run that JSON as a conversation prompt.
    fn refresh_commands(&mut self, cx: &mut Context<Self>) {
        self.request(json!({"type": "get_commands"}), cx, |this, result, cx| {
            // Read before the result is consumed: a failed listing leaves the
            // channel closed, and `report` puts that error in the conversation.
            let registered = discovers_desktop(&result);
            if let Some(data) = this.report(result) {
                this.commands = parse_commands(&data);
                let applied = this.desktop.ready(registered);
                this.apply_desktop(applied, cx);
            }
        });
    }

    pub(super) fn refresh_state(&mut self, cx: &mut Context<Self>) {
        self.request(json!({"type": "get_state"}), cx, |this, result, _| {
            if let Some(state) = this.report(result) {
                this.models.current = ModelInfo::parse(&state["model"]);
                this.models.thinking = state["thinkingLevel"].as_str().unwrap_or("off").to_string();
                this.session_name = state["sessionName"].as_str().map(str::to_string);
                this.session_file = state["sessionFile"].as_str().map(PathBuf::from);
                this.usage.auto_compaction = state["autoCompactionEnabled"].as_bool();
            }
        });
        self.request(
            json!({"type": "get_available_models"}),
            cx,
            |this, result, _| {
                this.models.loaded = true;
                this.models.rev += 1;
                if let Some(data) = this.report(result) {
                    this.models.available = data["models"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(ModelInfo::parse)
                        .collect();
                }
            },
        );
        self.refresh_thinking_levels(cx);
        self.refresh_commands(cx);
        self.request(json!({"type": "get_messages"}), cx, |this, result, _| {
            if let Some(data) = this.report(result) {
                let messages = data["messages"].as_array().cloned().unwrap_or_default();
                if !messages.is_empty() {
                    this.transcript.load_messages(&messages);
                    this.markdown.clear();
                    this.sync_todo_from_history();
                }
            }
        });
        self.refresh_stats(cx);
    }

    pub(super) fn refresh_stats(&mut self, cx: &mut Context<Self>) {
        self.request(
            json!({"type": "get_session_stats"}),
            cx,
            |this, result, cx| {
                if let Ok(stats) = result {
                    this.usage.cost = stats["cost"].as_f64().unwrap_or(0.0);
                    this.usage.context_percent = stats["contextUsage"]["percent"].as_f64();
                    let threshold = crate::features::alerts::context_threshold(cx);
                    if this
                        .usage
                        .context_latch
                        .observe(this.usage.context_percent, threshold)
                    {
                        let active = this.window_active(cx);
                        crate::features::alerts::fire(
                            gentle_core::notifications::Alert::ContextHigh,
                            active,
                            cx,
                        );
                    }
                }
            },
        );
    }

    /// The levels the current model accepts; until they arrive (or if the
    /// backend can't say) the slider keeps the last list.
    fn refresh_thinking_levels(&mut self, cx: &mut Context<Self>) {
        self.request(
            json!({"type": "get_available_thinking_levels"}),
            cx,
            |this, result, _| {
                if let Ok(data) = result {
                    let levels: Vec<String> = data["levels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.as_str().map(str::to_string))
                        .collect();
                    if !levels.is_empty() {
                        this.models.thinking_levels = levels;
                    }
                }
            },
        );
    }

    /// The task list as the conversation's last `todo` call left it.
    fn sync_todo_from_history(&mut self) {
        for item in self.transcript.items.iter().rev() {
            if let gentle_core::transcript::Item::Tool { name, details, .. } = item
                && name == "todo"
                && let Some(tasks) = gentle_core::tools::todo_tasks(details)
            {
                self.todo.apply(tasks);
                return;
            }
        }
    }

    pub(super) fn set_model(&mut self, model: ModelInfo, cx: &mut Context<Self>) {
        let command = json!({"type": "set_model", "provider": model.provider, "modelId": model.id});
        self.request(command, cx, move |this, result, cx| {
            if this.report(result).is_some() {
                this.models.current = Some(model);
                this.refresh_stats(cx);
                this.refresh_thinking_levels(cx);
                this.request(json!({"type": "get_state"}), cx, |this, result, _| {
                    if let Ok(state) = result {
                        this.models.thinking =
                            state["thinkingLevel"].as_str().unwrap_or("off").to_string();
                    }
                });
            }
        });
    }

    pub(super) fn stop(&mut self, cx: &mut Context<Self>) {
        // Pi's Esc: drop queued messages first so abort does not resume them.
        self.request(json!({"type": "clear_queue"}), cx, |_, _, _| {});
        self.request(json!({"type": "abort"}), cx, |this, result, _| {
            this.report(result);
        });
    }

    /// RPC `new_session`: a fresh conversation on the same backend. A fresh
    /// session also re-reads defaults such as the permission mode.
    pub fn new_session(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.request(json!({"type": "new_session"}), cx, |this, result, cx| {
            if let Some(data) = this.report(result)
                && data["cancelled"] != true
            {
                this.reset_conversation();
                this.reset_desktop_channel();
                this.refresh_state(cx);
                this.refresh_git(cx);
            }
        });
    }

    /// RPC `switch_session` to a saved conversation.
    pub fn open_session(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let command = json!({"type": "switch_session", "sessionPath": path});
        self.request(command, cx, move |this, result, cx| {
            let switched = this
                .report(result)
                .is_some_and(|data| data["cancelled"] != true);
            if this.backend.pending_session.as_ref() == Some(&path) {
                this.backend.pending_session = None;
            }
            if switched {
                this.reset_conversation();
                this.session_file = Some(path);
                this.reset_desktop_channel();
                this.refresh_state(cx);
            }
        });
    }

    /// Send any RPC command for a window-level screen; failures surface in the transcript.
    pub fn send_command(&mut self, command: Value, cx: &mut Context<Self>) {
        self.request(command, cx, |this, result, cx| {
            this.report(result);
            this.refresh_state(cx);
        });
    }

    /// RPC `set_session_name`; the tab title follows.
    pub fn rename(&mut self, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let command = json!({"type": "set_session_name", "name": name});
        self.request(command, cx, move |this, result, _| {
            if this.report(result).is_some() {
                // `sync_chrome` (after this) tells the window.
                this.session_name = Some(name);
            }
        });
    }

    /// `/compact [instructions]`: RPC `compact`. Pi reports progress and the
    /// result (or failure) as compaction events, shown in the conversation.
    pub(super) fn compact(&mut self, instructions: Option<String>, cx: &mut Context<Self>) {
        if self.usage.compacting || self.process().is_none() {
            return;
        }
        self.usage.compacting = true;
        let mut command = json!({"type": "compact"});
        if let Some(instructions) = instructions {
            command["customInstructions"] = instructions.into();
        }
        self.request(command, cx, |this, _, cx| {
            // Success and failure both arrive as `compaction_end`.
            this.usage.compacting = false;
            this.refresh_stats(cx);
        });
    }

    pub(super) fn set_auto_compaction(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let command = json!({"type": "set_auto_compaction", "enabled": enabled});
        self.request(command, cx, move |this, result, _| {
            if this.report(result).is_some() {
                this.usage.auto_compaction = Some(enabled);
            }
        });
    }

    /// `/copy`: RPC `get_last_assistant_text` to the clipboard.
    pub(super) fn copy_last_answer(&mut self, cx: &mut Context<Self>) {
        self.request(
            json!({"type": "get_last_assistant_text"}),
            cx,
            |this, result, cx| {
                let Some(data) = this.report(result) else {
                    return;
                };
                match data["text"].as_str().filter(|t| !t.trim().is_empty()) {
                    Some(text) => this.copy_text(text.to_string(), cx),
                    None => {
                        this.toast(Notification::warning("There is no answer to copy yet."), cx)
                    }
                }
            },
        );
    }

    /// `/export [path]`: RPC `export_html` to `path`, or where the save
    /// dialog says (`~/Downloads/<session>.html` suggested), then reveal it.
    pub(super) fn export_html(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        if let Some(path) = path {
            self.export_to(expand_home(&path), cx);
            return;
        }
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| self.cwd.clone());
        let name = format!("{}.html", export_file_stem(&self.title()));
        let chosen = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.export_to(path, cx));
        })
        .detach();
    }

    fn export_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let command = json!({"type": "export_html", "outputPath": path});
        self.request(command, cx, move |this, result, cx| {
            if let Some(data) = this.report(result) {
                let written = data["path"].as_str().map_or(path, PathBuf::from);
                cx.reveal_path(&written);
                this.note(
                    format!("Exported the conversation to {}", written.display()),
                    cx,
                );
            }
        });
    }

    pub(super) fn send_prompt(&mut self, text: String, cx: &mut Context<Self>) {
        self.send_prompt_with(text, Vec::new(), cx);
    }

    /// RPC `prompt`, or `steer` mid-turn, with `images`.
    pub(super) fn send_prompt_with(
        &mut self,
        text: String,
        images: Vec<attachments::Attachment>,
        cx: &mut Context<Self>,
    ) {
        let steer = self.transcript.running;
        let command = attachments::prompt_command(&text, &images, steer);
        for image in &images {
            self.image_cache.insert(image);
        }
        if !steer {
            let shown = images.iter().map(|a| a.image.clone()).collect();
            self.live
                .expect_reply(&text, shown, self.transcript.items.len());
        }
        // An extension command may answer with notices: gather them.
        let capture = text.trim_start().starts_with('/').then(|| {
            if let Some(open) = self.capture.as_ref().map(|c| c.id) {
                self.finish_capture(open);
            }
            self.capture_seq += 1;
            self.capture = Some(builtins::Capture::new(self.capture_seq, &text));
            self.capture_seq
        });
        self.request(command, cx, move |this, result, cx| {
            let reply = this.report(result);
            if reply.is_some() {
                this.live.reply_acknowledged();
            } else {
                this.live.reply_failed();
            }
            if let Some(id) = capture {
                let handled = reply.is_some_and(|data| data["disposition"] == "handled");
                this.command_replied(id, handled, cx);
            }
        });
    }

    /// Pi answered extension command `id`: a handled command's output is
    /// final [`builtins::OUTPUT_GRACE`] later; anything else started a turn
    /// (a prompt template or skill) and shows as one.
    fn command_replied(&mut self, id: u64, handled: bool, cx: &mut Context<Self>) {
        let Some(capture) = self.capture.as_mut().filter(|c| c.id == id) else {
            return;
        };
        if !handled {
            self.capture = None;
            return;
        }
        capture.replied(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(builtins::OUTPUT_GRACE).await;
            let _ = this.update(cx, |this, cx| {
                let follow = this.list.at_bottom();
                if this.finish_capture(id) {
                    this.sync_view(cx);
                    if follow {
                        this.list.scroll_to_end();
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Close capture `id`, adding its output to the conversation; whether
    /// anything was added.
    fn finish_capture(&mut self, id: u64) -> bool {
        let Some(capture) = self.capture.take_if(|c| c.id == id) else {
            return false;
        };
        if capture.notices.is_empty() {
            return false;
        }
        self.transcript
            .command_output(capture.command, capture.notices);
        true
    }

    /// Run a Gentle Shell extension command (`/gentle:…`) over RPC without
    /// adding a turn: Pi runs extension commands at once, even mid-stream.
    /// `done` gets `Ok` only when Pi reports the command as `handled`, so a
    /// missing or renamed command surfaces instead of being sent as a prompt.
    pub fn run_extension_command(
        &mut self,
        text: String,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<(), String>, &mut App) + 'static,
    ) {
        if self.process().is_none() {
            done(
                Err("Gentle Shell is still starting in this tab; try again in a moment.".into()),
                cx,
            );
            return;
        }
        let command = json!({"type": "prompt", "message": text});
        self.request(command, cx, move |_, result, cx| {
            let outcome = match result {
                Ok(data) if data["disposition"] == "handled" => Ok(()),
                Ok(data) => Err(format!(
                    "Gentle Shell did not run {text} (disposition {}). Is gentle-pi up to date?",
                    data["disposition"]
                )),
                Err(error) => Err(format!("{error:#}")),
            };
            done(outcome, cx);
        });
    }
}

/// `~/x` → under the home folder; other paths as given.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// A session title as a file name: path separators and other characters
/// file systems reject become `-`.
fn export_file_stem(title: &str) -> String {
    let stem: String = title
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let stem = stem.trim_matches(['.', ' ']);
    if stem.is_empty() {
        "session".into()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use serde_json::{Value, json};

    use super::{discovers_desktop, export_file_stem, parse_commands};

    #[test]
    fn only_a_listed_desktop_command_opens_the_channel() {
        let listed = Ok(json!({"commands": [
            {"name": "gentle:mode", "description": "Permissions"},
            {"name": "gentle:desktop", "description": "Desktop records"},
        ]}));
        assert!(discovers_desktop(&listed));

        // Another Gentle Shell command is not the extension.
        let other = Ok(json!({"commands": [{"name": "gentle:mode"}]}));
        assert!(!discovers_desktop(&other));

        // A failed listing, or data without commands, opens nothing: its prompt
        // would reach Pi as raw JSON.
        let failed: anyhow::Result<Value> = Err(anyhow!("get_commands failed"));
        assert!(!discovers_desktop(&failed));
        assert!(!discovers_desktop(&Ok(json!({"commands": "nope"}))));
    }

    #[test]
    fn command_lists_keep_their_names_and_descriptions() {
        let commands = parse_commands(&json!({"commands": [
            {"name": "a", "description": "d"},
            {"description": "no name"},
            "junk",
        ]}));
        assert_eq!(commands, vec![("a".to_string(), "d".to_string())]);
        assert!(parse_commands(&json!({})).is_empty());
    }

    #[test]
    fn export_names_are_safe_file_names() {
        assert_eq!(export_file_stem("Fix the API"), "Fix the API");
        assert_eq!(export_file_stem("a/b: c?"), "a-b- c-");
        assert_eq!(export_file_stem("  ..  "), "session");
        assert_eq!(export_file_stem(".hidden"), "hidden");
    }
}
