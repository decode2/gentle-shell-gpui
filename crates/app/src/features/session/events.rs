//! What the backend sends a tab: RPC events, extension UI requests, stderr
//! and exit, and the working-tree refreshes a turn's tool calls schedule.

use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pi_rpc::{Incoming, UiAnswer, UiMethod, UiRequest};
use serde_json::Value;

use super::state::desktop;
use super::state::{BackendStatus, Dialog, TurnActivity};
use super::{SessionEvent, SessionView, view};
use crate::features::browser::CanvasNotice;
use crate::features::git;
use gentle_core::tools;
use gentle_core::transcript::{self, NoticeLevel};
use gentle_core::web;

/// Least time between working-tree refreshes a turn's tool calls trigger.
const GIT_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
/// Stderr lines kept for an exited backend.
const STDERR_LINES: usize = 40;

impl SessionView {
    /// Apply a batch of backend messages in order, then sync the view and
    /// repaint once.
    pub(super) fn handle_batch(
        &mut self,
        batch: Vec<Incoming>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let follow = self.list.at_bottom();
        for incoming in batch {
            self.handle(incoming, window, cx);
            if self.backend.restart_pending
                && !self.transcript.running
                && self.extension.dialogs.is_empty()
            {
                // The rest of the batch belongs to the backend being replaced.
                self.restart_backend(window, cx);
                break;
            }
        }
        self.sync_view(cx);
        if follow {
            self.list.scroll_to_end();
        }
        self.sync_chrome(cx);
        cx.notify();
    }

    fn handle(&mut self, incoming: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        match incoming {
            Incoming::Event(event) => self.handle_event(&event, window, cx),
            Incoming::Ui(request) => self.handle_ui(request, window, cx),
            Incoming::Stderr(line) => {
                if self.backend.stderr.len() == STDERR_LINES {
                    self.backend.stderr.pop_front();
                }
                self.backend.stderr.push_back(line);
            }
            Incoming::Exited(code) => {
                self.backend.status = BackendStatus::Exited(code);
                self.transcript.interrupt();
                self.extension.dialogs.clear();
                // Nothing the dead backend said or numbered applies any more.
                self.desktop.reset();
            }
        }
    }

    fn handle_event(&mut self, event: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let answering = self.transcript.streaming_item();
        let effects = self.transcript.apply(event);
        if let Some(ix) = answering
            && self.transcript.streaming_item() != Some(ix)
        {
            self.answer_finished(ix, cx);
        }
        match event["type"].as_str() {
            Some("queue_update") => self.activity.queued = queued_count(event),
            Some("compaction_start") => self.usage.compacting = true,
            Some("compaction_end") => {
                self.usage.compacting = false;
                self.refresh_stats(cx);
            }
            Some("session_info_changed") => {
                self.session_name = event["name"].as_str().map(str::to_string);
            }
            Some("thinking_level_changed") => {
                self.models.thinking = event["level"].as_str().unwrap_or("off").to_string();
            }
            Some("bash_execution_update") => {
                if let (Some(index), Some(delta)) = (self.active_shell, event["delta"].as_str()) {
                    self.transcript.shell_output(index, delta);
                }
            }
            Some("tool_execution_start") => {
                self.activity.tool_started(event);
                // An agent booting or driving a device from its shell shows up
                // in the window's instance tray. Launching from the card
                // itself does not pass through here.
                if matches!(event["toolName"].as_str(), Some("bash" | "bash_background"))
                    && let Some(command) = event["args"]["command"].as_str()
                {
                    if let Some(intent) = gentle_core::devices::launch_intent(command) {
                        cx.emit(SessionEvent::Device(intent));
                    }
                    if let Some(command) = gentle_core::devices::device_command(command) {
                        cx.emit(SessionEvent::DeviceCommand(command));
                    }
                }
            }
            Some("tool_execution_end") => {
                let details = &event["result"]["details"];
                if event["toolName"] == "todo"
                    && let Some(tasks) = tools::todo_tasks(details)
                {
                    self.todo.apply(tasks);
                }
                self.activity.tool_ended(event);
                if event["toolName"] == "gentle_design"
                    && let Some(canvas) = design_canvas(details)
                {
                    cx.emit(SessionEvent::DesignCanvas(canvas));
                }
                if matches!(
                    event["toolName"].as_str(),
                    Some("bash" | "bash_background" | "monitor")
                ) && let Some(url) = web::loopback_url(&tool_output(event))
                {
                    cx.emit(SessionEvent::AgentWeb(url));
                }
            }
            Some("message_end") if event["message"]["role"] == "custom" => {
                self.activity.custom_message_ended(event);
            }
            _ => {}
        }
        if effects.settled {
            self.activity.phase = None;
            self.activity.queued = 0;
            self.refresh_stats(cx);
            // A stopped turn needs no alert: the user just stopped it.
            if event["aborted"] != true {
                let active = window.is_window_active();
                crate::features::alerts::fire(
                    gentle_core::notifications::Alert::TurnEnded,
                    active,
                    cx,
                );
            }
        }
        if effects.settled {
            // Always once at turn end, whatever the tool calls scheduled.
            self.refresh_git(cx);
        } else if effects.files_touched {
            self.refresh_git_soon(cx);
        }
    }

