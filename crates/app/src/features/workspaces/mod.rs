//! Named workspaces (Tern calls them sessions): each has a root folder and its
//! own tabs. The list, with every tab's pane layout, is kept as JSON in
//! `<data dir>/gentle-shell-gpui/workspaces.json` and restored on launch.
//!
//! This module holds the serialized model; `store` reads and writes the file.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::shared::pane::{PaneKind, PaneTree};

mod store;
#[cfg(test)]
mod tests;

pub use gentle_core::workspaces::unique_name;
pub use store::{load, save};

/// One pane as saved: enough to build it again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedPane {
    pub kind: PaneKind,
    pub cwd: PathBuf,
    /// The file a file pane shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The conversation an agent pane had, resumed when it still exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub layout: PaneTree<SavedPane>,
    /// Index of the focused pane among the layout's leaves.
    #[serde(default)]
    pub focused: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub name: String,
    pub root: PathBuf,
    #[serde(default)]
    pub tabs: Vec<SavedTab>,
    #[serde(default)]
    pub active_tab: usize,
    /// Unix seconds.
    #[serde(default)]
    pub last_active: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedState {
    #[serde(default)]
    pub version: u32,
    /// Index of the workspace the window showed last.
    #[serde(default)]
    pub current: usize,
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
    /// The files sidebar (⌘B) was showing.
    #[serde(default = "shown")]
    pub sidebar: bool,
}

fn shown() -> bool {
    true
}

/// A first launch shows the sidebar, as a file from before it was saved does.
impl Default for SavedState {
    fn default() -> Self {
        Self {
            version: 0,
            current: 0,
            workspaces: Vec::new(),
            sidebar: true,
        }
    }
}

pub const VERSION: u32 = 1;

/// How a workspace root is compared: canonical, so a folder reached through
/// a symlink, `..` or another drive-letter case finds the same workspace as
/// `gsd` (which passes canonical paths). Unresolvable paths stay as given.
pub fn root_key(path: PathBuf) -> PathBuf {
    dunce::canonicalize(&path).unwrap_or(path)
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
