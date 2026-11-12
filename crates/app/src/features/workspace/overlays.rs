//! Screens drawn over the workspace: palette, settings, providers, sessions,
//! usage, memory, profiles, onboarding; and the surge. Floating web and
//! device cards live in `stages.rs`.

use super::*;

impl Workspace {
    // ── Screens ─────────────────────────────────────────────────────────────

    /// Open the switcher on `category`; the same shortcut again closes it, the
    /// other one changes category.
    pub(super) fn open_palette(
        &mut self,
        category: Category,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((Overlay::Palette(view), _)) = &self.overlay {
            if view.read(cx).category() == category {
                self.close_overlay(window, cx);
            } else {
                view.update(cx, |view, cx| view.set_category(category, window, cx));
            }
            return;
        }
        self.start_active_session(window, cx);
        let commands = self
            .active_session()
            .map(|t| t.read(cx).commands())
            .unwrap_or_default();
        let workspaces = self
            .spaces
            .iter()
            .enumerate()
            .map(|(index, space)| WorkspaceRow {
                index,
                name: space.name.clone(),
                tabs: space.tab_count(),
                current: index == self.current,
            })
            .collect();
        let input = PaletteInput {
            commands,
            workspaces,
            root: self.space().root.clone(),
            category,
        };
        let view = cx.new(|cx| PaletteView::new(input, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &PaletteEvent, window, cx| match event {
                PaletteEvent::Close => this.close_overlay(window, cx),
                PaletteEvent::Run(PaletteAction::RenameWorkspace { index, name }) => {
                    // Renaming happens in place; the switcher stays open.
                    this.rename_workspace(*index, name.clone(), cx);
                }
                PaletteEvent::Run(action) => {
                    this.close_overlay(window, cx);
                    this.run_palette_action(action.clone(), window, cx);
                }
            },
        );
        self.overlay = Some((Overlay::Palette(view), subscription));
        cx.notify();
    }

    pub(super) fn run_palette_action(
        &mut self,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.active_session();
        match action {
            PaletteAction::Providers => self.open_providers(window, cx),
            PaletteAction::Sessions => self.open_sessions(window, cx),
            PaletteAction::Profiles => self.open_profiles(window, cx),
            PaletteAction::Usage => self.open_usage(window, cx),
            PaletteAction::Memory => self.open_memory(window, cx),
            PaletteAction::Fleet => self.open_fleet(window, cx),
            PaletteAction::Devices => self.devices.update(cx, |panel, cx| panel.show(cx)),
            PaletteAction::Settings => self.open_settings(window, cx),
            PaletteAction::Setup => self.open_onboarding(None, window, cx),
            PaletteAction::Welcome => self.open_welcome(None, window, cx),
            PaletteAction::NewTab => self.on_new_tab(&NewTab, window, cx),
            PaletteAction::NewTerminalTab => self.on_new_terminal_tab(&NewTerminalTab, window, cx),
            PaletteAction::TilePane => self.tile(None, window, cx),
            PaletteAction::SplitRight => self.split(SplitAxis::Horizontal, None, window, cx),
            PaletteAction::SplitDown => self.split(SplitAxis::Vertical, None, window, cx),
            PaletteAction::NewAgentPane => self.split_with_kind(PaneKind::Agent, window, cx),
            PaletteAction::NewTerminalPane => self.split_with_kind(PaneKind::Terminal, window, cx),
            PaletteAction::ClosePane => self.on_close_pane(&ClosePane, window, cx),
            PaletteAction::ToggleSidebar => self.toggle_sidebar(cx),
            PaletteAction::Agents => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.open_agents_panel(cx));
                }
            }
            PaletteAction::NewSession => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.new_session(window, cx));
                }
            }
            PaletteAction::Command(name) => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.run_prompt(format!("/{name}"), cx));
                }
            }
            PaletteAction::Screen(screen) => self.open_screen(screen, tab, window, cx),
            PaletteAction::SwitchWorkspace(index) => self.switch_workspace(index, window, cx),
            PaletteAction::RenameWorkspace { index, name } => {
                self.rename_workspace(index, name, cx)
            }
            PaletteAction::DeleteWorkspace(index) => {
                self.confirm_delete_workspace(index, window, cx)
            }
            PaletteAction::CreateWorkspace {
                name,
                pick_folder: false,
            } => {
                let root = self.active_cwd(cx);
                self.create_workspace(Some(name), root, window, cx);
            }
            PaletteAction::CreateWorkspace {
                name,
                pick_folder: true,
            } => self.pick_workspace_folder(name, window, cx),
            PaletteAction::OpenFile(path) => self.open_file(path, window, cx),
            PaletteAction::SwitchBranch(branch) => self.confirm_switch_branch(branch, window, cx),
        }
    }

    /// Play the max-effort surge over the window; choosing max again
    /// restarts it.
    pub(super) fn play_surge(&mut self, cx: &mut Context<Self>) {
        self.surge_runs += 1;
        let run = self.surge_runs;
        self.surge = Some((run, std::time::Instant::now()));
        self._surge_end = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::shared::surge::DURATION)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.surge.is_some_and(|(playing, _)| playing == run) {
                    this.surge = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// The native screen of a terminal-only Gentle Shell command; `session`
    /// is the agent it concerns (the one that ran it, else the active one).
    pub(super) fn open_screen(
        &mut self,
        screen: Screen,
        session: Option<Entity<SessionView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match screen {
            Screen::Usage => self.open_usage(window, cx),
            Screen::Stats => {
                self.open_usage(window, cx);
                if let Some((Overlay::Usage(view), _)) = &self.overlay {
                    view.update(cx, |view, cx| view.show_stats(cx));
                }
            }
            // Background jobs have no window of their own: the Agents window
            // is where the session's background work lives.
            Screen::Agents | Screen::Jobs => {
                if let Some(session) = session.or_else(|| self.active_session()) {
                    session.update(cx, |s, cx| s.open_agents_panel(cx));
                }
            }
            Screen::Profiles => self.open_profiles(window, cx),
            Screen::Palette => {
                self.close_overlay(window, cx);
                self.open_palette(Category::Commands, window, cx);
            }
            Screen::Changes => {
                let Some(tree) = self
                    .sidebar
                    .clone()
                    .and_then(|view| view.downcast::<FileTreeView>().ok())
                else {
                    return;
                };
                if !self.sidebar_visible {
                    self.toggle_sidebar(cx);
                }
                tree.update(cx, |tree, cx| tree.show_changes(window, cx));
            }
        }
    }

    pub(super) fn open_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_active_session(window, cx);
        let cwd = self.active_cwd(cx);
        let current = self
            .active_session()
            .and_then(|t| t.read(cx).session_file());
        let view = cx.new(|cx| SessionsView::new(cwd, current, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SessionsEvent, window, cx| match event {
                SessionsEvent::Close => this.close_overlay(window, cx),
                SessionsEvent::NewSession => {
                    this.close_overlay(window, cx);
                    if let Some(tab) = this.active_session() {
                        tab.update(cx, |s, cx| s.new_session(window, cx));
                    }
                }
                SessionsEvent::Open { path, cwd } => {
                    this.close_overlay(window, cx);
                    let path = path.clone();
                    if *cwd == this.active_cwd(cx) {
                        if let Some(tab) = this.active_session() {
                            tab.update(cx, |s, cx| s.open_session(path, cx));
                        }
                    } else {
                        this.open_tab(PaneKind::Agent, cwd.clone(), None, Some(path), window, cx);
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Sessions(view), subscription));
        cx.notify();
    }

    pub(super) fn open_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_active_session(window, cx);
        let cwd = self.active_cwd(cx);
        let models = self
            .active_session()
            .map(|t| t.read(cx).model_choices())
            .unwrap_or_default()
            .into_iter()
            .map(|(provider, id, name, reasoning)| ModelChoice {
                provider,
                id,
                name,
                reasoning,
            })
            .collect();
        let session = self.active_session();
        let view = cx.new(|cx| ProfilesView::new(cwd, models, session, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &ProfilesEvent, window, cx| {
            let tab = this.active_session();
            match event {
                ProfilesEvent::Close => this.close_overlay(window, cx),
                ProfilesEvent::Changed => {}
                ProfilesEvent::RunCommand(text) => {
                    if let Some(tab) = tab {
                        let text = text.clone();
                        tab.update(cx, |s, cx| s.run_prompt(text, cx));
                    }
                }
                ProfilesEvent::SwitchOrchestrator { model, thinking } => {
                    if let Some(tab) = tab {
                        let ((provider, id), thinking) = (model.clone(), thinking.clone());
                        tab.update(cx, |s, cx| {
                            s.send_command(serde_json::json!({"type": "set_model", "provider": provider, "modelId": id}), cx);
                            if let Some(level) = thinking {
                                s.send_command(serde_json::json!({"type": "set_thinking_level", "level": level}), cx);
                            }
                        });
                    }
                }
                ProfilesEvent::UseInSession { profile, model, thinking } => {
                    if let Some(tab) = tab {
                        let (model, thinking) = (model.clone(), thinking.clone());
                        let note = format!("Profile \u{201c}{profile}\u{201d} applied to this session's orchestrator; helpers keep the routing this session started with.");
                        tab.update(cx, |s, cx| {
                            s.note(note, cx);
                            if let Some((provider, id)) = model {
                                s.send_command(serde_json::json!({"type": "set_model", "provider": provider, "modelId": id}), cx);
                            }
                            if let Some(level) = thinking {
                                s.send_command(serde_json::json!({"type": "set_thinking_level", "level": level}), cx);
                            }
                        });
                    }
                }
            }
        });
        self.overlay = Some((Overlay::Profiles(view), subscription));
        cx.notify();
    }

    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_active_session(window, cx);
        let cwd = self.active_cwd(cx);
        let view = cx.new(|cx| SettingsView::new(cwd, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Close => this.close_overlay(window, cx),
                SettingsEvent::Changed => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.reload_models(window, cx));
                    }
                }
                SettingsEvent::DevicesChanged(prefs) => {
                    let prefs = *prefs;
                    crate::features::devices::hub(cx)
                        .update(cx, |hub, cx| hub.set_prefs(prefs, cx));
                    pi_rpc::set_system_prompt(crate::features::devices::agent_instructions(prefs));
                    if !prefs.enabled {
                        this.devices.update(cx, |panel, cx| panel.hide(cx));
                    }
                    cx.notify();
                }
                SettingsEvent::MusicChanged => {
                    this.music.update(cx, |music, cx| music.refresh(cx));
                }
                SettingsEvent::BackendUpdated => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.restart_for_update(window, cx));
                    }
                }
                SettingsEvent::Setup(event) => {
                    let usable = view.read(cx).setup_usable(cx);
                    this.on_setup_event(event, usable, window, cx);
                }
                SettingsEvent::ShellCommand(text) => {
                    let view = view.downgrade();
                    let report = move |result: Result<(), String>, cx: &mut App| {
                        let _ = view.update(cx, |view, cx| view.shell_done(result, cx));
                    };
                    match this.active_session() {
                        Some(session) => session.update(cx, |session, cx| {
                            session.run_extension_command(text.clone(), cx, report)
                        }),
                        None => report(
                            Err(
                                "Open an agent tab first: Gentle Shell applies this setting."
                                    .into(),
                            ),
                            cx,
                        ),
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Settings(view), subscription));
        cx.notify();
    }

    pub(super) fn open_memory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        let view = cx.new(|cx| MemoryView::new(cwd, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &MemoryEvent, window, cx| match event {
                MemoryEvent::Close => this.close_overlay(window, cx),
            },
        );
        self.overlay = Some((Overlay::Memory(view), subscription));
        cx.notify();
    }

    pub(super) fn open_fleet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        if let Err(error) = crate::features::orchestrator::sidecar::ensure_daemon(&cwd) {
            eprintln!("orchestrator: {error}");
        }
        let view = cx.new(|cx| FleetView::new(window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &FleetEvent, window, cx| match event {
                FleetEvent::Close => this.close_overlay(window, cx),
            },
        );
        self.overlay = Some((Overlay::Fleet(view), subscription));
        cx.notify();
    }

    pub(super) fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The Subscription tab reads through the active agent's RPC slice, so
        // its backend must be started (probe included) before it is opened.
        self.start_active_session(window, cx);
        let session = self.active_session();
        let view = cx.new(|cx| UsageView::new(window, cx, session));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &UsageEvent, window, cx| match event {
                UsageEvent::Close => this.close_overlay(window, cx),
            },
        );
        self.overlay = Some((Overlay::Usage(view), subscription));
        cx.notify();
    }

    pub(super) fn open_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        let view = cx.new(|cx| ProvidersView::new(cwd, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ProvidersEvent, window, cx| match event {
                ProvidersEvent::Close => {
                    this.close_overlay(window, cx);
                    // Signed in from the welcome screen: back to its sequence.
                    if let Some(view) = this.resume_welcome.take() {
                        this.attach_welcome(view, window, cx);
                    }
                }
                ProvidersEvent::Changed => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.reload_models(window, cx));
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Providers(view), subscription));
        cx.notify();
    }

    /// The onboarding screen; `probe` is a look at the machine already taken.
    pub(super) fn open_onboarding(
        &mut self,
        probe: Option<Probe>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| OnboardingView::new(probe, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &OnboardingEvent, window, cx| {
                let usable = view.read(cx).usable();
                this.on_setup_event(event, usable, window, cx);
            },
        );
        self.overlay = Some((Overlay::Onboarding(view), subscription));
        cx.notify();
    }

    /// The first-run welcome screen; `probe` is a look at the machine already
    /// taken. ⌘K → "Welcome screen" shows it again.
    pub(super) fn open_welcome(
        &mut self,
        probe: Option<Probe>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resume_welcome = None;
        let view = cx.new(|cx| WelcomeView::new(probe, cx));
        self.attach_welcome(view, window, cx);
    }

    /// Show `view` (new, or back from Providers) and route its events.
    pub(super) fn attach_welcome(
        &mut self,
        view: Entity<WelcomeView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &WelcomeEvent, window, cx| match event {
                WelcomeEvent::Close => this.close_overlay(window, cx),
                // Providers takes the window; closing it returns to the welcome.
                WelcomeEvent::Setup(OnboardingEvent::SignIn) => {
                    this.resume_welcome = Some(view.clone());
                    this.close_overlay(window, cx);
                    this.open_providers(window, cx);
                }
                WelcomeEvent::Setup(event) => {
                    let usable = view.read(cx).usable(cx);
                    this.on_setup_event(event, usable, window, cx);
                }
            },
        );
        view.read(cx).focus_handle().clone().focus(window, cx);
        self.overlay = Some((Overlay::Welcome(view), subscription));
        cx.notify();
    }

    /// An action from the setup checklist, on the onboarding screen or in
    /// Configuration (which is closed first, like the screen, when the action
    /// moves elsewhere). `usable`: gentle-shell can start as of its last look.
    pub(super) fn on_setup_event(
        &mut self,
        event: &OnboardingEvent,
        usable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            OnboardingEvent::Close => self.close_overlay(window, cx),
            OnboardingEvent::Installed => {
                if usable {
                    self.retry_failed_sessions(window, cx);
                }
            }
            OnboardingEvent::SignIn => {
                self.close_overlay(window, cx);
                self.open_providers(window, cx);
            }
            OnboardingEvent::Terminal(command) => {
                self.close_overlay(window, cx);
                self.open_setup_terminal(command.clone(), window, cx);
            }
        }
    }

    /// A terminal tab with `command` typed at the prompt for the user to run
    /// (it may ask for a password); the clipboard holds it when the terminal
    /// can't take it.
    pub(super) fn open_setup_terminal(
        &mut self,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = dirs::home_dir().unwrap_or_else(|| self.active_cwd(cx));
        let Some(pane) = self.new_pane(PaneKind::Terminal, home, None, None, window, cx) else {
            cx.write_to_clipboard(ClipboardItem::new_string(command));
            window.push_notification(
                Notification::info("The setup command is on your clipboard: run it in a terminal."),
                cx,
            );
            return;
        };
        self.push_pane_tab(pane, window, cx);
        let terminal = self.panes.get(&pane).and_then(|p| {
            p.item
                .view
                .clone()
                .downcast::<crate::features::terminal::TerminalView>()
                .ok()
        });
        let note = match terminal {
            Some(terminal) => {
                terminal.update(cx, |terminal, cx| terminal.type_when_ready(command, cx));
                "The command is typed in this terminal: press Enter to run it. Reopen Setup & requirements from the command palette when it's done."
            }
            None => {
                cx.write_to_clipboard(ClipboardItem::new_string(command));
                "The setup command is on your clipboard: paste it here and press Enter. Reopen Setup & requirements from the command palette when it's done."
            }
        };
        window.push_notification(Notification::info(note), cx);
    }

    /// Start again every agent tab whose backend could not start (onboarding
    /// just made gentle-shell available).
    pub(super) fn retry_failed_sessions(&self, window: &mut Window, cx: &mut Context<Self>) {
        for tab in self.sessions() {
            tab.update(cx, |session, cx| session.retry_start(window, cx));
        }
    }
}
