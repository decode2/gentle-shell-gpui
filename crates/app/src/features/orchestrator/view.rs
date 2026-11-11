//! The Fleet screen: the multi-agent board, driven by the local
//! orchestrator daemon. Lanes render attention-first; a detail section
//! below shows the selected session's journal, diff and evidence.

use std::time::Duration;

// ureq 3 wraps http::Response<ureq::Body>; the decode helpers work on it.

use gpui::Task;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use gentle_core::orchestrator::{Board, JournalEntry, Lane};
use gpui::Entity;

use super::client;
use super::stream;
use crate::shared::ui;

/// Millisecond wall timestamp for unique worker ids.
fn chrono_now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The lane's accent color, matching the daemon's semantic.
fn lane_color(lane: Lane) -> Hsla {
    match lane {
        Lane::NeedsYou => ui::warning(),
        Lane::Working => ui::petal(),
        Lane::InReview => ui::info(),
        Lane::ReadyToMerge => ui::success(),
    }
}

pub enum FleetEvent {
    Close,
}

pub struct FleetView {
    cwd: std::path::PathBuf,
    board: Option<Board>,
    agents: Vec<gentle_core::orchestrator::DetectedAgent>,
    projects: Vec<gentle_core::orchestrator::Project>,
    selected_project: String,
    selected: Option<String>,
    journal: Vec<JournalEntry>,
    diff_files: Vec<gentle_core::orchestrator::DiffFile>,
    evidence_status: Option<String>,
    evidence_summary: Option<String>,
    evidence_receipt: Option<String>,
    version: Option<String>,
    error: Option<String>,
    loading: bool,
    chat: Entity<InputState>,
    _load: Task<()>,
    _once: Task<()>,
    _stream: Task<()>,
}

