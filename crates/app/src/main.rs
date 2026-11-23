#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cli;
mod features;
mod platform;
mod shared;

use features::setup;
use platform::instance;

fn main() {
    // `gentle-device …` (the agents' command) runs here and exits.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--device") {
        std::process::exit(features::devices::cli::run(args.collect()));
    }
    // An explicit folder (or `--cwd`, which `gsd` passes) selects or creates
    // the workspace rooted there; without one the last workspace is restored.
    let cli = cli::parse_cli();
    let folder = cli::requested_folder(&cli);
    let prompt = cli.prompt;
    let url = cli.url;
    if let Some(url) = url.as_deref()
        && (url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")))
    {
        eprintln!("gentle-shell-gpui: --open-url expects an http(s) URL");
        std::process::exit(2);
    }
    // Only one app per user: a second launch hands its request over and exits.
    let request = instance::OpenRequest {
        folder: folder.clone(),
        prompt: prompt.clone(),
        url: url.clone(),
        device: None,
    };
    let requests = match instance::claim(&request) {
        instance::Role::Forwarded => return,
        instance::Role::Primary(requests) => requests,
    };
    // `gentle-open` and `gentle-device` live in the toolchain folder the
    // agents get on PATH; create them before that PATH is computed.
    platform::shims::install();
    platform::gsd::refresh();
    // What every agent is told about opening pages and devices in the app.
    pi_rpc::set_system_prompt(features::devices::agent_instructions(
        setup::devices::prefs(),
    ));
    // Toolchains onboarding installed into the app's own folders.
    setup::apply_toolchain_path();
    // Gentle Shell 4.7's own install moved its config folder without copying
    // the old one: bring profiles, routing and preferences along once.
    match pi_rpc::adopt_standalone_config() {
        Ok(copied) if !copied.is_empty() => {
            eprintln!(
                "Copied {} Gentle Shell settings to its new config folder",
                copied.len()
            )
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("Could not copy Gentle Shell's settings to its new folder: {error}")
        }
    }
    app::run(folder, prompt, url, requests);
}
