//! The Agents panel: Gentle Agents subagents for this session, Tern-style.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Selectable as _, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use pi_rpc::desktop_rpc::DesktopAgent;

use super::SessionView;
use super::state::desktop;
use crate::shared::ui;
use gentle_core::activity::{AgentTask, ThreadItem};
use gentle_core::transcript::format_tokens;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentsTab {
    #[default]
    Agents,
    Activity,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentsFilter {
    #[default]
    All,
    Active,
    Finished,
}

impl AgentsFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Active,
            Self::Active => Self::Finished,
            Self::Finished => Self::All,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::All => "Filter: all",
            Self::Active => "Filter: active",
            Self::Finished => "Filter: finished",
        }
    }
    fn keeps(self, helper: &Helper) -> bool {
        match self {
            Self::All => true,
            Self::Active => helper.is_active(),
            Self::Finished => !helper.is_active(),
        }
    }
}

/// One helper shown in the panel: the structured channel projection when the
/// peer offers it, or the legacy `gentle-agents` widget entry otherwise. The
/// panel never shows both, so the two sources cannot fight or double a helper.
#[derive(Clone, Copy)]
enum Helper<'a> {
    Structured(&'a DesktopAgent),
    Legacy(&'a AgentTask),
}

impl Helper<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Structured(agent) => &agent.id,
            Self::Legacy(task) => &task.summary.id,
        }
    }

    fn agent(&self) -> &str {
        match self {
            Self::Structured(agent) => &agent.agent,
            Self::Legacy(task) => &task.summary.agent,
        }
    }

    fn status(&self) -> &str {
        match self {
            Self::Structured(agent) => &agent.status,
            Self::Legacy(task) => &task.summary.status,
        }
    }

    fn is_active(&self) -> bool {
        match self {
            Self::Structured(agent) => {
                matches!(agent.status.as_str(), "running" | "queued" | "waiting")
            }
            Self::Legacy(task) => task.summary.is_active(),
        }
    }

    fn label(&self) -> Option<&str> {
        match self {
            Self::Structured(_) => None,
            Self::Legacy(task) => task.summary.label.as_deref(),
        }
    }

    fn last_step(&self) -> Option<String> {
        match self {
            Self::Structured(agent) => Some(agent.last_step.to_string()),
            Self::Legacy(task) => task.summary.last_step.clone(),
        }
    }

    fn elapsed_ms(&self, now: u64) -> Option<u64> {
        match self {
            Self::Structured(_) => None,
            Self::Legacy(task) => task.summary.elapsed_ms(now),
        }
    }

    fn turns(&self) -> Option<u64> {
        match self {
            Self::Structured(_) => None,
            Self::Legacy(task) => Some(task.summary.turns),
        }
    }

    fn tool_calls(&self) -> Option<u64> {
        match self {
            Self::Structured(_) => None,
            Self::Legacy(task) => Some(task.summary.tool_calls),
        }
    }
}

/// Token breakdown lines for one typed subagent, with an honest "unavailable"
/// marker where a breakdown field is not recorded (an in-flight task, or a
/// restored run that never recorded one). Pure so it can be tested without GPUI.
/// Model and cost are shown separately by the detail pane.
pub fn agent_usage_lines(agent: &DesktopAgent) -> Vec<(&'static str, String)> {
    let usage = &agent.usage;
    let token = |value: Option<u64>| value.map_or_else(|| "unavailable".to_string(), format_tokens);
    vec![
        ("Tokens", format_tokens(usage.total)),
        ("Input", token(usage.input)),
        ("Output", token(usage.output)),
        ("Cache read", token(usage.cache)),
        ("Reasoning", token(usage.reasoning)),
    ]
}

/// A billed cost as money; the protocol carries a fractional, non-negative
/// number.
fn format_cost(cost: f64) -> String {
    format!("${cost:.4}")
}

/// A legacy count (turns, tools) that the structured projection does not carry.
fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "–".into(), |value| value.to_string())
}

/// Panel state kept on the session.
#[derive(Default)]
pub struct AgentsPanel {
    pub open: bool,
    pub tab: AgentsTab,
    pub filter: AgentsFilter,
    pub selected: Option<String>,
    pub expanded_tools: std::collections::HashSet<(String, usize)>,
}

