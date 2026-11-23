use std::path::PathBuf;

use super::store::{load_from, save_to};
use super::{SavedPane, SavedState, SavedTab, SavedWorkspace, VERSION};
use crate::shared::pane::{PaneKind, PaneTree, SplitAxis};

fn sample() -> SavedState {
    let agent = SavedPane {
        kind: PaneKind::Agent,
        cwd: PathBuf::from("/tmp/project"),
        path: None,
        session: Some(PathBuf::from("/tmp/sessions/a.jsonl")),
    };
    let terminal = SavedPane {
        kind: PaneKind::Terminal,
        cwd: PathBuf::from("/tmp/project"),
        path: None,
        session: None,
    };
    let file = SavedPane {
        kind: PaneKind::File,
        cwd: PathBuf::from("/tmp/project"),
        path: Some(PathBuf::from("/tmp/project/README.md")),
        session: None,
    };
    SavedState {
        version: VERSION,
        current: 1,
        workspaces: vec![
            SavedWorkspace {
                name: "Default".into(),
                root: PathBuf::from("/tmp"),
                tabs: vec![SavedTab {
                    layout: PaneTree::Leaf(agent.clone()),
                    focused: 0,
                }],
                active_tab: 0,
                last_active: 10,
            },
            SavedWorkspace {
                name: "project".into(),
                root: PathBuf::from("/tmp/project"),
                tabs: vec![SavedTab {
                    layout: PaneTree::Split {
                        axis: SplitAxis::Horizontal,
                        ratio: 0.4,
                        first: Box::new(PaneTree::Leaf(agent)),
                        second: Box::new(PaneTree::Split {
                            axis: SplitAxis::Vertical,
                            ratio: 0.5,
                            first: Box::new(PaneTree::Leaf(terminal)),
                            second: Box::new(PaneTree::Leaf(file)),
                        }),
                    },
                    focused: 2,
                }],
                active_tab: 0,
                last_active: 20,
            },
        ],
        sidebar: false,
    }
}

#[test]
fn json_round_trip() {
    let state = sample();
    let json = serde_json::to_string(&state).unwrap();
    let back: SavedState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
    // Optional fields stay out of the file and default back in.
    assert!(!json.contains("\"path\":null"));
    let minimal: SavedState =
        serde_json::from_str(r#"{"workspaces":[{"name":"x","root":"/x"}]}"#).unwrap();
    assert_eq!(minimal.workspaces[0].tabs.len(), 0);
    assert_eq!(minimal.current, 0);
    // A file from before the sidebar was saved keeps it showing.
    assert!(minimal.sidebar);
    assert_eq!(SavedState::default().sidebar, minimal.sidebar);
}

#[test]
fn file_round_trip() {
    let dir = std::env::temp_dir().join(format!("gentle-ws-test-{}", std::process::id()));
    let path = dir.join("nested").join("workspaces.json");
    let state = sample();
    save_to(&path, &state).unwrap();
    assert_eq!(load_from(&path), Some(state));
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(load_from(&path), None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_folder_reached_another_way_keys_the_same_workspace() {
    let base = std::env::temp_dir().join(format!("ws-key-test-{}", std::process::id()));
    std::fs::create_dir_all(base.join("a")).unwrap();
    let canonical = dunce::canonicalize(base.join("a")).unwrap();
    // The explorer builds `dir.join(name)`; `gsd` passes a canonical path.
    assert_eq!(
        super::root_key(base.join("a").join("..").join("a")),
        canonical
    );
    assert_eq!(super::root_key(canonical.clone()), canonical);
    // A folder that can't be resolved keeps its path.
    let missing = base.join("missing");
    assert_eq!(super::root_key(missing.clone()), missing);
    std::fs::remove_dir_all(&base).unwrap();
}