impl FleetView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = cx.new(|cx| InputState::new(window, cx).placeholder("Escribile al worker…"));
        let _poll: Task<()> = cx.spawn(async move |this, cx| {
            loop {
                if let Err(error) = client::get_board() {
                    _ = this.update(cx, |view, cx| {
                        view.error = Some(error.clone());
                        cx.notify();
                    });
                } else if let Ok(board) = client::get_board() {
                    _ = this.update(cx, |view, cx| {
                        view.board = Some(board);
                        view.error = None;
                        cx.notify();
                    });
                }
                if this.upgrade().is_none() {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        });
        let (tx, rx) = smol::channel::unbounded::<Board>();
        stream::spawn_stream(client::base_url(), move |board| {
            let _ = tx.try_send(board);
        });
        let _stream: Task<()> = cx.spawn(async move |this, cx| {
            loop {
                let Ok(board) = rx.recv().await else { return };
                if this
                    .update(cx, |view, cx| {
                        view.board = Some(board.clone());
                        view.error = None;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        });

        let _once: Task<()> = cx.spawn(async move |this, cx| {
            if let Ok(agents) = client::get_agents() {
                _ = this.update(cx, |view, cx| {
                    view.agents = agents;
                    cx.notify();
                });
            }
            if let Ok(projects) = client::get_projects() {
                _ = this.update(cx, |view, cx| {
                    view.projects = projects;
                    cx.notify();
                });
            }
            if let Ok(version) = client::get_version() {
                _ = this.update(cx, |view, cx| {
                    view.version = Some(version.version);
                    cx.notify();
                });
            }
        });
        let _ = window;
        Self {
            cwd: std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default(),
            board: None,
            agents: Vec::new(),
            projects: Vec::new(),
            selected_project: String::new(),
            selected: None,
            journal: Vec::new(),
            diff_files: Vec::new(),
            evidence_status: None,
            evidence_summary: None,
            evidence_receipt: None,
            version: None,
            error: None,
            loading: true,
            chat,
            _load: _poll,
            _once,
            _stream,
        }
    }
}

impl EventEmitter<FleetEvent> for FleetView {}

impl Render for FleetView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let board = self
            .board
            .clone()
            .map(|b| b.for_project(&self.selected_project))
            .unwrap_or_default();
        let total = board.total();
        let agents = self.agents.clone();

        let lanes: Vec<AnyElement> = Lane::ALL
            .iter()
            .map(|lane| {
                let cards = board.lane(*lane).to_vec();
                let count = cards.len();
                let color = lane_color(*lane);
                let column = v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .p_2()
                    .rounded_lg()
                    .bg(ui::surface())
                    .border_1()
                    .border_color(ui::border())
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size_2().rounded_full().bg(color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(ui::muted())
                                    .child(lane.label().to_string()),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(ui::faint())
                                    .child(format!("{count}")),
                            ),
                    )
                    .children(cards.iter().enumerate().map(|(idx, card)| {
                        let agent_name = agents
                            .iter()
                            .find(|a| a.agent == card.agent)
                            .map(|a| a.display_name.clone())
                            .unwrap_or_else(|| card.agent.clone());
                        let session_id = card.id.clone();
                        let status = card
                            .question
                            .clone()
                            .or_else(|| card.status.clone())
                            .unwrap_or_default();
                        h_flex()
                            .id(("fleet-card", idx))
                            .flex_wrap()
                            .gap_x_2()
                            .p_2()
                            .rounded_md()
                            .bg(ui::raised())
                            .border_1()
                            .border_color(ui::border())
                            .cursor_pointer()
                            .hover(|d| d.border_color(ui::accent()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(session_id.clone());
                                this.load_detail(session_id.clone(), cx);
                                cx.notify();
                            }))
                            .child(div().text_sm().text_color(ui::fg()).child(card.id.clone()))
                            .children(card.pr.map(|pr| {
                                div()
                                    .text_xs()
                                    .text_color(ui::info())
                                    .child(format!("#{pr}"))
                            }))
                            .when(card.gate_pending, |d| {
                                d.child(div().text_xs().text_color(ui::info()).child("recibo…"))
                            })
                            .child(
                                div()
                                    .w_full()
                                    .text_xs()
                                    .text_color(ui::muted())
                                    .child(agent_name),
                            )
                            .when(!status.is_empty(), |d| {
                                d.child(
                                    div()
                                        .w_full()
                                        .text_xs()
                                        .text_color(if card.question.is_some() {
                                            ui::warning()
                                        } else {
                                            ui::muted()
                                        })
                                        .child(status),
                                )
                            })
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_xs()
                                    .text_color(ui::faint())
                                    .child(card.branch.clone()),
                            )
                            .into_any_element()
                    }));
                column.into_any_element()
            })
            .collect();

        let detail = self.selected.as_ref().map(|session| {
            let id = session.clone();
            let journal_rows: Vec<AnyElement> = self
                .journal
                .iter()
                .rev()
                .take(30)
                .rev()
                .map(|entry| {
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(ui::faint())
                                .child(entry.kind.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(ui::muted())
                                .child(entry.text.clone()),
                        )
                        .into_any_element()
                })
                .collect();
            let diff_rows: Vec<AnyElement> = self
                .diff_files
                .iter()
                .map(|file| {
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(ui::fg())
                                .child(file.path.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(ui::success())
                                .child(format!("+{}", file.additions)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(ui::danger())
                                .child(format!("−{}", file.deletions)),
                        )
                        .into_any_element()
                })
                .collect();
            v_flex()
                .gap_2()
                .p_2()
                .rounded_lg()
                .bg(ui::panel())
                .border_1()
                .border_color(ui::border())
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .text_color(ui::fg())
                                .child(format!("Sesión {id}")),
                        )
                        .child(
                            Button::new("fleet-stop")
                                .label("Detener")
                                .danger()
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.stop_session(id.clone(), cx);
                                })),
                        ),
                )
                .children(journal_rows)
                .children(diff_rows)
                .children(self.evidence_status.iter().map(|status| {
                    div()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(status.clone())
                }))
                .children(
                    self.evidence_summary
                        .iter()
                        .map(|summary| div().text_xs().text_color(ui::fg()).child(summary.clone())),
                )
                .children(self.evidence_receipt.iter().map(|receipt| {
                    div()
                        .text_xs()
                        .text_color(ui::faint())
                        .child(receipt.clone())
                }))
        });

        let chat_input = self.selected.as_ref().map(|session| {
            let id = session.clone();
            h_flex()
                .gap_2()
                .child(Input::new(&self.chat).small().flex_1())
                .child(
                    Button::new("fleet-send")
                        .label("Enviar")
                        .small()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let message = this.chat.read(cx).value().trim().to_string();
                            if message.is_empty() {
                                return;
                            }
                            this.send_chat(id.clone(), message, window, cx);
                        })),
                )
        });

        let header = h_flex()
            .gap_2()
            .items_center()
            .child(div().text_lg().text_color(ui::fg()).child("Fleet"))
            .child({
                let project = self.projects.first().cloned();
                Button::new("fleet-launch")
                    .label("+ Nueva sesión")
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let request = gentle_core::orchestrator::LaunchRequest {
                            id: format!("worker-{}", chrono_now_ms()),
                            task: "Describe qué querés que construya".into(),
                            agent: "pi".into(),
                            branch: format!("orch/worker-{}", chrono_now_ms()),
                            project: project.as_ref().map(|p| p.id.clone()),
                            launch: true,
                        };
                        if let Err(error) = client::launch(&request) {
                            this.error = Some(error);
                        }
                        cx.notify();
                    }))
            })
            .child(
                h_flex()
                    .gap_1()
                    .children(self.projects.iter().map(|project| {
                        let id = project.id.clone();
                        Button::new(SharedString::from(format!("fleet-project-{id}")))
                            .label(id)
                            .small()
                    }))
                    .child({
                        Button::new("fleet-add-project")
                            .label("+ Repo")
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                let name = this
                                    .cwd
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "repo".into());
                                let project = gentle_core::orchestrator::Project {
                                    id: name,
                                    repo: "owner/repo".into(),
                                    repo_path: Some(this.cwd.to_string_lossy().to_string()),
                                };
                                if let Err(error) = client::add_project(&project) {
                                    this.error = Some(error);
                                } else if let Ok(projects) = client::get_projects() {
                                    this.projects = projects;
                                }
                                cx.notify();
                            }))
                    }),
            )
            .child(div().text_xs().text_color(ui::faint()).child(format!(
						"{total} workers{}",
						self.version
							.as_ref()
							.map(|v| format!(" · daemon {v}"))
							.unwrap_or_default()
					)))
            .child(
                Button::new("fleet-close")
                    .icon(IconName::Close)
                    .ghost()
                    .small()
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(FleetEvent::Close);
                    })),
            );

        let body: AnyElement = if let Some(error) = &self.error {
            div()
                .p_4()
                .child(
                    h_flex()
                        .items_start()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(ui::danger().opacity(0.08))
                        .border_1()
                        .border_color(ui::danger().opacity(0.22))
                        .child(
                            Icon::new(IconName::CircleAlert)
                                .small()
                                .text_color(ui::danger())
                                .mt_0p5(),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(ui::danger())
                                .child(error.clone()),
                        ),
                )
                .into_any_element()
        } else if self.loading {
            h_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(Spinner::new().small())
                .into_any_element()
        } else if total == 0 {
            v_flex()
				.size_full()
				.items_center()
				.justify_center()
				.gap_2()
				.p_8()
				.child(Icon::new(IconName::Layers).text_color(ui::faint()))
				.child(
					div()
						.max_w(px(460.))
						.text_center()
						.text_sm()
						.text_color(ui::muted())
						.child("Sin sesiones todavía. Lanzá workers con + Nueva sesión cuando el daemon esté conectado; el board clasifica cada uno por hechos."),
				)
				.into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_h_0()
                .gap_2()
                .children(lanes)
                .into_any_element()
        };

        v_flex()
            .size_full()
            .gap_2()
            .p_3()
            .child(header)
            .child(body)
            .children(detail)
            .children(chat_input)
    }
}

impl FleetView {
    fn load_detail(&mut self, session: String, cx: &mut Context<Self>) {
        if let Ok(entries) = client::get_events(&session) {
            self.journal = entries;
        }
        if let Ok(diff) = client::get_diff(&session) {
            self.diff_files = diff.files;
        }
        if let Ok(evidence) = client::get_evidence(&session) {
            self.evidence_status = Some(evidence.status);
            self.evidence_summary = evidence.summary;
            self.evidence_receipt = evidence.receipt.map(|r| match evidence.valid {
                true => format!("recibo {r}"),
                false => format!("⚠ recibo alterado {r}"),
            });
            self.journal = evidence.journal;
        }
        cx.notify();
    }

    fn stop_session(&mut self, session: String, cx: &mut Context<Self>) {
        let result = client::stop(&session);
        if let Err(error) = result {
            self.error = Some(error);
        }
        cx.notify();
    }

    fn send_chat(
        &mut self,
        session: String,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = client::chat(&session, &message) {
            self.error = Some(error);
        } else {
            self.chat.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
            self.journal.push(JournalEntry {
                at: String::new(),
                kind: "chat-out".into(),
                text: message,
            });
        }
        cx.notify();
    }
}