    fn handle_ui(&mut self, request: UiRequest, window: &mut Window, cx: &mut Context<Self>) {
        // Gentle Desktop's records travel as a widget but are control traffic:
        // the adapter correlates them, so they never land in widget storage.
        // Every other widget keeps the handling below unchanged.
        if desktop::is_desktop_widget(&request) {
            let applied = self.desktop.ui(&request);
            self.apply_desktop(applied, cx);
            return;
        }
        if request.method.is_dialog() {
            if let Some(timeout) = request.timeout {
                let id = request.id.clone();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(timeout))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.extension.dialogs.retain(|d| d.id != id);
                        cx.notify();
                    });
                })
                .detach();
            }
            let first = self.extension.dialogs.is_empty();
            crate::features::alerts::fire(
                gentle_core::notifications::Alert::NeedsYou,
                window.is_window_active(),
                cx,
            );
            self.extension.dialogs.push_back(Dialog {
                id: request.id,
                method: request.method,
            });
            if first {
                self.prepare_dialog(window, cx);
            }
            return;
        }
        match request.method {
            UiMethod::Notify {
                message,
                notify_type,
            } => {
                let level = notice_level(notify_type.as_deref());
                match crate::features::browser::canvas_notice(&message) {
                    Some(canvas) => cx.emit(SessionEvent::DesignCanvas(canvas)),
                    None => {
                        if let Some(url) = web::loopback_url(&message) {
                            cx.emit(SessionEvent::AgentWeb(url));
                        }
                    }
                }
                let error = level == NoticeLevel::Error;
                // Usage warnings repeat every turn: keep them as a composer chip.
                if !error && let Some((percent, window_key)) = view::parse_rate_limit(&message) {
                    self.extension.rate_limits.insert(
                        window_key,
                        view::RateLimit {
                            percent,
                            text: message,
                        },
                    );
                } else {
                    // An extension command's output also lands in the
                    // conversation, as one result card.
                    if let Some(capture) = &mut self.capture {
                        capture.offer(level, &message, Instant::now());
                    }
                    // Info goes to the notification center only; warnings and
                    // errors also toast.
                    if level != NoticeLevel::Info && (error || self.extension.first_toast(&message))
                    {
                        let note = if error {
                            Notification::error(message.clone())
                        } else {
                            Notification::warning(message.clone())
                        };
                        window.push_notification(note, cx);
                    }
                    cx.emit(SessionEvent::Notice { level, message });
                }
            }
            UiMethod::SetStatus { key, text } => match text.filter(|t| !t.trim().is_empty()) {
                Some(text) => {
                    if key == "gentle:mode" {
                        self.mode = gentle_core::gentle_state::PermissionMode::from_status(&text);
                    }
                    self.extension.statuses.insert(key, text);
                }
                None => {
                    self.extension.statuses.remove(&key);
                }
            },
            UiMethod::SetWidget { key, lines, .. } => {
                let lines = lines.filter(|l| !l.is_empty());
                if key == "gentle-agents" {
                    let next = lines.and_then(|l| gentle_core::activity::parse(&l));
                    self.activity.agents = Some(gentle_core::activity::retain_finished(
                        self.activity.agents.take(),
                        next,
                    ));
                } else if let Some(lines) = lines {
                    self.extension.widgets.insert(key, lines);
                } else {
                    self.extension.widgets.remove(&key);
                }
            }
            UiMethod::SetTitle { title } => {
                self.title = Some(title).filter(|t| !t.is_empty());
            }
            UiMethod::SetEditorText { text } => {
                self.composer
                    .update(cx, |state, cx| state.set_value(text, window, cx));
            }
            _ => {}
        }
    }

    /// Point keyboard focus at the front dialog: its text field, or the
    /// session itself so typing does not leak into the composer.
    fn prepare_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, placeholder) = match self.extension.dialogs.front().map(|d| &d.method) {
            Some(UiMethod::Input { placeholder, .. }) => (None, placeholder.clone()),
            Some(UiMethod::Editor { prefill, .. }) => (prefill.clone(), None),
            Some(_) => {
                window.focus(&self.focus_handle, cx);
                return;
            }
            None => {
                self.focus_composer(window, cx);
                return;
            }
        };
        self.dialog_input.update(cx, |state, cx| {
            state.set_value(text.unwrap_or_default(), window, cx);
            state.set_placeholder(placeholder.unwrap_or_default(), window, cx);
            state.focus(window, cx);
        });
    }

    /// Answer the front dialog and show the next one.
    pub(super) fn answer_dialog(
        &mut self,
        answer: UiAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.extension.dialogs.pop_front() else {
            return;
        };
        if let Some(process) = self.process()
            && let Err(error) = process.respond_ui(&dialog.id, answer)
        {
            self.transcript
                .notice(NoticeLevel::Error, format!("{error:#}"));
        }
        self.prepare_dialog(window, cx);
        cx.notify();
    }

    pub(super) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        self.git.seq += 1;
        self.git.at = Some(Instant::now());
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { git::snapshot(&cwd) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.git.snapshot = snapshot;
                cx.notify();
            });
        })
        .detach();
    }

    /// [`Self::refresh_git`] at most once per [`GIT_REFRESH_INTERVAL`]: a
    /// call inside it defers one refresh to its end.
    fn refresh_git_soon(&mut self, cx: &mut Context<Self>) {
        if self.git.scheduled {
            return;
        }
        let wait = git_refresh_wait(self.git.at.map(|at| at.elapsed()));
        if wait.is_zero() {
            self.refresh_git(cx);
            return;
        }
        self.git.scheduled = true;
        let seq = self.git.seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.git.scheduled = false;
                // A refresh since (the turn ended) already covered it.
                if this.git.seq == seq {
                    this.refresh_git(cx);
                }
            });
        })
        .detach();
    }
}

