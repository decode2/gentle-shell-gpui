//! The activity line over the composer: turn timer, phase, current tool and
//! the session's status chips (jobs, widgets, usage, helpers, rate limits).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use pi_rpc::desktop_rpc::JobExitCode;

use super::{NARROW, TINY, clock};
use crate::features::session::SessionView;
use crate::shared::ui;
use gentle_core::tools;
use gentle_core::transcript::{Item, ToolStatus, format_tokens};

impl SessionView {
    /// Elapsed time, phase and current tool over the composer, with the
    /// session's live chips on the right.
    pub(super) fn render_activity(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy();
        let waiting = self.live.pending.as_ref().filter(|p| !p.started);
        let since = self
            .live
            .turn
            .or_else(|| self.live.pending.as_ref().map(|p| p.since));
        // Only shown while busy; with no tool running there is nothing to find.
        let scan = busy && self.transcript.running_tools() > 0;
        let current_tool = self
            .transcript
            .items
            .get(if scan { 0 } else { self.transcript.items.len() }..)
            .unwrap_or_default()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::Tool {
                    name,
                    args,
                    details,
                    status: ToolStatus::Running,
                    ..
                } => {
                    let s = tools::summarize(name, args, details);
                    Some(if s.subject.is_empty() {
                        s.title
                    } else {
                        format!(
                            "{} {}",
                            s.title,
                            gentle_core::text::first_line(&s.subject, 60)
                        )
                    })
                }
                _ => None,
            });
        let provider = self
            .models
            .current
            .as_ref()
            .map_or_else(|| "the agent".to_string(), |m| m.provider.clone());

        let left = h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .when(busy, |this| {
                this.child(ui::pulse(div().size(px(6.)).rounded_full().bg(ui::petal())))
                    .when_some(since, |this, since| {
                        this.child(
                            div()
                                .font_family(ui::MONO)
                                .text_color(ui::muted())
                                .child(clock(since.elapsed())),
                        )
                    })
                    .child(div().h(px(10.)).w(px(1.)).bg(ui::border_strong()))
                    .map(|this| match (waiting, self.activity.phase) {
                        (Some(_), _) => this.child(
                            div()
                                .text_color(ui::petal())
                                .child(format!("✿ waiting for {provider}…")),
                        ),
                        (None, Some(phase)) => {
                            this.child(div().text_color(ui::petal()).child(format!("✿ {phase}…")))
                        }
                        (None, None) => {
                            this.child(div().text_color(ui::petal()).child("✿ working…"))
                        }
                    })
                    .when_some(current_tool, |this, tool| {
                        this.child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(ui::muted())
                                .child(tool),
                        )
                    })
            });

        let width = self.pane_width.get();
        let statuses: Vec<(String, String)> = self
            .extension
            .statuses
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "gentle:mode" | "gentle:yolo" | "gentle-jobs"))
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect();
        // Tiny panes fold every extension status (and limits) into one chip.
        let collapse = width < TINY;
        let folded: Vec<String> = if collapse {
            statuses
                .iter()
                .map(|(_, s)| s.clone())
                .chain(self.extension.rate_limits.values().map(|l| l.text.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let right = h_flex()
            .flex_shrink_0()
            .gap_2()
            .when(!collapse, |this| {
                this.children(statuses.into_iter().map(|(key, s)| {
                    if width < NARROW {
                        // Icon-only, the text lives in the tooltip.
                        div()
                            .id(ElementId::Name(format!("status-{key}").into()))
                            .child(Icon::new(IconName::Info).xsmall().text_color(ui::faint()))
                            .tooltip(move |window, cx| Tooltip::new(s.clone()).build(window, cx))
                            .into_any_element()
                    } else {
                        div()
                            .max_w(px(220.))
                            .truncate()
                            .text_color(ui::faint())
                            .child(s)
                            .into_any_element()
                    }
                }))
            })
            .when(!folded.is_empty(), |this| {
                let tip = folded.join("\n");
                this.child(
                    div()
                        .id("status-overflow")
                        .child(ui::chip(format!("+{}", folded.len()), ui::muted()))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
                )
            })
            .when(self.jobs_visible(), |this| {
                this.child(self.render_jobs_chip(cx))
            })
            .when(self.activity.queued > 0, |this| {
                this.child(ui::chip(
                    format!("{} queued", self.activity.queued),
                    ui::warning(),
                ))
            })
            // Tern-style: jobs, widgets and last-response usage live in chips
            // here instead of a side panel. The task list floats at the top of
            // the conversation instead (see `render_todo_island`).
            .when(!collapse, |this| this.children(self.render_widget_chips()))
            .when(width >= NARROW, |this| {
                this.children(self.render_usage_chip())
            })
            .when(self.helpers_visible(), |this| {
                this.child(self.render_helpers_chip(cx))
            })
            .children(self.extension.rate_limits.iter().filter(|_| !collapse).map(
                |(window, limit)| {
                    let color = if limit.percent >= 95 {
                        ui::danger()
                    } else if limit.percent >= 80 {
                        ui::warning()
                    } else {
                        ui::muted()
                    };
                    let tip = limit.text.clone();
                    div()
                        .id(ElementId::Name(format!("limit-{window}").into()))
                        .child(ui::chip(
                            format!("{} {}%", limit_label(window), limit.percent),
                            color,
                        ))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                },
            ))
            .child(self.render_mode_picker(cx));

        h_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .h(px(24.))
            .px_1()
            .gap_3()
            .text_size(px(11.))
            .child(left)
            .child(right)
    }

    // ── Status chips (Tern-style: everything that used to sit in a side
    //    panel lives in small chips over the composer, each opening a card) ──

    /// Whether a job's status word means it is still running.
    fn jobs_visible(&self) -> bool {
        if self.desktop.jobs_ready() {
            !self.desktop.jobs().is_empty()
        } else {
            !self.activity.jobs.is_empty()
        }
    }

    /// Whether the helpers chip has anything to show (structured or legacy).
    fn helpers_visible(&self) -> bool {
        if self.desktop.subagents_ready() {
            !self.desktop.subagents().is_empty()
        } else {
            self.activity
                .agents
                .as_ref()
                .is_some_and(|a| !a.tasks.is_empty())
        }
    }

    /// Background jobs: "⧗ 2 running" or "Jobs 3", with the list on click and
    /// a cancel action for running jobs once the peer offers `cancel_job`.
    fn render_jobs_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let structured = self.desktop.jobs_ready();
        let cancel_supported = self.desktop.cancel_job_supported();
        // Counts use the full list, never the truncated display rows, so a
        // >20-job session still reports its real running/total.
        let (running, total) = if structured {
            let jobs = self.desktop.jobs();
            (
                jobs.iter()
                    .filter(|job| running_status(&job.status))
                    .count(),
                jobs.len(),
            )
        } else {
            (
                self.activity
                    .jobs
                    .iter()
                    .filter(|job| job.is_running())
                    .count(),
                self.activity.jobs.len(),
            )
        };
        let rows: Vec<JobChipRow> = if structured {
            self.desktop
                .jobs()
                .iter()
                .take(20)
                .map(|job| {
                    let running = running_status(&job.status);
                    let (glyph, color) = if running {
                        ("⧗", ui::accent())
                    } else if matches!(job.exit_code, Some(JobExitCode::Code(0)))
                        && job.status != "failed"
                    {
                        ("✓", ui::success())
                    } else {
                        ("✗", ui::danger())
                    };
                    let state = match &job.exit_code {
                        Some(JobExitCode::Code(code)) => format!("exit {code}"),
                        Some(JobExitCode::Null) => "no exit code".to_string(),
                        None => job.status.clone(),
                    };
                    let label = if job.label.is_empty() {
                        format!("job {}", job.id)
                    } else {
                        job.label.clone()
                    };
                    JobChipRow {
                        id: job.id.clone(),
                        label,
                        glyph,
                        color,
                        state,
                        running,
                    }
                })
                .collect()
        } else {
            self.activity
                .jobs
                .iter()
                .take(20)
                .map(|job| {
                    let (glyph, color) = if job.is_running() {
                        ("⧗", ui::accent())
                    } else if job.exit_code.unwrap_or(0) == 0 && job.status != "failed" {
                        ("✓", ui::success())
                    } else {
                        ("✗", ui::danger())
                    };
                    let label = if job.label.is_empty() {
                        format!("job {}", job.id)
                    } else {
                        job.label.clone()
                    };
                    let state = match job.exit_code {
                        Some(code) => format!("exit {code}"),
                        None => job.status.clone(),
                    };
                    JobChipRow {
                        id: job.id.clone(),
                        label,
                        glyph,
                        color,
                        state,
                        running: job.is_running(),
                    }
                })
                .collect()
        };
        let color = if running > 0 {
            ui::accent()
        } else {
            ui::muted()
        };
        let label = if running > 0 {
            format!("{running} running")
        } else {
            format!("Jobs {total}")
        };
        let weak = cx.entity().downgrade();
        chip_popover(
            "jobs",
            IconName::Activity,
            color,
            label,
            "Background jobs",
            format!("{running}/{total}"),
            move || {
                rows.iter()
                    .map(|row| {
                        let mut item = h_flex()
                            .gap_2()
                            .text_size(px(12.))
                            .child(div().flex_none().text_color(row.color).child(row.glyph))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(ui::MONO)
                                    .text_color(ui::fg())
                                    .child(row.label.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(ui::faint())
                                    .child(row.state.clone()),
                            );
                        if row.running && cancel_supported {
                            let id = row.id.clone();
                            let weak = weak.clone();
                            item = item.child(
                                Button::new(SharedString::from(format!("job-cancel-{id}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::OctagonX))
                                    .tooltip("Cancel this job over the Desktop channel")
                                    .on_click(move |_, _, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.cancel_running_job(id.clone(), cx)
                                        });
                                    }),
                            );
                        }
                        item.into_any_element()
                    })
                    .collect()
            },
        )
    }

    /// Extension widgets (`setWidget`): one chip each, its lines on click.
    /// Gentle Shell's YOLO banner (`gentle:yolo`) is left out: the
    /// permission chip already shows the mode.
    fn render_widget_chips(&self) -> Vec<AnyElement> {
        self.extension
            .widgets
            .iter()
            .filter(|(key, _)| key.as_str() != "gentle:yolo")
            .map(|(key, lines)| {
                let lines = lines.clone();
                let title = key.clone();
                chip_popover(
                    SharedString::from(format!("widget-{key}")),
                    IconName::LayoutDashboard,
                    ui::muted(),
                    gentle_core::text::first_line(key, 18),
                    title,
                    String::new(),
                    move || {
                        lines
                            .iter()
                            .map(|line| {
                                div()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .font_family(ui::MONO)
                                    .text_color(ui::muted())
                                    .child(line.clone())
                                    .into_any_element()
                            })
                            .collect()
                    },
                )
                .into_any_element()
            })
            .collect()
    }

    /// Helpers (subagents): "1/3" active of total; the card lists each with
    /// its status, task and last step, and opens the full Agents window.
    fn render_helpers_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (active, rows) = if self.desktop.subagents_ready() {
            let agents = self.desktop.subagents();
            (
                agents
                    .iter()
                    .filter(|agent| {
                        matches!(agent.status.as_str(), "running" | "queued" | "waiting")
                    })
                    .count(),
                agents
                    .iter()
                    .take(20)
                    .map(|agent| {
                        (
                            agent.agent.clone(),
                            agent.status.clone(),
                            crate::features::session::agents::status_color(&agent.status),
                            None,
                            Some(agent.last_step.to_string()),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            match &self.activity.agents {
                Some(activity) => (
                    (activity.summary.running + activity.summary.waiting + activity.summary.queued)
                        as usize,
                    activity
                        .tasks
                        .iter()
                        .take(20)
                        .map(|task| {
                            let s = &task.summary;
                            (
                                s.agent.clone(),
                                s.status.clone(),
                                crate::features::session::agents::status_color(&s.status),
                                s.label.clone(),
                                s.last_step
                                    .as_deref()
                                    .map(|step| gentle_core::text::first_line(step, 60)),
                            )
                        })
                        .collect::<Vec<_>>(),
                ),
                None => (0, Vec::new()),
            }
        };
        let total = rows.len();
        let color = if active > 0 {
            ui::warning()
        } else {
            ui::muted()
        };
        let weak = cx.entity().downgrade();
        chip_popover(
            "helpers",
            IconName::Users,
            color,
            format!("{active}/{total}"),
            "Helpers",
            format!("{active} active"),
            move || {
                let mut items: Vec<AnyElement> = rows
                    .iter()
                    .map(|(agent, status, color, label, step)| {
                        v_flex()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .child(div().flex_none().size(px(6.)).rounded_full().bg(*color))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(ui::fg())
                                            .child(agent.clone()),
                                    )
                                    .child(
                                        div().flex_none().text_color(*color).child(status.clone()),
                                    ),
                            )
                            .when_some(label.clone(), |d, label| {
                                d.child(
                                    div()
                                        .pl_4()
                                        .min_w_0()
                                        .text_size(px(11.))
                                        .text_color(ui::muted())
                                        .child(label),
                                )
                            })
                            .when_some(step.clone(), |d, step| {
                                d.child(
                                    div()
                                        .pl_4()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(11.))
                                        .font_family(ui::MONO)
                                        .text_color(ui::faint())
                                        .child(step),
                                )
                            })
                            .into_any_element()
                    })
                    .collect();
                let weak = weak.clone();
                items.push(
                    h_flex()
                        .pt_1()
                        .child(
                            Button::new("helpers-open-agents")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::Users))
                                .label("Open Agents (⌘⇧A)")
                                .on_click(move |_, _, cx| {
                                    let _ = weak.update(cx, |this, cx| this.open_agents(None, cx));
                                }),
                        )
                        .into_any_element(),
                );
                items
            },
        )
    }

    /// The last response's tokens: output on the chip, the breakdown on click.
    fn render_usage_chip(&self) -> Option<impl IntoElement + use<>> {
        let usage = self.transcript.last_usage?;
        let rows = [
            ("Input", format_tokens(usage.input)),
            ("Output", format_tokens(usage.output)),
            ("Cache read", format_tokens(usage.cache_read)),
        ];
        Some(chip_popover(
            "last-usage",
            IconName::ChartPie,
            ui::faint(),
            format!("out {}", format_tokens(usage.output)),
            "Last response",
            String::new(),
            move || {
                rows.iter()
                    .map(|(name, value)| {
                        h_flex()
                            .text_size(px(12.))
                            .child(div().flex_1().text_color(ui::muted()).child(*name))
                            .child(
                                div()
                                    .font_family(ui::MONO)
                                    .text_color(ui::fg())
                                    .child(value.clone()),
                            )
                            .into_any_element()
                    })
                    .collect()
            },
        ))
    }
}

