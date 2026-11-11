//! The switcher (⌘K), after Tern's: one search field over segmented
//! categories — the app's screens and pane commands plus every Gentle Shell
//! slash command the backend reports, the window's workspaces (switch,
//! rename, delete, create), files under the workspace root and git branches.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use gentle_core::commands::Screen;
use gentle_core::fuzzy::{FileEntry, Needle};

mod commands;
mod keyboard;
mod rows;
mod sources;
mod view;

use commands::{command_row, screen_rows};
pub use sources::switch_branch;
use sources::{list_branches, walk_files};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    Providers,
    Profiles,
    Sessions,
    Usage,
    Memory,
    Fleet,
    Devices,
    Settings,
    /// The onboarding screen: dependency checks and installs.
    Setup,
    /// The first-run welcome screen, from its intro.
    Welcome,
    Agents,
    NewSession,
    /// A new tab with an agent pane.
    NewTab,
    NewTerminalTab,
    /// A new pane of the focused kind, tiled Hyprland-style.
    TilePane,
    SplitRight,
    SplitDown,
    /// Split the focused pane with an agent / terminal beside it.
    NewAgentPane,
    NewTerminalPane,
    ClosePane,
    ToggleSidebar,
    /// Send `/name` as a prompt in the active agent pane.
    Command(String),
    /// A Gentle Shell command whose UI is terminal-only, opened natively.
    Screen(Screen),
    SwitchWorkspace(usize),
    RenameWorkspace {
        index: usize,
        name: String,
    },
    DeleteWorkspace(usize),
    /// A new workspace named `name`, rooted at the current folder or, with
    /// `pick_folder`, at one the user chooses (named after it when `name` is empty).
    CreateWorkspace {
        name: String,
        pick_folder: bool,
    },
    OpenFile(PathBuf),
    /// Asks for confirmation before running `git switch`.
    SwitchBranch(String),
}

pub enum PaletteEvent {
    Run(PaletteAction),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    All,
    Commands,
    Workspaces,
    Files,
    Branches,
}

const CATEGORIES: [(Category, &str); 5] = [
    (Category::All, "All"),
    (Category::Commands, "Commands"),
    (Category::Workspaces, "Workspaces"),
    (Category::Files, "Files"),
    (Category::Branches, "Branches"),
];

impl Category {
    fn placeholder(self) -> &'static str {
        match self {
            Category::All => "Search commands, workspaces, files…",
            Category::Commands => "Type a command or screen…",
            Category::Workspaces => "Find or create a workspace…",
            Category::Files => "Find a file…",
            Category::Branches => "Find a branch…",
        }
    }
}

/// A workspace as the switcher lists it.
#[derive(Clone, Debug)]
pub struct WorkspaceRow {
    /// Index in the window's workspace list.
    pub index: usize,
    pub name: String,
    pub tabs: usize,
    pub current: bool,
}

pub struct PaletteInput {
    /// Slash commands of the active agent pane: `(name, description)`.
    pub commands: Vec<(String, String)>,
    pub workspaces: Vec<WorkspaceRow>,
    /// The current workspace's root, for files and branches.
    pub root: PathBuf,
    pub category: Category,
}

#[derive(Clone)]
struct Row {
    icon: IconName,
    title: String,
    meta: String,
    detail: String,
    group: &'static str,
    action: PaletteAction,
    /// Workspace rows get rename/delete buttons.
    workspace: Option<usize>,
    current: bool,
}

enum Line {
    Header(&'static str),
    Row(usize),
}

const MAX_FILES: usize = 20_000;

pub struct PaletteView {
    query: Entity<InputState>,
    category: Category,
    selected: usize,
    commands: Vec<Row>,
    workspaces: Vec<WorkspaceRow>,
    root: PathBuf,
    files: Option<Vec<FileEntry>>,
    branches: Option<Vec<(String, bool)>>,
    renaming: Option<(usize, Entity<InputState>)>,
    /// The query as last matched.
    needle: Needle,
    /// Matched rows and their display lines, recomputed by
    /// [`Self::refresh_rows`] when the query, category or sources change.
    rows: Vec<Row>,
    lines: Vec<Line>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl PaletteView {
    pub fn new(input: PaletteInput, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder(input.category.placeholder()));
        query.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.selected = 0;
                this.scroll.scroll_to_item(0);
                this.refresh_rows(cx);
                cx.notify();
            }
        });
        let mut commands = screen_rows();
        commands.extend(input.commands.into_iter().filter_map(command_row));

        let root = input.root.clone();
        let files = cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    walk_files(&root, MAX_FILES)
                        .into_iter()
                        .map(FileEntry::new)
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.files = Some(files);
                this.refresh_rows(cx);
                cx.notify();
            });
        });
        let root = input.root.clone();
        let branches = cx.spawn(async move |this, cx| {
            let branches = cx
                .background_executor()
                .spawn(async move { list_branches(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.branches = Some(branches);
                this.refresh_rows(cx);
                cx.notify();
            });
        });

        let mut this = Self {
            query,
            category: input.category,
            selected: 0,
            commands,
            workspaces: input.workspaces,
            root: input.root,
            files: None,
            branches: None,
            renaming: None,
            needle: Needle::default(),
            rows: Vec::new(),
            lines: Vec::new(),
            scroll: ScrollHandle::new(),
            _subscriptions: vec![subscription],
            _tasks: vec![files, branches],
        };
        this.refresh_rows(cx);
        this
    }

    pub fn category(&self) -> Category {
        self.category
    }

    pub fn set_category(
        &mut self,
        category: Category,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.category = category;
        self.selected = 0;
        self.renaming = None;
        self.scroll.scroll_to_item(0);
        self.query.update(cx, |state, cx| {
            state.set_placeholder(category.placeholder(), window, cx);
            state.focus(window, cx);
        });
        self.refresh_rows(cx);
        cx.notify();
    }
}