impl TurnActivity {
    /// `tool_execution_start`: the phase the call implies, and the label of
    /// a background command for the job it will report.
    fn tool_started(&mut self, event: &Value) {
        let tool = event["toolName"].as_str().unwrap_or_default();
        if let Some(phase) = gentle_core::gentle_state::infer_phase(tool, &event["args"]) {
            self.phase = Some(phase);
        }
        if matches!(tool, "bash_background" | "monitor") {
            let label = event["args"]["label"]
                .as_str()
                .or(event["args"]["command"].as_str())
                .unwrap_or_default();
            self.job_commands.insert(
                transcript::str_field(event, "toolCallId"),
                label.to_string(),
            );
        }
    }

    /// `tool_execution_end`: a job the call reported, labeled by the command
    /// that started it.
    fn tool_ended(&mut self, event: &Value) {
        if let Some(job) = gentle_core::gentle_state::job_from_details(&event["result"]["details"])
        {
            let label = self
                .job_commands
                .remove(&transcript::str_field(event, "toolCallId"));
            gentle_core::gentle_state::upsert_job(&mut self.jobs, job, label.as_deref());
        }
    }

    /// `message_end` of a custom message: a job notice updates its job.
    fn custom_message_ended(&mut self, event: &Value) {
        if let Some(job) = gentle_core::gentle_state::job_from_details(&event["message"]["details"])
        {
            gentle_core::gentle_state::upsert_job(&mut self.jobs, job, None);
        }
    }
}

/// Messages waiting behind the turn (`queue_update`).
fn queued_count(event: &Value) -> usize {
    let count = |key: &str| event[key].as_array().map_or(0, Vec::len);
    count("steering") + count("followUp")
}