pub fn status_color(status: &str) -> Hsla {
    match status {
        "running" => ui::warning(),
        "waiting" | "queued" => ui::accent(),
        "done" | "finished" | "completed" | "succeeded" => ui::success(),
        "failed" | "error" | "aborted" | "cancelled" | "stopped" => ui::danger(),
        _ => ui::muted(),
    }
}

fn duration(ms: Option<u64>) -> String {
    ms.map(ui::format_duration).unwrap_or_else(|| "–".into())
}

/// Wall-clock milliseconds, the unit of the activity document's timestamps.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl SessionView {
    pub(super) fn open_agents(&mut self, select: Option<String>, cx: &mut Context<Self>) {
        self.agents_panel.open = true;
        if select.is_some() {
            self.agents_panel.selected = select;
        }
        // Tick running times once a second while the panel is open.
        self._agents_ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |this, cx| {
                        let open = this.agents_panel.open;
                        if open
                            && this
                                .activity
                                .agents
                                .as_ref()
                                .is_some_and(|a| a.tasks.iter().any(|t| t.summary.is_active()))
                        {
                            cx.notify();
                        }
                        open
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        cx.notify();
    }

    pub(super) fn close_agents(&mut self, cx: &mut Context<Self>) {
        self.agents_panel.open = false;
        self._agents_ticker = Task::ready(());
        cx.notify();
    }

    /// The helpers to show: structured first once the channel seeded them, the
    /// legacy `gentle-agents` widget otherwise.
    fn helpers(&self) -> Vec<Helper<'_>> {
        if self.desktop.subagents_ready() {
            self.desktop
                .subagents()
                .iter()
                .map(Helper::Structured)
                .collect()
        } else {
            self.activity
                .agents
                .iter()
                .flat_map(|activity| activity.tasks.iter())
                .map(Helper::Legacy)
                .collect()
        }
    }

    fn selected_helper(&self) -> Option<Helper<'_>> {
        let helpers = self.helpers();
        let id = self.agents_panel.selected.as_deref();
        helpers
            .iter()
            .copied()
            .find(|helper| Some(helper.id()) == id)
            .or_else(|| helpers.first().copied())
    }

    /// Stop one helper over `cancel_subagent`; the legacy prompt stays only for
    /// a peer that genuinely does not offer the method.
    fn stop_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let applied = self.desktop.cancel_subagent(id.to_string());
        match applied.report.as_ref() {
            Some(desktop::supervision::Report::Unsupported) => self.ask_to_stop_agent(id, cx),
            _ => self.apply_supervision(applied, cx),
        }
    }

    /// Gentle Shell has no host-side stop for helpers; ask the agent to cancel it.
    fn ask_to_stop_agent(&mut self, id: &str, cx: &mut Context<Self>) {
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

    pub(super) fn render_agents_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.agents_panel.open {
            return None;
        }
        let structured = self.desktop.subagents_ready();
        let helpers = self.helpers();
        let now = now_ms();
        let filter = self.agents_panel.filter;
        let visible: Vec<Helper> = helpers
            .iter()
            .copied()
            .filter(|h| filter.keeps(h))
            .collect();
        let selected = self.selected_helper();
        let selected_id = selected.map(|h| h.id().to_string());
        let running = helpers.iter().filter(|h| h.status() == "running").count();
        let waiting = helpers.iter().filter(|h| h.status() == "waiting").count();
        let queued = helpers.iter().filter(|h| h.status() == "queued").count();
        let finished = helpers.iter().filter(|h| !h.is_active()).count();
        let total_turns: u64 = helpers.iter().filter_map(|h| h.turns()).sum();
        let total_tools: u64 = helpers.iter().filter_map(|h| h.tool_calls()).sum();
        let mut summary_bits = vec![
            format!("{running} running"),
            format!("{waiting} waiting"),
            format!("{queued} queued"),
            format!("{finished} finished"),
        ];
        if !structured {
            summary_bits.push(format!("{total_turns} turns"));
            summary_bits.push(format!("{total_tools} tools"));
        }
        let summary = summary_bits.join(" · ");

        let header = ui::screen_header()
            .child(Icon::new(IconName::Users).small().text_color(ui::muted()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::fg())
                    .child("Agents"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(ui::muted())
                    .child(summary),
            )
            .when(running > 0, |d| {
                d.child(ui::pulse(
                    div()
                        .flex_shrink_0()
                        .size(px(6.))
                        .rounded_full()
                        .bg(ui::warning()),
                ))
            })
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .font_family(ui::MONO)
                    .text_color(ui::faint())
                    .child(helpers.len().to_string()),
            )
            .child(
                Button::new("agents-close")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::X))
                    .on_click(cx.listener(|this, _, _, cx| this.close_agents(cx))),
            );

        let tab_button = |id: &'static str, label: &'static str, tab: AgentsTab| {
            let active = self.agents_panel.tab == tab;
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(active, |d| d.bg(ui::raised()).text_color(ui::fg()))
                .when(!active, |d| {
                    d.text_color(ui::muted()).hover(|d| d.text_color(ui::fg()))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.agents_panel.tab = tab;
                    cx.notify();
                }))
                .child(label)
        };
        let tabs = h_flex()
            .self_start()
            .p(px(2.))
            .gap(px(2.))
            .rounded_lg()
            .bg(ui::well())
            .border_1()
            .border_color(ui::border())
            .child(tab_button("agents-tab", "Agents", AgentsTab::Agents))
            .child(tab_button("activity-tab", "Activity", AgentsTab::Activity));

        let left_body = match self.agents_panel.tab {
            AgentsTab::Agents => {
                self.render_agents_table(&visible, selected_id.as_deref(), now, cx)
            }
            AgentsTab::Activity => match selected {
                Some(Helper::Legacy(task)) => self.render_thread(task, cx),
                Some(Helper::Structured(_)) => div()
                    .pt_6()
                    .text_sm()
                    .text_center()
                    .text_color(ui::muted())
                    .child("The transcript is not available for helpers from this Gentle Shell.")
                    .into_any_element(),
                None => div().into_any_element(),
            },
        };

        let left = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_3()
            .gap_3()
            .child(tabs)
            .child(
                div()
                    .id("agents-left")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(if helpers.is_empty() {
                        v_flex()
                            .pt_10()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::Bot).text_color(ui::faint()))
                            .child(
                                div()
                                    .max_w(px(360.))
                                    .text_sm()
                                    .text_center()
                                    .text_color(ui::muted())
                                    .child("No subagents yet. When el Gentleman delegates work, helpers appear here live."),
                            )
                            .into_any_element()
                    } else {
                        left_body
                    }),
            );

        let right = v_flex()
            .id("agents-detail")
            .w(relative(0.36))
            .min_w(px(240.))
            .max_w(px(420.))
            .flex_shrink_0()
            .h_full()
            .p_5()
            .gap_3()
            .border_l_1()
            .border_color(ui::border())
            .overflow_y_scroll()
            .children(selected.map(|helper| self.render_agent_detail(&helper, now)));

        let footer = h_flex()
            .px_4()
            .py_3()
            .gap_2()
            .border_t_1()
            .border_color(ui::border())
            .when_some(
                selected
                    .filter(|h| h.is_active())
                    .map(|h| h.id().to_string()),
                |d, id| {
                    let rpc = selected.is_some_and(|h| {
                        h.is_active()
                            && matches!(h, Helper::Structured(_))
                            && self.desktop.cancel_subagent_supported()
                    });
                    if rpc {
                        let id = id.clone();
                        d.child(
                            Button::new("agents-stop")
                                .danger()
                                .outline()
                                .small()
                                .icon(Icon::new(IconName::OctagonX))
                                .label("Stop")
                                .tooltip("Ask Gentle Shell to cancel this helper over the Desktop channel")
                                .on_click(cx.listener(move |this, _, _, cx| this.stop_agent(&id, cx))),
                        )
                    } else {
                        d.child(
                            Button::new("agents-stop")
                                .danger()
                                .outline()
                                .small()
                                .icon(Icon::new(IconName::OctagonX))
                                .label("Ask to stop")
                                .tooltip("Gentle Shell stops helpers from its agent: this asks it to run subagent_cancel")
                                .on_click(cx.listener(move |this, _, _, cx| this.ask_to_stop_agent(&id, cx))),
                        )
                    }
                },
            )
            .child(
                Button::new("agents-filter")
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::Funnel))
                    .label(filter.label())
                    .selected(filter != AgentsFilter::All)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.agents_panel.filter = this.agents_panel.filter.next();
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("agents-close")
                    .ghost()
                    .small()
                    .label("Close")
                    .on_click(cx.listener(|this, _, _, cx| this.close_agents(cx))),
            )
            .child(
                Button::new("agents-transcript")
                    .primary()
                    .small()
                    .icon(Icon::new(if self.agents_panel.tab == AgentsTab::Activity {
                        IconName::ArrowLeft
                    } else {
                        IconName::MessageSquare
                    }))
                    .label(if self.agents_panel.tab == AgentsTab::Activity { "Back to agents" } else { "Open transcript" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.agents_panel.tab = match this.agents_panel.tab {
                            AgentsTab::Agents => AgentsTab::Activity,
                            AgentsTab::Activity => AgentsTab::Agents,
                        };
                        cx.notify();
                    })),
            );

        let (backdrop, panel) = ui::screen(None);
        Some(
            backdrop
                .id("agents-overlay")
                .occlude()
                .child(ui::pop_in(
                    "agents-screen",
                    panel
                        .child(header)
                        .child(h_flex().flex_1().min_h_0().child(left).child(right))
                        .child(footer),
                ))
                .into_any_element(),
        )
    }

    fn render_agents_table(
        &self,
        helpers: &[Helper],
        selected: Option<&str>,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let column = |w: f32, text: String| div().w(px(w)).flex_shrink_0().text_right().child(text);
        let head = h_flex()
            .px_3()
            .pb_1()
            .gap_3()
            .text_xs()
            .text_color(ui::faint())
            .child(div().w(px(10.)).flex_shrink_0())
            .child(div().flex_1().min_w_0().child("Agent"))
            .child(column(72., "Status".into()))
            .child(column(56., "Time".into()))
            .child(column(40., "Turns".into()))
            .child(column(40., "Tools".into()));
        let rows = helpers.iter().enumerate().map(|(ix, helper)| {
            let id = helper.id().to_string();
            let is_selected = selected == Some(helper.id());
            let color = status_color(helper.status());
            let (name, kind) = match helper.label() {
                Some(label) if !label.is_empty() => {
                    (label.to_string(), Some(helper.agent().to_string()))
                }
                _ => (helper.agent().to_string(), None),
            };
            let running = helper.status() == "running";
            let row = h_flex()
                .id(SharedString::from(format!("agent-row-{}", helper.id())))
                .px_3()
                .py_2()
                .gap_3()
                .rounded_lg()
                .cursor_pointer()
                .when(is_selected, |d| d.bg(ui::raised()))
                .when(!is_selected, |d| d.hover(|d| d.bg(ui::surface_hover())))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.agents_panel.selected = Some(id.clone());
                    if event.click_count() >= 2 {
                        this.agents_panel.tab = AgentsTab::Activity;
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(10.))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .child(if running {
                            ui::pulse(div().size(px(7.)).rounded_full().bg(color))
                                .into_any_element()
                        } else {
                            div()
                                .size(px(7.))
                                .rounded_full()
                                .bg(color)
                                .into_any_element()
                        }),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .min_w(px(48.))
                                .max_w(px(200.))
                                .truncate()
                                .text_sm()
                                .font_family(ui::MONO)
                                .text_color(ui::fg())
                                .child(name),
                        )
                        .when_some(kind, |d, kind| d.child(kind_chip(kind)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(ui::muted())
                                .child(helper.last_step().unwrap_or_default()),
                        ),
                )
                .child(
                    column(72., helper.status().to_string())
                        .truncate()
                        .text_xs()
                        .text_color(color),
                )
                .child(
                    column(56., duration(helper.elapsed_ms(now)))
                        .text_xs()
                        .font_family(ui::MONO)
                        .text_color(ui::muted()),
                )
                .child(
                    column(40., count(helper.turns()))
                        .text_xs()
                        .font_family(ui::MONO)
                        .text_color(ui::muted()),
                )
                .child(
                    column(40., count(helper.tool_calls()))
                        .text_xs()
                        .font_family(ui::MONO)
                        .text_color(ui::muted()),
                );
            ui::appear(("agent-row-in", ix), div().child(row))
        });
        v_flex()
            .gap_0p5()
            .child(head)
            .children(rows)
            .into_any_element()
    }

    fn render_agent_detail(&self, helper: &Helper, now: u64) -> AnyElement {
        let field = |label: &'static str, value: String, mono: bool| {
            v_flex().gap_1().child(ui::section_label(label)).child(
                div()
                    .text_sm()
                    .text_color(ui::fg())
                    .when(mono, |d| d.font_family(ui::MONO).text_xs())
                    .child(value),
            )
        };
        let stat = |label: &'static str, value: String| {
            ui::card()
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2()
                .gap_0p5()
                .child(ui::section_label(label))
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_family(ui::MONO)
                        .text_color(ui::fg())
                        .child(value),
                )
        };
        match helper {
            Helper::Structured(agent) => {
                let color = status_color(&agent.status);
                v_flex()
                    .gap_4()
                    .child(
                        v_flex().gap_1().child(
                            h_flex()
                                .gap_2()
                                .min_w_0()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(ui::fg())
                                        .child(agent.agent.clone()),
                                )
                                .child(ui::chip(agent.status.clone(), color).flex_shrink_0()),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(stat(
                                "Model",
                                agent.model.clone().unwrap_or_else(|| "unavailable".into()),
                            ))
                            .child(stat("Cost", format_cost(agent.usage.cost))),
                    )
                    .child(
                        v_flex().gap_1().child(ui::section_label("Usage")).children(
                            agent_usage_lines(agent)
                                .into_iter()
                                .map(|(label, value)| field(label, value, true)),
                        ),
                    )
                    .when_some(agent.thinking.clone(), |d, thinking| {
                        d.child(field("Thinking", thinking, false))
                    })
                    .child(field("Last step", agent.last_step.to_string(), true))
                    .child(field("Id", agent.id.clone(), true))
            }
            Helper::Legacy(task) => {
                let s = &task.summary;
                let color = status_color(&s.status);
                v_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(ui::fg())
                                            .child(s.agent.clone()),
                                    )
                                    .child(ui::chip(s.status.clone(), color).flex_shrink_0()),
                            )
                            .when_some(s.label.clone(), |d, label| {
                                d.child(div().text_xs().text_color(ui::muted()).child(label))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(stat("Time", duration(s.elapsed_ms(now))))
                            .child(stat("Turns", s.turns.to_string()))
                            .child(stat("Tools", s.tool_calls.to_string())),
                    )
                    .when_some(s.error.clone(), |d, error| {
                        d.child(
                            v_flex()
                                .gap_1()
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .bg(ui::danger().opacity(0.08))
                                .border_1()
                                .border_color(ui::danger().opacity(0.22))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(ui::danger())
                                        .child("ERROR"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family(ui::MONO)
                                        .text_color(ui::danger())
                                        .child(error),
                                ),
                        )
                    })
                    .when_some(s.last_step.clone(), |d, step| {
                        d.child(field("Last step", step, true))
                    })
                    .when_some(s.prompt.clone(), |d, prompt| {
                        d.child(
                            v_flex().gap_1().child(ui::section_label("Task")).child(
                                ui::well_box()
                                    .px_3()
                                    .py_2()
                                    .text_color(ui::fg())
                                    .whitespace_normal()
                                    .child(prompt),
                            ),
                        )
                    })
                    .child(field("Id", s.id.clone(), true))
            }
        }
        .into_any_element()
    }

    fn render_thread(&self, task: &AgentTask, cx: &mut Context<Self>) -> AnyElement {
        let id = task.summary.id.clone();
        let mut column = v_flex().gap_2().px_1();
        if task.thread.dropped > 0 {
            column = column.child(
                div()
                    .text_xs()
                    .text_color(ui::faint())
                    .child(format!("… {} earlier items dropped", task.thread.dropped)),
            );
        }
        for (ix, item) in task.thread.items.iter().enumerate() {
            column =
                column.child(match item {
                    ThreadItem::Text { text } => div()
                        .text_sm()
                        .text_color(ui::fg())
                        .child(text.clone())
                        .into_any_element(),
                    ThreadItem::Thinking { text } => div()
                        .text_xs()
                        .italic()
                        .text_color(ui::faint())
                        .child(text.clone())
                        .into_any_element(),
                    ThreadItem::Note { text } => div()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(text.clone())
                        .into_any_element(),
                    ThreadItem::Tool {
                        name,
                        args,
                        running,
                        is_error,
                        output,
                    } => {
                        let key = (id.clone(), ix);
                        let open = self.agents_panel.expanded_tools.contains(&key);
                        let status: AnyElement = if *running {
                            ui::spinner(Size::XSmall, ui::accent()).into_any_element()
                        } else if *is_error {
                            Icon::new(IconName::X)
                                .xsmall()
                                .text_color(ui::danger())
                                .into_any_element()
                        } else {
                            Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(ui::success())
                                .into_any_element()
                        };
                        let body = output.clone().unwrap_or_default();
                        let (shown, hidden) = ui::tail(&body, if open { 40 } else { 3 });
                        ui::card()
                            .overflow_hidden()
                            .child(
                                h_flex()
                                    .id(SharedString::from(format!("thread-{id}-{ix}")))
                                    .px_3()
                                    .py_1p5()
                                    .gap_2()
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(ui::surface_hover()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.agents_panel.expanded_tools.remove(&key) {
                                            this.agents_panel.expanded_tools.insert(key.clone());
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        Icon::new(if open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .xsmall()
                                        .text_color(ui::faint()),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(ui::champagne())
                                            .child(name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .font_family(ui::MONO)
                                            .text_color(ui::muted())
                                            .child(ThreadItem::tool_subject(args)),
                                    )
                                    .child(status),
                            )
                            .when(!shown.trim().is_empty(), |d| {
                                d.child(
                                    v_flex()
                                        .px_3()
                                        .py_1p5()
                                        .border_t_1()
                                        .border_color(ui::border())
                                        .bg(ui::well())
                                        .overflow_hidden()
                                        .font_family(ui::MONO)
                                        .text_xs()
                                        .text_color(ui::muted())
                                        .when(hidden > 0, |d| {
                                            d.child(
                                                div()
                                                    .text_color(ui::faint())
                                                    .child(format!("… {hidden} more lines")),
                                            )
                                        })
                                        .children(shown.lines().map(|l| {
                                            div().whitespace_nowrap().child(l.to_string())
                                        })),
                                )
                            })
                            .into_any_element()
                    }
                    ThreadItem::Other => div().into_any_element(),
                });
        }
        column.into_any_element()
    }
}

/// Tern's model pill: outlined, rounded, with a small leading dot.
fn kind_chip(text: String) -> Div {
    h_flex()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .gap_1()
        .px_2()
        .h(px(18.))
        .rounded_full()
        .border_1()
        .border_color(ui::border_strong())
        .bg(ui::well())
        .text_size(px(10.5))
        .text_color(ui::muted())
        .child(
            div()
                .flex_shrink_0()
                .size(px(4.))
                .rounded_full()
                .bg(ui::faint()),
        )
        .child(text)
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{DesktopAgent, DesktopAgentStep, DesktopAgentUsage};

    use super::{agent_usage_lines, format_cost};

    fn agent(model: Option<&str>, usage: DesktopAgentUsage) -> DesktopAgent {
        DesktopAgent {
            id: "a1".into(),
            agent: "worker".into(),
            status: "running".into(),
            model: model.map(str::to_string),
            model_source: None,
            fallback_from: None,
            thinking: None,
            last_step: DesktopAgentStep::Writing,
            usage,
        }
    }

    #[test]
    fn usage_lines_report_tokens_and_honest_unavailable_breakdown() {
        let full = agent(
            Some("anthropic/claude-sonnet-4"),
            DesktopAgentUsage {
                total: 15234,
                cost: 0.0421,
                input: Some(12000),
                output: Some(3234),
                cache: Some(5000),
                reasoning: Some(4000),
            },
        );
        let lines = agent_usage_lines(&full);
        assert_eq!(lines[0], ("Tokens", "15.2k".to_string()));
        assert_eq!(lines[1], ("Input", "12k".to_string()));
        assert_eq!(lines[2], ("Output", "3.2k".to_string()));
        assert_eq!(lines[3], ("Cache read", "5k".to_string()));
        assert_eq!(lines[4], ("Reasoning", "4k".to_string()));

        // Breakdown absent: honest "unavailable", never invented zeroes.
        let partial = agent(
            None,
            DesktopAgentUsage {
                total: 0,
                cost: 0.0,
                input: None,
                output: None,
                cache: None,
                reasoning: None,
            },
        );
        let lines = agent_usage_lines(&partial);
        assert_eq!(lines[0], ("Tokens", "0".to_string()));
        assert_eq!(lines[1].1, "unavailable");
        assert_eq!(lines[2].1, "unavailable");
        assert_eq!(lines[3].1, "unavailable");
        assert_eq!(lines[4].1, "unavailable");
    }

    #[test]
    fn cost_is_money() {
        assert_eq!(format_cost(0.0421), "$0.0421");
        assert_eq!(format_cost(0.0), "$0.0000");
        assert_eq!(format_cost(1.5), "$1.5000");
    }
}
