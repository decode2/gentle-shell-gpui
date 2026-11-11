//! `Render` for the workspace window.

use super::*;

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Follow keyboard focus into panes (a terminal focused by its own click).
        if let Some(tab) = self.tab()
            && let Some(inside) = tab.tree.leaves().into_iter().find(|id| {
                self.panes
                    .get(id)
                    .is_some_and(|p| p.item.focus.contains_focused(window, cx))
            })
            && inside != tab.focused
            && let Some(tab) = self.tab_mut()
        {
            tab.focused = inside;
        }
        let sidebar = self.sidebar.clone().filter(|_| self.sidebar_visible);
        let has_sidebar = self.sidebar.is_some();
        // Below this the screen buttons fold into one overflow menu.
        let narrow = window.viewport_size().width < px(900.);
        // Native web views would cover a screen opened over them; device
        // cards stop capturing while covered.
        self.set_stages_covered(self.overlay.is_some(), cx);
        let (devices_docked, devices_floating) = {
            let panel = self.devices.read(cx);
            let docked = panel.mode() == crate::features::devices::Mode::Docked;
            (panel.visible() && docked, panel.visible() && !docked)
        };
        // While the surge plays, the window shakes before the rose hatches:
        // the root moves in place and asks for the next frame, so the element
        // tree (and every element's state) stays the same.
        let (shake_x, shake_y) = match self.surge {
            Some((_, started)) => {
                window.request_animation_frame();
                crate::shared::surge::shake_at(started.elapsed())
            }
            None => (0., 0.),
        };
        v_flex()
            .size_full()
            .text_color(ui::fg())
            // A click in a web page (a device, the browser card) takes the
            // keyboard away from GPUI; any click that reaches GPUI takes it back.
            .capture_any_mouse_down(|_, window, _| {
                crate::features::browser::reclaim_keyboard(window)
            })
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_new_terminal_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_tile_pane))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_new_agent_pane))
            .on_action(cx.listener(Self::on_new_terminal_pane))
            .on_action(cx.listener(Self::on_close_pane))
            .on_action(cx.listener(Self::on_focus_next_pane))
            .on_action(cx.listener(Self::on_focus_prev_pane))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_open_providers))
            .on_action(cx.listener(Self::on_open_sessions))
            .on_action(cx.listener(Self::on_open_palette))
            .on_action(cx.listener(Self::on_open_workspaces))
            .on_action(cx.listener(Self::on_close_overlay))
            .key_context("GentleWorkspace")
            .relative()
            .left(px(shake_x))
            .top(px(shake_y))
            .child(
                TitleBar::new()
                    .h(px(36.))
                    .bg(gpui_kit::transparent_black())
                    .child(
                        h_flex()
                            // Fill gpui-component's bar without counting toward
                            // its width: `w_full` there resolves against the
                            // content and pushed the buttons out of the window.
                            .flex_1()
                            .w_0()
                            .min_w_0()
                            .pr_2()
                            .gap_3()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .flex_1()
                                    // Width 0: the tabs take what the buttons leave.
                                    .w_0()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .child(self.render_workspace_chip(cx))
                                    .child(div().flex_none().w(px(1.)).h(px(14.)).bg(ui::border()))
                                    .child(self.render_tabs(cx)),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_1()
                                    // Without this, Windows answers clicks on these
                                    // buttons as title-bar drags (HTCAPTION).
                                    .occlude()
                                    // Tern keeps pane layout in the title bar, so
                                    // panes carry no header; these act on the
                                    // focused pane.
                                    .when(self.tab().is_some(), |d| {
                                        let mac = cfg!(target_os = "macos");
                                        d.child(
                                            Button::new("title-tile-pane")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::LayoutDashboard))
                                                .tooltip(format!(
                                                    "New pane, tiled ({})",
                                                    if mac { "⌘D" } else { "Ctrl+Shift+D" }
                                                ))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.on_tile_pane(&TilePane, window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("title-split-down")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::Rows2))
                                                .tooltip(format!(
                                                    "Split down ({})",
                                                    if mac { "⌘⇧D" } else { "Ctrl+Shift+E" }
                                                ))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.on_split_down(&SplitDown, window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("title-close-pane")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::X))
                                                .tooltip(format!(
                                                    "Close pane ({})",
                                                    if mac { "⌘W" } else { "Ctrl+W" }
                                                ))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.on_close_pane(&ClosePane, window, cx)
                                                })),
                                        )
                                        .child(div().w(px(1.)).h(px(14.)).mx_1().bg(ui::border()))
                                    })
                                    .when_some(
                                        crate::features::app_update::badge(cx),
                                        |d, (label, tip)| {
                                            d.child(
                                                div()
                                                    .id("app-update-pill")
                                                    .cursor_pointer()
                                                    .hover(|s| s.opacity(0.8))
                                                    .tooltip(move |window, cx| {
                                                        Tooltip::new(tip.clone()).build(window, cx)
                                                    })
                                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                        cx.stop_propagation()
                                                    })
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.open_settings(window, cx)
                                                    }))
                                                    .child(ui::chip(label, ui::accent())),
                                            )
                                        },
                                    )
                                    .children(crate::features::music::title_button(&self.music, cx))
                                    .children(self.render_devices_button(cx))
                                    .children(self.render_tray(cx))
                                    .child({
                                        let weak = cx.entity().downgrade();
                                        div()
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .child(notices::bell(
                                                &self.notices,
                                                Rc::new(move |pane, window, cx| {
                                                    let _ = weak.update(cx, |this, cx| {
                                                        this.reveal_pane(pane, window, cx)
                                                    });
                                                }),
                                                cx,
                                            ))
                                    })
                                    .child(
                                        Button::new("open-palette")
                                            .ghost()
                                            .xsmall()
                                            .icon(Icon::new(IconName::Command))
                                            .tooltip("Commands (⌘K)")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.on_open_palette(&OpenPalette, window, cx)
                                            })),
                                    )
                                    .when(narrow, |d| {
                                        let weak = cx.entity().downgrade();
                                        d.child(
                                            div()
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .child(
                                                    Button::new("title-overflow")
                                                        .ghost()
                                                        .xsmall()
                                                        .icon(Icon::new(IconName::Ellipsis))
                                                        .dropdown_menu(move |menu, _, _| {
                                                            type Open = fn(
                                                                &mut Workspace,
                                                                &mut Window,
                                                                &mut Context<Workspace>,
                                                            );
                                                            let entry = |icon: IconName,
                                                                         label: &'static str,
                                                                         detail: &'static str,
                                                                         open: Open| {
                                                                let weak = weak.clone();
                                                                PopupMenuItem::element(
                                                                    move |_, _| {
                                                                        menu_row(
                                                                            icon, label, detail,
                                                                            "",
                                                                        )
                                                                    },
                                                                )
                                                                .on_click(move |_, window, cx| {
                                                                    let _ = weak.update(
                                                                        cx,
                                                                        |this, cx| {
                                                                            open(this, window, cx)
                                                                        },
                                                                    );
                                                                })
                                                            };
                                                            menu.min_w(px(260.))
                                                                .item(entry(
                                                                    IconName::Settings,
                                                                    "Configuration",
                                                                    "Settings for this folder",
                                                                    |this, window, cx| {
                                                                        this.open_settings(
                                                                            window, cx,
                                                                        )
                                                                    },
                                                                ))
                                                                .item(entry(
                                                                    IconName::Layers,
                                                                    "Profiles",
                                                                    "Model and effort per agent",
                                                                    |this, window, cx| {
                                                                        this.open_profiles(
                                                                            window, cx,
                                                                        )
                                                                    },
                                                                ))
                                                                .item(entry(
                                                                    IconName::Brain,
                                                                    "Memory",
                                                                    "Engram observations",
                                                                    |this, window, cx| {
                                                                        this.open_memory(window, cx)
                                                                    },
                                                                ))
                                                                .item(entry(
                                                                    IconName::RotateCcwClock,
                                                                    "Sessions",
                                                                    "Resume a past session",
                                                                    |this, window, cx| {
                                                                        this.open_sessions(
                                                                            window, cx,
                                                                        )
                                                                    },
                                                                ))
                                                                .item(entry(
                                                                    IconName::KeyRound,
                                                                    "Providers",
                                                                    "Sign-in and API keys",
                                                                    |this, window, cx| {
                                                                        this.open_providers(
                                                                            window, cx,
                                                                        )
                                                                    },
                                                                ))
                                                        }),
                                                ),
                                        )
                                    })
                                    .when(!narrow, |d| {
                                        d.child(
                                            Button::new("open-settings")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::Settings))
                                                .tooltip("Configuration")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_settings(window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("open-profiles")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::Layers))
                                                .tooltip("Profiles: model and effort per agent")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_profiles(window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("open-memory")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::Brain))
                                                .tooltip("Memory (Engram)")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_memory(window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("open-sessions")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::RotateCcwClock))
                                                .tooltip("Sessions")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_sessions(window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("open-providers")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::KeyRound))
                                                .tooltip("Providers and sign-in")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_providers(window, cx)
                                                })),
                                        )
                                    })
                                    .when(has_sidebar, |d| {
                                        d.child(
                                            Button::new("toggle-sidebar")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(if self.sidebar_visible {
                                                    IconName::PanelLeftClose
                                                } else {
                                                    IconName::PanelLeftOpen
                                                }))
                                                .tooltip("Toggle sidebar (⌘B)")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_sidebar(cx)
                                                })),
                                        )
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .px_1p5()
                    .pb_1p5()
                    .gap_1p5()
                    .children(sidebar.map(|view| {
                        ui::appear(
                            "sidebar-in",
                            div()
                                .w(px(SIDEBAR_WIDTH))
                                .flex_none()
                                .h_full()
                                .rounded_xl()
                                .bg(ui::panel())
                                .border_1()
                                .border_color(gpui_kit::transparent_black())
                                .overflow_hidden()
                                .child(view.cached(StyleRefinement::default().size_full())),
                        )
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_body(window, cx)),
                    )
                    .when(devices_docked, |row| row.child(self.devices.clone())),
            )
            .children(self.stages.iter().filter_map(Stage::element))
            .when(devices_floating, |root| root.child(self.devices.clone()))
            .children(self.overlay.as_ref().map(|(overlay, _)| match overlay {
                Overlay::Providers(view) => view.clone().into_any_element(),
                Overlay::Sessions(view) => view.clone().into_any_element(),
                Overlay::Palette(view) => view.clone().into_any_element(),
                Overlay::Usage(view) => view.clone().into_any_element(),
                Overlay::Memory(view) => view.clone().into_any_element(),
                Overlay::Fleet(view) => view.clone().into_any_element(),
                Overlay::Settings(view) => view.clone().into_any_element(),
                Overlay::Profiles(view) => view.clone().into_any_element(),
                Overlay::Onboarding(view) => view.clone().into_any_element(),
                Overlay::Welcome(view) => view.clone().into_any_element(),
            }))
            .when_some(self.surge, |root, (run, _)| {
                root.child(crate::shared::surge::overlay(run))
            })
    }
}

impl Workspace {
    /// The title bar's device panel button, while "Use devices" is on.
    fn render_devices_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !crate::features::devices::hub(cx).read(cx).prefs().enabled {
            return None;
        }
        let open = self.devices.read(cx).visible();
        Some(
            div()
                // Clicks here are not title-bar drags.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new("title-devices")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Smartphone))
                        .when(open, |b| b.text_color(ui::petal()))
                        .tooltip("Devices: simulators and emulators")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.devices.update(cx, |panel, cx| panel.toggle(cx))
                        })),
                )
                .into_any_element(),
        )
    }
}