/// The text a shell tool's result carries: the command's output.
fn tool_output(event: &Value) -> String {
    let mut out = String::new();
    match &event["result"]["content"] {
        serde_json::Value::Array(parts) => {
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        serde_json::Value::String(text) => out.push_str(text),
        _ => {}
    }
    if let Some(output) = event["result"]["output"].as_str() {
        out.push_str(output);
    }
    out
}

/// What a `gentle_design` result says about its canvas.
fn design_canvas(details: &Value) -> Option<CanvasNotice> {
    if details["stopped"] == true {
        Some(CanvasNotice::Stopped)
    } else {
        details["url"].as_str().map(|url| CanvasNotice::Open {
            url: url.to_string(),
            refreshed: details["refreshed"] == true,
        })
    }
}

/// An extension `notify` type as a notice level; unknown types are info.
fn notice_level(notify_type: Option<&str>) -> NoticeLevel {
    match notify_type {
        Some("error") => NoticeLevel::Error,
        Some("warning") => NoticeLevel::Warning,
        _ => NoticeLevel::Info,
    }
}

/// How long a working-tree refresh must wait, given the time since the last one.
fn git_refresh_wait(since_last: Option<Duration>) -> Duration {
    since_last.map_or(Duration::ZERO, |since| {
        GIT_REFRESH_INTERVAL.saturating_sub(since)
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        GIT_REFRESH_INTERVAL, design_canvas, git_refresh_wait, notice_level, queued_count,
        tool_output,
    };
    use crate::features::browser::CanvasNotice;
    use crate::features::session::state::TurnActivity;
    use gentle_core::transcript::NoticeLevel;

    #[test]
    fn git_refreshes_are_spaced_out() {
        assert_eq!(git_refresh_wait(None), Duration::ZERO);
        assert_eq!(
            git_refresh_wait(Some(Duration::from_millis(500))),
            Duration::from_millis(1000)
        );
        assert_eq!(git_refresh_wait(Some(GIT_REFRESH_INTERVAL)), Duration::ZERO);
        assert_eq!(
            git_refresh_wait(Some(Duration::from_secs(9))),
            Duration::ZERO
        );
    }

    #[test]
    fn queue_counts_steering_and_follow_ups() {
        let event = json!({"type": "queue_update", "steering": ["a"], "followUp": ["b", "c"]});
        assert_eq!(queued_count(&event), 3);
        assert_eq!(queued_count(&json!({"type": "queue_update"})), 0);
    }

    #[test]
    fn shell_results_expose_their_text_for_url_detection() {
        let event = json!({
            "type": "tool_execution_end",
            "result": {"content": [{"type": "text", "text": "Local: http://localhost:5173/"}]}
        });
        assert!(tool_output(&event).contains("http://localhost:5173/"));
        assert_eq!(tool_output(&json!({"result": {}})), "");
    }

    #[test]
    fn design_results_open_refresh_or_stop_the_canvas() {
        assert_eq!(
            design_canvas(&json!({"url": "http://localhost:4000"})),
            Some(CanvasNotice::Open {
                url: "http://localhost:4000".into(),
                refreshed: false,
            })
        );
        assert_eq!(
            design_canvas(&json!({"url": "http://x", "refreshed": true})),
            Some(CanvasNotice::Open {
                url: "http://x".into(),
                refreshed: true,
            })
        );
        // Stopping wins over a stale URL.
        assert_eq!(
            design_canvas(&json!({"url": "http://x", "stopped": true})),
            Some(CanvasNotice::Stopped)
        );
        assert_eq!(design_canvas(&json!({})), None);
    }

    #[test]
    fn notify_types_map_to_levels() {
        assert_eq!(notice_level(Some("error")), NoticeLevel::Error);
        assert_eq!(notice_level(Some("warning")), NoticeLevel::Warning);
        assert_eq!(notice_level(Some("info")), NoticeLevel::Info);
        assert_eq!(notice_level(None), NoticeLevel::Info);
    }

    #[test]
    fn background_jobs_take_the_label_of_their_command() {
        let mut activity = TurnActivity::default();
        activity.tool_started(&json!({
            "toolName": "bash_background",
            "toolCallId": "call-1",
            "args": {"command": "npm run dev"},
        }));
        assert_eq!(activity.job_commands["call-1"], "npm run dev");

        activity.tool_ended(&json!({
            "toolName": "bash_background",
            "toolCallId": "call-1",
            "result": {"details": {"gentleJobs": {"jobId": 7, "status": "running"}}},
        }));
        assert!(activity.job_commands.is_empty());
        assert_eq!(activity.jobs.len(), 1);
        assert_eq!(activity.jobs[0].id, "7");
        assert_eq!(activity.jobs[0].label, "npm run dev");

        // A later notice updates the job and keeps its label.
        activity.custom_message_ended(&json!({
            "message": {"role": "custom", "details": {"gentleJobs": {"jobId": 7, "status": "exited", "exitCode": 0}}},
        }));
        assert_eq!(activity.jobs.len(), 1);
        assert_eq!(activity.jobs[0].status, "exited");
        assert_eq!(activity.jobs[0].label, "npm run dev");
    }

    #[test]
    fn tool_calls_set_the_phase_only_when_they_imply_one() {
        let mut activity = TurnActivity::default();
        activity.tool_started(&json!({"toolName": "read", "args": {"path": "a.rs"}}));
        assert_eq!(activity.phase, Some("exploring"));
        activity.tool_started(&json!({"toolName": "unknown_tool", "args": {}}));
        assert_eq!(activity.phase, Some("exploring"));
        assert!(activity.job_commands.is_empty());
    }
}