/// One row of the background-jobs chip, from either the structured channel or
/// the legacy `details.gentleJobs` source.
struct JobChipRow {
    id: String,
    label: String,
    glyph: &'static str,
    color: Hsla,
    state: String,
    running: bool,
}

/// Whether a job's status word means it is still running, the same vocabulary
/// the legacy `gentle_core::gentle_state::Job` uses.
fn running_status(status: &str) -> bool {
    matches!(status, "running" | "started" | "")
}

/// A status chip over the composer that opens a card above it: icon, title,
/// optional count, and the rows `body` builds when the card opens. Every
/// chip (todos, jobs, widgets, usage) shares this look.
pub(super) fn chip_popover(
    id: impl Into<SharedString>,
    icon: IconName,
    color: Hsla,
    label: impl Into<SharedString>,
    title: impl Into<SharedString>,
    count: String,
    body: impl Fn() -> Vec<AnyElement> + 'static,
) -> Popover {
    let id: SharedString = id.into();
    let title: SharedString = title.into();
    let trigger = Button::new(SharedString::from(format!("{id}-chip")))
        .ghost()
        .xsmall()
        .text_color(color)
        .icon(Icon::new(icon))
        .label(label);
    Popover::new(SharedString::from(format!("{id}-popover")))
        .anchor(Anchor::BottomRight)
        .trigger(trigger)
        .w(px(360.))
        .appearance(false)
        .content(move |_, _, cx| {
            ui::island(cx)
                .w_full()
                .child(ui::popover_in(
                    ElementId::Name(format!("{id}-content-in").into()),
                    v_flex()
                        .p_3()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Icon::new(icon).small().text_color(color))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(ui::fg())
                                        .child(title.clone()),
                                )
                                .child(div().flex_1())
                                .when(!count.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .font_family(ui::MONO)
                                            .text_color(ui::muted())
                                            .child(count.clone()),
                                    )
                                }),
                        )
                        .child(
                            v_flex()
                                .id(SharedString::from(format!("{id}-popover-list")))
                                .max_h(px(380.))
                                .overflow_y_scroll()
                                .gap_1p5()
                                .children(body()),
                        ),
                ))
                .into_any_element()
        })
}

