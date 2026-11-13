//! The confirmation card: unsaved edits, Delete, Make default, and Gentle
//! Shell's own dialog for an `apply_profile`.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use crate::features::profiles::{Confirm, ProfilesView};
use crate::shared::ui;

impl ProfilesView {
    pub(super) fn render_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.confirm.as_ref()?;
        let (title, lines, buttons): (String, Vec<String>, Vec<AnyElement>) = match confirm {
            Confirm::Unsaved(leave) => {
                let name = self.selected.clone().unwrap_or_default();
                let save_leave = leave.clone();
                let discard_leave = leave.clone();
                (
                    format!("\"{name}\" has {} unsaved edit(s)", self.change_count()),
                    vec!["Save them before leaving this profile, or discard them.".into()],
                    vec![
                        Button::new("unsaved-discard")
                            .small()
                            .danger()
                            .icon(Icon::new(IconName::Trash))
                            .label("Discard")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.draft = this.stored.clone();
                                this.proceed(discard_leave.clone(), window, cx);
                            }))
                            .into_any_element(),
                        Button::new("unsaved-save")
                            .small()
                            .primary()
                            .icon(Icon::new(IconName::Check))
                            .label("Save and continue")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let leave = save_leave.clone();
                                this.confirm = None;
                                this.save_then(cx, move |this, _, cx| {
                                    this.proceed_without_window(leave, cx)
                                });
                            }))
                            .into_any_element(),
                    ],
                )
            }
            Confirm::Delete(name) => {
                let target = name.clone();
                let models = self
                    .env
                    .as_ref()
                    .map(|e| e.models_path().display().to_string())
                    .unwrap_or_default();
                (
                    "Delete profile?".into(),
                    vec![format!(
                        "Delete profile \"{name}\" from profiles.json? The routing in {models} is not changed."
                    )],
                    vec![
                        Button::new("delete-ok")
                            .small()
                            .danger()
                            .icon(Icon::new(IconName::Trash))
                            .label("Delete")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.delete(target.clone(), cx)),
                            )
                            .into_any_element(),
                    ],
                )
            }
            Confirm::MakeDefault(plan) => {
                let target = plan.name.clone();
                (
                    plan.title.clone(),
                    plan.lines.clone(),
                    vec![
                        Button::new("default-ok")
                            .small()
                            .primary()
                            .icon(Icon::new(if plan.pinned {
                                IconName::GitBranch
                            } else {
                                IconName::Star
                            }))
                            .label(if plan.pinned {
                                "Pin this clone"
                            } else {
                                "Apply"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm = None;
                                this.make_default(target.clone(), cx);
                            }))
                            .into_any_element(),
                    ],
                )
            }
            // Gentle Shell's title and message, exactly as it sent them: the
            // token it resends is bound to this text.
            Confirm::ApplyProfile {
                title,
                message,
                retry,
            } => {
                let retry = retry.clone();
                (
                    title.clone(),
                    message.lines().map(str::to_string).collect(),
                    vec![
                        Button::new("apply-ok")
                            .small()
                            .primary()
                            .icon(Icon::new(IconName::Star))
                            .label("Apply")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm = None;
                                this.shell_apply(retry.clone(), cx);
                            }))
                            .into_any_element(),
                    ],
                )
            }
        };
        let icon = match confirm {
            Confirm::MakeDefault(_) | Confirm::ApplyProfile { .. } => {
                Icon::new(IconName::Star).text_color(ui::accent())
            }
            Confirm::Unsaved(_) => Icon::new(IconName::TriangleAlert).text_color(ui::warning()),
            Confirm::Delete(_) => Icon::new(IconName::Trash).text_color(ui::danger()),
        };
        Some(
            ui::pop_in(
                "profiles-confirm",
                v_flex()
                    .mx_4()
                    .mb_3()
                    .p_4()
                    .gap_2()
                    .rounded_xl()
                    .border_1()
                    .border_color(ui::border_strong())
                    .bg(ui::raised())
                    .shadow_lg()
                    .child(
                        h_flex().gap_2().child(icon.small().flex_shrink_0()).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui::fg())
                                .child(title),
                        ),
                    )
                    .child(
                        div()
                            .id("confirm-lines")
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .child(
                                v_flex().gap_0p5().children(
                                    lines
                                        .into_iter()
                                        .map(|l| div().text_xs().text_color(ui::muted()).child(l)),
                                ),
                            ),
                    )
                    .child(
                        h_flex()
                            .pt_1()
                            .gap_2()
                            .flex_wrap()
                            .justify_end()
                            .child(
                                Button::new("confirm-cancel")
                                    .small()
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm = None;
                                        cx.notify();
                                    })),
                            )
                            .children(buttons),
                    ),
            )
            .into_any_element(),
        )
    }
}
