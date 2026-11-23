//! Keybinding action handlers.

use super::*;

impl Workspace {
    pub(super) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        self.open_tab(PaneKind::Agent, cwd, None, None, window, cx);
    }

    pub(super) fn on_new_terminal_tab(
        &mut self,
        _: &NewTerminalTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.active_cwd(cx);
        self.open_tab(PaneKind::Terminal, cwd, None, None, window, cx);
    }

    pub(super) fn on_close_tab(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.space().active;
        self.close_tab(active, window, cx);
    }

    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let space = self.space();
        let next = (space.active + 1) % space.tabs.len().max(1);
        self.activate(next, window, cx);
    }

    pub(super) fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let space = self.space();
        let len = space.tabs.len().max(1);
        let prev = (space.active + len - 1) % len;
        self.activate(prev, window, cx);
    }

    pub(super) fn on_split_right(
        &mut self,
        _: &SplitRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split(SplitAxis::Horizontal, None, window, cx);
    }

    /// ⌘D: a new pane tiled Hyprland-style (dwindle), splitting the focused
    /// pane along its longer side.
    pub(super) fn on_tile_pane(
        &mut self,
        _: &TilePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tile(None, window, cx);
    }

    pub(super) fn on_split_down(
        &mut self,
        _: &SplitDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split(SplitAxis::Vertical, None, window, cx);
    }

    pub(super) fn on_new_agent_pane(
        &mut self,
        _: &NewAgentPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_with_kind(PaneKind::Agent, window, cx);
    }

    pub(super) fn on_new_terminal_pane(
        &mut self,
        _: &NewTerminalPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_with_kind(PaneKind::Terminal, window, cx);
    }

    pub(super) fn on_close_pane(
        &mut self,
        _: &ClosePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.focused_pane() {
            self.close_pane(pane, window, cx);
        }
    }

    pub(super) fn on_focus_next_pane(
        &mut self,
        _: &FocusNextPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(next) = self.tab().and_then(|t| t.tree.cycle(&t.focused, true)) {
            self.focus_pane(next, window, cx);
        }
    }

    pub(super) fn on_focus_prev_pane(
        &mut self,
        _: &FocusPrevPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(prev) = self.tab().and_then(|t| t.tree.cycle(&t.focused, false)) {
            self.focus_pane(prev, window, cx);
        }
    }

    pub(super) fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar(cx);
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.emit(WorkspaceEvent::SidebarToggled(self.sidebar_visible));
        self.schedule_save(cx);
        cx.notify();
    }

    pub fn sidebar_visible(&self) -> bool {
        self.sidebar_visible
    }

    pub(super) fn on_open_providers(
        &mut self,
        _: &OpenProviders,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_providers(window, cx);
    }

    pub(super) fn on_close_overlay(
        &mut self,
        _: &CloseOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &self.overlay {
            // An install is running: the screen stays until it ends.
            Some((Overlay::Onboarding(view), _)) if view.read(cx).busy() => {}
            Some((Overlay::Welcome(view), _)) if view.read(cx).busy(cx) => {}
            Some(_) => self.close_overlay(window, cx),
            None => cx.propagate(),
        }
    }

    pub(super) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let retry = match &self.overlay {
            Some((Overlay::Onboarding(view), _)) => view.read(cx).usable(),
            Some((Overlay::Welcome(view), _)) => view.read(cx).usable(cx),
            _ => false,
        };
        self.overlay = None;
        if retry {
            self.retry_failed_sessions(window, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn on_open_sessions(
        &mut self,
        _: &OpenSessions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_sessions(window, cx);
    }

    pub(super) fn on_open_palette(
        &mut self,
        _: &OpenPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(Category::All, window, cx);
    }

    pub(super) fn on_open_explorer(
        &mut self,
        _: &OpenExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_explorer(window, cx);
    }

    pub(super) fn on_open_workspaces(
        &mut self,
        _: &OpenWorkspaces,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(Category::Workspaces, window, cx);
    }
}