/// `Claude rate limit warning: 51% used (seven_day)` → (51, `seven_day`).
pub(in crate::features::session) fn parse_rate_limit(message: &str) -> Option<(u32, String)> {
    const MARK: &str = "% used (";
    let at = message.find(MARK)?;
    let before = &message[..at];
    let digits = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let percent = before[at - digits..].parse().ok()?;
    let rest = &message[at + MARK.len()..];
    let window = &rest[..rest.find(')')?];
    (!window.is_empty()
        && window
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'))
    .then(|| (percent, window.to_string()))
}

/// `seven_day` → `7d`, `five_hour` → `5h`.
fn limit_label(window: &str) -> String {
    let short = window
        .replace("seven_day", "7d")
        .replace("five_hour", "5h")
        .replace("one_hour", "1h")
        .replace("one_day", "1d");
    short.replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::{limit_label, parse_rate_limit};

    #[test]
    fn rate_limit_warnings_parse() {
        assert_eq!(
            parse_rate_limit("Claude rate limit warning: 51% used (seven_day)"),
            Some((51, "seven_day".to_string()))
        );
        assert_eq!(parse_rate_limit("Everything is fine"), None);
        assert_eq!(limit_label("seven_day"), "7d");
        assert_eq!(limit_label("five_hour"), "5h");
        assert_eq!(limit_label("seven_day_opus"), "7d opus");
    }
}
