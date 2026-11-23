//! Composition root: starts GPUI, installs themes, key bindings and menus,
//! and opens the window around the workspace.

pub mod integration;

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::*;

use crate::features::workspace::{
    ClosePane, CloseTab, FocusNextPane, FocusPrevPane, NewTab, NewTerminalTab, NextTab, PrevTab,
    Quit, SplitDown, SplitRight, TilePane, ToggleSidebar, Workspace,
};
use crate::{
    features::app_update, features::files, features::session, features::workspace,
    platform::instance, shared::assets, shared::themes,
};

/// Runs the app until its last window closes. `requests` carries later
/// launches' folders and prompts to this window.
pub fn run(
    folder: Option<PathBuf>,
    prompt: Option<String>,
    url: Option<String>,
    requests: async_channel::Receiver<instance::OpenRequest>,
) {
    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            themes::init(cx);
            #[cfg(target_os = "macos")]
            crate::platform::icons::set_dock_icon();
            app_update::init(cx);

            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            set_menus(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                window_min_size: Some(size(px(720.), px(480.))),
                window_background: WindowBackgroundAppearance::Blurred,
                titlebar: Some(TitlebarOptions {
                    title: Some("Gentle Shell".into()),
                    appears_transparent: true,
                    // Centered in the 36 px title bar (workspace.rs).
                    traffic_light_position: Some(point(px(14.), px(11.))),
                }),
                app_id: Some("dev.gentle-shell.gpui".into()),
                #[cfg(target_os = "linux")]
                icon: crate::platform::icons::window_icon(),
                ..TitleBar::window_options()
            };
            integration::register(cx);
            let (handle, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
                let workspace = cx.new(|cx| {
                    Workspace::new(folder.clone(), prompt.clone(), url.clone(), window, cx)
                });
                integration::attach_sidebar(&workspace, window, cx);
                workspace
            })
            .expect("failed to open the Gentle Shell window");
            // Requests from later launches: bring this window forward and act on them.
            // Weak, so quitting releases the workspace (and stops its tabs' backends).
            let workspace = workspace.downgrade();
            cx.spawn(async move |cx| {
                while let Ok(request) = requests.recv().await {
                    // An agent's `gentle-device` doesn't pull the window forward.
                    let activate = request.device.is_none();
                    let _ = handle.update(cx, |_, window, cx| {
                        if activate {
                            window.activate_window();
                        }
                        let _ = workspace.update(cx, |w, cx| w.open_request(request, window, cx));
                    });
                    if activate {
                        cx.update(|cx| cx.activate(true));
                    }
                }
            })
            .detach();
            cx.activate(true);
        });
}

fn bind_keys(cx: &mut App) {
    // Cmd on macOS, Ctrl elsewhere.
    macro_rules! key {
        ($k:literal) => {
            if cfg!(target_os = "macos") {
                concat!("cmd-", $k)
            } else {
                concat!("ctrl-", $k)
            }
        };
    }
    // A new tiled pane is ctrl-shift-d off macOS: ctrl-d belongs to terminals.
    let (tile, split_down, workspaces) = if cfg!(target_os = "macos") {
        ("cmd-d", "cmd-shift-d", "ctrl-cmd-k")
    } else {
        ("ctrl-shift-d", "ctrl-shift-e", "ctrl-alt-k")
    };
    cx.bind_keys([
        KeyBinding::new(key!("t"), NewTab, None),
        KeyBinding::new(key!("shift-t"), NewTerminalTab, None),
        KeyBinding::new(key!("w"), ClosePane, None),
        KeyBinding::new(key!("shift-w"), CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new(tile, TilePane, None),
        KeyBinding::new(split_down, SplitDown, None),
        KeyBinding::new(key!("]"), FocusNextPane, None),
        KeyBinding::new(key!("["), FocusPrevPane, None),
        KeyBinding::new(key!("b"), ToggleSidebar, None),
        KeyBinding::new(key!("q"), Quit, None),
        KeyBinding::new("escape", workspace::CloseOverlay, Some("GentleWorkspace")),
        KeyBinding::new("escape", session::Abort, Some(session::CONTEXT)),
        KeyBinding::new(key!("."), session::Stop, Some(session::CONTEXT)),
        KeyBinding::new(
            key!("shift-a"),
            session::ToggleAgents,
            Some(session::CONTEXT),
        ),
        KeyBinding::new(key!("n"), session::NewSession, Some(session::CONTEXT)),
        KeyBinding::new(key!("k"), workspace::OpenPalette, None),
        KeyBinding::new(key!("o"), workspace::OpenExplorer, None),
        KeyBinding::new(workspaces, workspace::OpenWorkspaces, None),
        KeyBinding::new(key!("shift-o"), workspace::OpenSessions, None),
    ]);
    files::FileView::bind_keys(cx);
}

fn set_menus(cx: &mut App) {
    cx.set_menus(vec![Menu {
        name: "Gentle Shell".into(),
        items: vec![
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("New Terminal Tab", NewTerminalTab),
            MenuItem::action("New Pane (Tiled)", TilePane),
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::action("Close Pane", ClosePane),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::separator(),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::separator(),
            MenuItem::action("Quit Gentle Shell", Quit),
        ],
        disabled: false,
    }]);
}
