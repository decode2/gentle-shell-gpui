//! What the backend sends a tab: RPC events, extension UI requests, stderr
//! and exit, and the working-tree refreshes a turn's tool calls schedule.

use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pi_rpc::desktop_rpc::{DesktopMethod, DesktopRecord, DesktopRpcError, decode_widget};
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
                self.permission.reset(self.desktop.session());
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
                    && let Some(canvas) =
                        tool_design_canvas(details, self.desktop.design_events_ready())
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
            self.handle_desktop_ui(&request, cx);
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
                match notify_target(&message, self.desktop.design_events_ready()) {
                    Some(NotifyTarget::Canvas(canvas)) => {
                        cx.emit(SessionEvent::DesignCanvas(canvas))
                    }
                    Some(NotifyTarget::Web(url)) => cx.emit(SessionEvent::AgentWeb(url)),
                    None => {}
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

    /// Route one Desktop widget record to the slice that owns it, then hand
    /// the original request to that slice so the shared session accepts it
    /// exactly once. Permission responses go to whoever's request is pending;
    /// permission events to the permission slice; everything else stays on the
    /// channel (probe, sequencing, resync, and every benign foreign record).
    fn handle_desktop_ui(&mut self, request: &UiRequest, cx: &mut Context<Self>) {
        let decoded = decode_widget(request);
        let pending = self.desktop.session().pending().map(|(_, method)| method);
        match route_desktop_widget(&decoded, pending) {
            DesktopRoute::Permission => {
                // Capture the intent before the slice consumes it, so a server
                // `unsupported_method` can still fall back to the legacy
                // save+restart path once.
                let requested_mode = self
                    .permission
                    .requested()
                    .map(|requested| super::from_desktop_mode(requested.mode));
                let applied = self.permission.apply(self.desktop.session(), request);
                let mut legacy_fallback = false;
                match applied.report.as_ref() {
                    // Events were lost: the channel owns the get_snapshot
                    // resync. The record was already accepted by the permission
                    // slice, so it is never accepted again here.
                    Some(desktop::permission::Report::OutOfOrderSequence) => {
                        let sends = self.desktop.resync();
                        self.send_desktop(sends, cx);
                    }
                    Some(desktop::permission::Report::UnsupportedMethod) => {
                        if let Some(mode) = requested_mode {
                            self.legacy_choose_mode_deferred(mode, cx);
                            legacy_fallback = true;
                        } else {
                            self.apply_permission(applied, cx);
                        }
                    }
                    _ => self.apply_permission(applied, cx),
                }
                if !legacy_fallback {
                    self.sync_supervision(cx);
                    self.sync_design(cx);
                    self.sync_config(cx);
                }
            }
            DesktopRoute::Supervision => {
                let applied = self.desktop.supervision_apply(request);
                match applied.report.as_ref() {
                    // Events were lost: the channel owns the get_snapshot
                    // resync, and the lists re-seed once it lands.
                    Some(desktop::supervision::Report::OutOfOrderSequence) => {
                        let sends = self.desktop.resync();
                        self.send_desktop(sends, cx);
                    }
                    Some(desktop::supervision::Report::UnsupportedMethod) => {
                        // The server refused a cancel it advertised: keep the
                        // legacy prompt path once, with the exact id.
                        if let Some(target) = self.desktop.take_cancel_target() {
                            self.legacy_cancel(target, cx);
                        } else {
                            self.apply_supervision(applied, cx);
                        }
                    }
                    _ => self.apply_supervision(applied, cx),
                }
                self.sync_supervision(cx);
                self.sync_design(cx);
                self.sync_config(cx);
            }
            DesktopRoute::Subscription => {
                let applied = self.desktop.subscription_apply(request);
                match applied.report.as_ref() {
                    // Events were lost: the channel owns the get_snapshot
                    // resync. The record was already accepted, so it is never
                    // accepted again.
                    Some(desktop::subscription::Report::OutOfOrderSequence) => {
                        let sends = self.desktop.resync();
                        self.send_desktop(sends, cx);
                    }
                    _ => self.apply_subscription(applied, cx),
                }
                self.sync_design(cx);
                self.sync_config(cx);
            }
            DesktopRoute::Design => {
                let applied = self.desktop.design_apply(request);
                match applied.report.as_ref() {
                    // Events were lost: the channel owns the get_snapshot
                    // resync, and the canvas re-seeds once it lands.
                    Some(desktop::design::Report::OutOfOrderSequence) => {
                        let sends = self.desktop.resync();
                        self.send_desktop(sends, cx);
                    }
                    _ => self.apply_design(applied, cx),
                }
                self.sync_supervision(cx);
                self.sync_design(cx);
                self.sync_config(cx);
            }
            DesktopRoute::Config => {
                let applied = self.desktop.config_apply(request);
                match applied.report.as_ref() {
                    // Events were lost: the channel owns the get_snapshot
                    // resync, and the Profiles screen re-reads every store
                    // behind it. The record was already accepted, so it is
                    // never accepted again.
                    Some(desktop::config::Report::OutOfOrderSequence) => {
                        let sends = self.desktop.resync();
                        self.send_desktop(sends, cx);
                        cx.notify();
                    }
                    _ => self.apply_config(applied, cx),
                }
                self.sync_supervision(cx);
                self.sync_design(cx);
                self.sync_config(cx);
            }
            DesktopRoute::Channel => {
                let applied = self.desktop.ui(request);
                self.apply_desktop(applied, cx);
                self.sync_permission_capabilities(cx);
                self.sync_supervision(cx);
                self.sync_design(cx);
                self.sync_config(cx);
            }
        }
    }

    /// Seed the design canvas once the probe and any in-flight request
    /// settle; it chains behind the other seeds the same way supervision does.
    fn sync_design(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.design_seed();
        self.apply_design(applied, cx);
    }

    /// Seed the subagent and job lists once the probe and any in-flight
    /// request settle. Called after every channel and supervision record, so
    /// the two lists chain behind the snapshot and permission seeds without
    /// blocking the initial probe.
    fn sync_supervision(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.supervision_seed();
        self.apply_supervision(applied, cx);
    }

    /// A server that advertised `cancel_subagent` answered `unsupported_method`:
    /// keep the legacy "ask the agent to run subagent_cancel" prompt exactly
    /// once. Jobs have no legacy prompt, so their failure is reported instead.
    fn legacy_cancel(
        &mut self,
        target: desktop::supervision::CancelTarget,
        cx: &mut Context<Self>,
    ) {
        if !target.is_subagent() {
            self.transcript.notice(
                NoticeLevel::Warning,
                desktop::supervision::Report::UnsupportedMethod.message(),
            );
            cx.notify();
            return;
        }
        let id = target.id().to_string();
        let agent = self
            .desktop
            .subagents()
            .iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.agent.clone())
            .or_else(|| {
                self.activity.agents.as_ref().and_then(|activity| {
                    activity
                        .tasks
                        .iter()
                        .find(|task| task.summary.id == id)
                        .map(|task| task.summary.agent.clone())
                })
            })
            .unwrap_or_default();
        let text = format!(
            "Stop the subagent task {id} ({agent}) now with subagent_cancel, then tell me what it had done."
        );
        self.send_prompt(text, cx);
    }

    /// Keep the permission slice's view of the peer's capabilities in step with
    /// the channel's probe, and seed the confirmed mode once those capabilities
    /// are known.
    fn sync_permission_capabilities(&mut self, cx: &mut Context<Self>) {
        if let Some(capabilities) = self.desktop.capabilities().cloned() {
            self.permission.set_capabilities(capabilities);
        }
        // The seed waits for any in-flight request, so the probe and the
        // permission slice never issue two requests at once.
        let seeded = self.permission.seed(self.desktop.session());
        self.apply_permission(seeded, cx);
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

/// Which slice owns one incoming Desktop record. A record is routed to the
/// permission slice only when it is a permission response or the advertised
/// permission event; supervision owns its responses and events; subscription
/// owns its responses and `rate_limit`; design owns `get_design_canvas` and
/// `design_canvas`; config owns `get_config`/`set_config` and
/// `config_changed`; everything else belongs to the channel
/// (probe, resync, and every benign foreign/late record). The shared session
/// then accepts the record exactly once, in the slice that owns it.
enum DesktopRoute {
    Permission,
    Supervision,
    Subscription,
    Design,
    Config,
    Channel,
}

/// Where a `notify` message's URL goes.
#[derive(Debug, PartialEq, Eq)]
enum NotifyTarget {
    Canvas(CanvasNotice),
    Web(String),
}

/// The legacy "Design canvas…" text drives the canvas only while the
/// structured `design_canvas` events are not its source; once they are, the
/// text would navigate a second time, so it is dropped (and never falls
/// through to the generic loopback opener). Any other loopback URL still
/// opens as a web app.
fn notify_target(message: &str, design_events_ready: bool) -> Option<NotifyTarget> {
    match crate::features::browser::canvas_notice(message) {
        Some(_) if design_events_ready => None,
        Some(canvas) => Some(NotifyTarget::Canvas(canvas)),
        None => web::loopback_url(message).map(NotifyTarget::Web),
    }
}

fn route_desktop_widget(
    decoded: &Result<Option<DesktopRecord>, DesktopRpcError>,
    pending: Option<DesktopMethod>,
) -> DesktopRoute {
    match decoded {
        Ok(Some(DesktopRecord::Response(_))) => {
            if matches!(
                pending,
                Some(DesktopMethod::GetPermissionMode) | Some(DesktopMethod::SetPermissionMode)
            ) {
                DesktopRoute::Permission
            } else if matches!(
                pending,
                Some(DesktopMethod::ListSubagents)
                    | Some(DesktopMethod::ListJobs)
                    | Some(DesktopMethod::CancelSubagent)
                    | Some(DesktopMethod::CancelJob)
            ) {
                DesktopRoute::Supervision
            } else if matches!(
                pending,
                Some(DesktopMethod::GetSubscriptionUsage)
                    | Some(DesktopMethod::RefreshSubscriptionUsage)
            ) {
                DesktopRoute::Subscription
            } else if pending == Some(DesktopMethod::GetDesignCanvas) {
                DesktopRoute::Design
            } else if matches!(
                pending,
                Some(DesktopMethod::GetConfig) | Some(DesktopMethod::SetConfig)
            ) {
                DesktopRoute::Config
            } else {
                DesktopRoute::Channel
            }
        }
        Ok(Some(DesktopRecord::Event(event)))
            if event.event == desktop::permission::PERMISSION_MODE_CHANGED =>
        {
            DesktopRoute::Permission
        }
        Ok(Some(DesktopRecord::Event(event)))
            if event.event == desktop::supervision::AGENTS_UPDATE
                || event.event == desktop::supervision::JOBS_UPDATE =>
        {
            DesktopRoute::Supervision
        }
        Ok(Some(DesktopRecord::Event(event)))
            if event.event == desktop::subscription::RATE_LIMIT =>
        {
            DesktopRoute::Subscription
        }
        Ok(Some(DesktopRecord::Event(event))) if event.event == desktop::design::DESIGN_CANVAS => {
            DesktopRoute::Design
        }
        Ok(Some(DesktopRecord::Event(event))) if event.event == desktop::config::CONFIG_CHANGED => {
            DesktopRoute::Config
        }
        _ => DesktopRoute::Channel,
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

/// A `gentle_design` result drives the canvas only while the structured
/// `design_canvas` events are not its source; once they are, it would
/// navigate a second time.
fn tool_design_canvas(details: &Value, design_events_ready: bool) -> Option<CanvasNotice> {
    if design_events_ready {
        return None;
    }
    design_canvas(details)
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

    use pi_rpc::desktop_rpc::{DesktopMethod, SCHEMA, WIDGET_KEY, decode_widget};
    use pi_rpc::{UiMethod, UiRequest};
    use serde_json::{Value, json};

    use super::{
        DesktopRoute, GIT_REFRESH_INTERVAL, NotifyTarget, design_canvas, git_refresh_wait,
        notice_level, notify_target, queued_count, route_desktop_widget, tool_design_canvas,
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
    fn design_results_yield_to_design_events() {
        let opened = json!({"url": "http://127.0.0.1:4100/tok/d/landing/"});
        let stopped = json!({"stopped": true});

        // An old Shell, or a channel not ready yet: the result drives the
        // canvas. Predicates, so a failure never prints the canvas URL.
        assert!(matches!(
            tool_design_canvas(&opened, false),
            Some(CanvasNotice::Open { url, refreshed: false })
                if url == "http://127.0.0.1:4100/tok/d/landing/"
        ));
        assert!(matches!(
            tool_design_canvas(&stopped, false),
            Some(CanvasNotice::Stopped)
        ));

        // Design events are the source: the result would navigate twice.
        assert!(tool_design_canvas(&opened, true).is_none());
        assert!(tool_design_canvas(&stopped, true).is_none());
    }

    #[test]
    fn legacy_canvas_notices_yield_to_design_events() {
        let url = "http://127.0.0.1:4100/tok/d/landing/";
        let opened = format!("Design canvas: {url}");

        // An old Shell, or a channel not ready yet: the legacy text navigates.
        // Predicates, so a failure never prints the canvas URL.
        assert!(matches!(
            notify_target(&opened, false),
            Some(NotifyTarget::Canvas(CanvasNotice::Open { url: shown, refreshed: false }))
                if shown == url
        ));
        assert!(matches!(
            notify_target("Design canvas stopped.", false),
            Some(NotifyTarget::Canvas(CanvasNotice::Stopped))
        ));

        // Design events are the source: the text neither navigates the canvas
        // nor falls through to the generic loopback opener.
        assert!(notify_target(&opened, true).is_none());
        assert!(notify_target("Design canvas stopped.", true).is_none());

        // Other loopback URLs keep opening as web apps either way.
        for ready in [false, true] {
            assert_eq!(
                notify_target("Dev server on http://localhost:5173/", ready),
                Some(NotifyTarget::Web("http://localhost:5173/".into()))
            );
            assert_eq!(notify_target("All done", ready), None);
        }
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

    /// A Desktop widget as Pi delivers a record.
    fn widget(lines: Option<Vec<String>>) -> UiRequest {
        UiRequest {
            id: "ui-1".into(),
            timeout: None,
            method: UiMethod::SetWidget {
                key: WIDGET_KEY.into(),
                lines,
                placement: None,
            },
        }
    }

    fn record(value: Value) -> Vec<String> {
        vec![value.to_string()]
    }

    #[test]
    fn desktop_records_route_to_the_slice_that_owns_them() {
        // Responses go to whoever's request is pending.
        let response = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": "desktop-1",
            "ok": true,
            "data": {"mode": "yolo", "default_mode": "yolo"},
        }))));
        let decoded = decode_widget(&response);
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::SetPermissionMode)),
            DesktopRoute::Permission
        ));
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::Capabilities)),
            DesktopRoute::Channel
        ));

        // The advertised permission event goes to the permission slice; every
        // other event stays on the channel for sequencing and resync.
        let permission_event = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": 1,
            "event": "permission_mode_changed",
            "data": {
                "mode": "yolo",
                "previous_mode": "normal",
                "default_mode": "yolo",
                "persisted": true,
                "source": "desktop",
            },
        }))));
        let decoded = decode_widget(&permission_event);
        assert!(matches!(
            route_desktop_widget(&decoded, None),
            DesktopRoute::Permission
        ));

        let state_event = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": 1,
            "event": "state_changed",
            "data": {},
        }))));
        let decoded = decode_widget(&state_event);
        assert!(matches!(
            route_desktop_widget(&decoded, None),
            DesktopRoute::Channel
        ));

        // Subscription responses and the advertised `rate_limit` event go to
        // the subscription slice.
        let usage_response = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": "desktop-1",
            "ok": true,
            "data": {"providers": []},
        }))));
        let decoded = decode_widget(&usage_response);
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::GetSubscriptionUsage)),
            DesktopRoute::Subscription
        ));
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::RefreshSubscriptionUsage)),
            DesktopRoute::Subscription
        ));

        let rate_limit_event = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": 1,
            "event": "rate_limit",
            "data": {"source": "response", "snapshots": []},
        }))));
        let decoded = decode_widget(&rate_limit_event);
        assert!(matches!(
            route_desktop_widget(&decoded, None),
            DesktopRoute::Subscription
        ));

        // The design canvas response and its advertised event go to the
        // design slice.
        let canvas_response = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": "desktop-1",
            "ok": true,
            "data": {"running": false, "slug": null, "connectedClients": 0, "designs": []},
        }))));
        let decoded = decode_widget(&canvas_response);
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::GetDesignCanvas)),
            DesktopRoute::Design
        ));
        let canvas_event = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "event",
            "seq": 1,
            "event": "design_canvas",
            "data": {"transition": "stop", "running": false, "slug": null, "connectedClients": 0, "designs": []},
        }))));
        let decoded = decode_widget(&canvas_event);
        assert!(matches!(
            route_desktop_widget(&decoded, None),
            DesktopRoute::Design
        ));

        // A cleared widget and a broken record are not this protocol's answer:
        // the channel owns them, and they stay benign.
        let cleared = decode_widget(&widget(None));
        assert!(matches!(
            route_desktop_widget(&cleared, None),
            DesktopRoute::Channel
        ));
        let broken = decode_widget(&widget(Some(vec!["not json".into()])));
        assert!(matches!(
            route_desktop_widget(&broken, None),
            DesktopRoute::Channel
        ));
    }

    #[test]
    fn config_records_route_to_the_config_slice_only() {
        // A config answer goes to the config slice only while its request is
        // the pending one; the same record answers nothing else.
        let answer = widget(Some(record(json!({
            "schema": SCHEMA,
            "kind": "response",
            "requestId": "desktop-3",
            "ok": false,
            "error": {"code": "revision_conflict", "message": "changed"},
        }))));
        let decoded = decode_widget(&answer);
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::GetConfig)),
            DesktopRoute::Config
        ));
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::SetConfig)),
            DesktopRoute::Config
        ));
        assert!(matches!(
            route_desktop_widget(&decoded, Some(DesktopMethod::GetSnapshot)),
            DesktopRoute::Channel
        ));
        assert!(matches!(
            route_desktop_widget(&decoded, None),
            DesktopRoute::Channel
        ));

        // Exactly `config_changed`; a look-alike name stays on the channel.
        let changed = |event: &str| {
            decode_widget(&widget(Some(record(json!({
                "schema": SCHEMA,
                "kind": "event",
                "seq": 1,
                "event": event,
                "data": {
                    "store": "profiles",
                    "scope": "global",
                    "writeTarget": "global",
                    "revision": "sha256:1",
                    "writeRevision": "sha256:2",
                    "source": "desktop",
                },
            })))))
        };
        assert!(matches!(
            route_desktop_widget(&changed("config_changed"), None),
            DesktopRoute::Config
        ));
        assert!(matches!(
            route_desktop_widget(&changed("config_changed_v2"), None),
            DesktopRoute::Channel
        ));
    }
}
