# gentle-shell-gpui

A native desktop app for [Gentle Shell](https://github.com/Gentleman-Programming/gentle-shell), built in Rust with [GPUI](https://gpui.rs) and [GPUI Kit](https://github.com/longbridge/gpui-kit). Use Gentle Shell from a window instead of a terminal. The layout follows [Tern](https://stencil.so/tern): translucent glass chrome, tabs in the title bar, a centered composer, and tool calls shown as cards.

The app does not reimplement Gentle Shell. Each tab starts `gentle-shell --mode rpc` and talks to it over Pi's [RPC protocol](https://pi.dev/docs/latest/rpc), so el Gentleman, ODD, Gentle Agents, permissions, Engram memory, review, skills and every extension run unchanged. The app sets `GENTLE_SHELL_INTERACTIVE_HOST=1`, which enables `ask_user_question` dialogs and the live helper activity feed. Gentle Shell's own screens are terminal-UI components that never reach an RPC host, so the app rebuilds them natively from the same files and events.

## Features

| Area | What the app does |
| --- | --- |
| Workspaces and tiling | Named workspaces (each with its own folder and tabs), saved and restored; agents in other workspaces keep running while you switch. Tabs split into panes: agent, terminal or file. Panes tile like Hyprland's dwindle: ⌘D adds a pane by splitting the focused one along its longer side, so panes spiral in instead of lining up in one row (a pane too small to halve opens the new one as a tab). Split right and split down (⌘⇧D) stay in the palette and menu. Panes have no header: new pane, split down and close pane (⌘W) sit in the title bar and act on the focused pane. ⌘] / ⌘[ move focus. Drag a divider to resize. Drag a pane's grip onto another pane: near an edge it moves to that side (the zone lights up), in the middle the two swap. ⌃⌘K switches workspace. The sidebar remembers whether it was open. |
| `gsd` from a terminal | `gsd` opens the folder you're in as a workspace, like `gs` opens Gentle Shell there; `gsd <folder>` opens that folder. With the app already running it switches the window to that workspace (creating it, with an agent tab) instead of starting a second app, and returns to your prompt at once. The Windows installer puts `gsd` on your user PATH (the uninstaller takes it off); elsewhere, or for a build from source, ⌘K → Install `gsd` command writes it to `~/.local/bin` (macOS, Linux) or adds it to your user PATH (Windows). On Windows `gsd` is a `.cmd`: it runs from cmd and PowerShell, not Git Bash. The PATH entry the palette adds stays until you remove it. Launching the app itself (Start menu, Finder, Dock) still restores the last workspace. |
| Folder explorer (⌘O) | An oil.nvim-style explorer to open Gentle Shell anywhere without leaving the keyboard. It starts at the focused pane's folder and lists one folder at a time, folders first (git repositories marked `repo`): ↵ or → enters a folder (↵ on a file opens it), `-`, ← or ⌫ goes to the parent and selects the folder you came from, `~` goes home, and typing filters (start with `.` for dotfiles). Click a segment of the path to jump there. ⌘↵ (Ctrl+↵ elsewhere) opens the selected folder (or the one shown, when a file is selected) as a workspace with an agent tab, the same as `gsd <folder>`, and **Open Gentle Shell here** opens the folder shown; ⌥↵ (Alt+↵) also opens a terminal tab there. Also in ⌘K and the workspace switcher as Browse folders. |
| Terminal panes | Your login shell in a real PTY (alacritty_terminal) with the Gentleman colors; ⌘⇧T opens a terminal tab. |
| Files sidebar (⌘B) | Files, Changes and Branches tabs. The tree respects `.gitignore` (⋯ → Show Ignored Files lists the rest dimmed), navigates with the arrow keys, and has New File / New Folder buttons with inline naming. Right-click a row for Open, Open to the Side (⌘↵), Open in New Tab (⌥⌘↵), Open with Default App, Insert Path into the agent prompt (⌥↵), Copy Path (⌥⌘C) / Relative Path (⌥⇧⌘C), Reveal in Finder (⌥⌘R), Rename (F2), Duplicate, Cut / Copy / Paste (⌘X / ⌘C / ⌘V) and Move to Trash (⌘⌫). Files open in a syntax-highlighted editor: ⌘S saves atomically, a dot marks unsaved edits, outside writes reload a clean buffer and ask (Reload / Keep mine) when you have unsaved edits, and closing a pane or tab with unsaved edits asks first. Markdown files open rendered, with an Edit · Split · Preview toggle in the file header; the preview follows unsaved edits live (relative images load from the file's folder) and the last choice is remembered for the session. Changes open as a read-only diff against HEAD. **Changes** (after Tern): Unstaged and Staged lists (or by folder) with a checkbox per file, Stage all / Unstage all, stash, discard on right-click, and a commit box (summary, description, Amend, Commit / Commit & Push with ⌘↵); "Generate message" asks your Gentle Shell (`--print --no-session --no-tools`) for a Conventional Commits message from the staged diff. **Branches**: filterable branches, remotes, tags, stashes (apply / pop / drop), worktrees and submodules; click a branch to check it out; the footer shows upstream and ↑ahead ↓behind. |
| First run | On first launch, or whenever something required is missing, a setup screen checks what the app needs on this system: Node.js ≥ 22.19 with npm, Go on Windows (gentle-pi builds Gentle AI with it), Gentle Shell (`gentle-pi`, with Pi inside), its one-time setup, and a signed-in provider; then Git, and optionally Engram (memory), the Claude bridge and its Claude Code login. What can be installed without your password is installed in place with a live log: Node 22 LTS from nodejs.org (checksum-verified) into the app's own folder when yours is missing or too old, gentle-pi into a user-owned npm prefix when the global one needs root, and Gentle Shell's setup itself. What needs `sudo` or an interactive login opens in a terminal tab with the command typed. Nothing touches your system Node or shell profile: the app adds its folders to the PATH it gives Gentle Shell. Install everything runs the missing steps in order; tabs that couldn't start retry once it's done. Reopen it any time from ⌘K → Setup & requirements or Configuration → Gentle Shell. |
| Start screen | Gentle Shell's rose and pixel wordmark, written in like the terminal intro. It follows `banner.json` (`showRose`, `showTextLogo`, `color`). |
| Conversation | Streaming Markdown, collapsible thinking, and tool cards (`Edit path +12 −3`, `Bash …`) with live output. Helper answers (`gentle-agents.result`) appear as Markdown cards titled with the agent and its status; background job notices show their label, exit code and output tail; Gentle Shell's review preflight, exploration budget and verification reminders get named cards. A slash command's text output appears as one result card. Hover an answer, or a message you sent, for Copy. When the conversation is longer than the pane, a rail on the right edge marks every message you sent, placed by the heights it has seen (rows not shown yet count as the average), and shows a scroll thumb while you scroll or point at it (the one you are reading highlighted, its first line on hover); click a mark to glide to it, and a ↓ button glides back to the latest output. Both hide when everything fits. |
| Composer | Enter sends; while the agent runs, Enter steers it and ⌘. (Ctrl+. elsewhere) or the Stop button stops it. `!cmd` / `!!cmd` run shell commands. `/` lists commands (↑/↓ move through them, Tab or Enter fills one in, Esc closes the list), including Pi's built-ins: `/compact [what to keep]`, `/name <title>`, `/copy`, `/export [path]` (HTML) and `/reload` (restarts the backend and resumes the conversation). Click the context percentage to compact now or turn auto-compaction on or off. For reasoning models, the effort chip (the level's icon, the level and a pixel meter) opens a slider in Gentle Shell's rose gradient with one stop per level the model accepts (`get_available_thinking_levels`: off … high, and xhigh / max where supported): click, drag or pick a label; the card says what the level costs, and the icon follows the level (moon, feather, leaf, brain, circuit brain, bolt, flame). Choosing max plays a 4-second surge over the window: a pixel comet laps the window's edge, shedding sparks and drawing a shimmering outline; the edge flares; the window trembles harder and harder while energy spirals into a glowing core; then the core hatches with a flash, a shockwave and one last jolt, and Gentle Shell's pixel rose bursts out with a rebound and a shower of sparks. Shows model, effort, context %, cost, the ODD phase (`✿ exploring…`), extension statuses, queued messages and running jobs. |
| Attachments | Attach images with 📎, by pasting (screenshots or copied image files) or by dropping them on the composer. Each image becomes an `[Image #N]` token and a thumbnail, and the agent receives the images with the prompt. Models without image input and `!cmd` shell commands refuse attachments with a warning. |
| Notifications (🔔) | Extension notices go to a notification center instead of piling up as toasts; only warnings and errors still toast. The badge counts unread notices, repeats collapse into `×N`, and clicking a notice jumps to the pane that raised it. |
| Sounds | App tabs follow Gentle Shell's `notifications.json` (the file `/gentle:customize` edits in the terminal; Configuration › Notifications). Off by default. When on, a finished turn, a question for you, or context ≥ 90% plays Gentle Shell's tones (macOS `afplay`, Windows PowerShell `SoundPlayer`, Linux `paplay` / `pw-play` / `aplay` when installed, otherwise silent) and bounces the Dock or flashes the taskbar while the app is in the background. |
| Gentle Design | When Gentle Shell's design canvas opens (`/gentle:design`, or the model's `gentle_design` tool), it waits in the title bar's instance tray (see Instances); open it from there and it shows in a floating browser card, bottom-right by default: drag its header to move it, any edge or corner to resize it, double-click the header or use the button to maximize it. The page stays live while you resize, and the card lifts while you drag it. The canvas reloads itself as artboards change, so the card follows the design live; comments and Implement on the canvas go back to the conversation as usual. Reload, open in your browser and hide are in its header; it closes when the canvas stops. The page is the system's web view: WKWebView on macOS, WebView2 on Windows (in a popup that follows the window, since GPUI draws above child windows; rounded on Windows 11), WebKitGTK on Linux under X11 (on Wayland, which has no child windows, the card says so and the canvas stays in your browser). Gentle Shell also opens its own browser tab the first time, since it has no setting to skip that. The embedded page draws above the app's own menus, so it hides while a screen (palette, settings, …) is open. |
| Devices | iOS simulators and Android emulators inside the app, live and touchable, as tabs in a device panel docked at the right (or floating; a button switches). Configuration → Devices: **Use devices** installs Expo's device hub (the one T3 Code uses) into the app's folder; the panel's + lists every simulator and emulator and boots the one you pick headless, with its screen streamed live and touch, keyboard and hardware buttons. **Agents can use devices** gives new agent sessions `gentle-device` (open a device in the panel) and `agent-device` (launch apps, tap, type, take screenshots), and tells them to use only those, so a device never opens in a separate Simulator or emulator window. A device an agent starts with its own `simctl boot` or `emulator -avd` still opens in the panel. Every agent is also told to open web pages with `gentle-open`, in the app's browser. Devices the app booted are shut down when it quits. Needs Node 22.12+; iOS needs Apple silicon and Xcode; Android needs the SDK and, for emulator video, ffmpeg (the section offers the install command). |
| Web apps | The in-app browser is not only for Gentle Design. `gentle-open <url>` is a shim installed at startup on the PATH the agents receive, so the model can open any http(s) page from its shell tool; the app forwards it to the running window through the single-instance server and keeps it in the instance tray. A loopback dev-server URL printed by a shell tool (`http://localhost:5173`, `127.0.0.1`, `[::1]`) goes to the tray too. `/web <url>` from the composer shows the card at once. **Each different URL gets its own card**; the same URL reuses it. Web and device cards tile side by side (the hybrid web + mobile case), and the design canvas keeps its own card too. Public URLs need the explicit tool. |
| UI testing (Maestro) | Settings → UI testing · Maestro shows what is installed (CLI, Java, whether the agent has the tools) and does everything from there: Install (or Connect to the agent when Maestro is already there), Test connection (starts the server the way the agent would: `maestro: connected, 10 tools`), Disconnect, Remove (disconnects and deletes the Maestro and Java the app installed; your own installs stay), with a live log. Connecting or disconnecting reloads open tabs once their turn ends, so the agent picks the change up. The app puts a Java runtime (Temurin 21 JRE from Adoptium, only when there is no Java 17+) and the Maestro CLI (GitHub release, ~350 MB) in its own toolchain folder, both checksum-verified and without `sudo`, on macOS, Linux and Windows. Then it runs `gentle-shell mcp add maestro`, so the agent gets Maestro's tools (`mcp__maestro__*`) in new sessions (or after `/reload`): `list_devices`, `take_screenshot`, `inspect_screen` and `run` (Maestro commands and flows: launch apps, tap, type, swipe, assert), on Android emulators, iOS simulators and web pages (a Chromium Maestro manages; web support is beta in Maestro). The `maestro` command is also on the agent's shell PATH. |
| Music (♪) | Gentle Shell's `/gentle:music` player, gentle-stream, in the title bar: endless music composed live (lo-fi by default, plus ambient, jazz, synthwave, chiptune, rock and more). Configuration → Music installs it into the app's own folder (uv, Python 3.13 and the player, about 150 MB; sampled instruments are an optional 120 MB), or uses the one you already have. The ♪ button opens a small player: the piece and its key and tempo, a live rose spectrum, play/pause, next piece, volume, and a mood field ("rainy night in a small café", any language) that recomposes the music at the next bar. A mood is one call to Gentle Shell's default model (its tokens and cost show after). The music keeps playing with the player closed; quitting the app stops music the app started. macOS and Linux. |
| Instances | What agents open (dev-server pages, `gentle-open`, the design canvas, emulators and simulators they boot) never pops up over your work: a circle in the title bar counts them, its badge colored by the one that most needs a look (red error, gold booting or waiting, green ready). Click it for the list: each row shows the device or page, the agent tab that opened it, its status and, for devices, the last command the agent ran on it (`adb shell input tap …`, `simctl openurl …`, `maestro test …`). Click a row to open its card, the eye to show or hide it, ⏻ to stop a device or × to close a page; Hide all puts every card back. A hidden page has no web view (and no web process) until you open it, and loads afresh then. |
| Providers (🔑) | Sign in with a subscription (Claude Pro/Max, ChatGPT, Copilot, …) or an API key (DeepSeek, OpenAI, Gemini, …), or sign out. It runs Pi's own `ModelRuntime.login` through a small Node helper, and Pi writes `auth.json` as `/login` does. A running backend only reads credentials when it starts, so signing in or out restarts the open agent tabs and resumes their conversations (a tab mid-turn waits for the turn to end); the model picker then lists that provider's models. |
| Agents (⌘⇧A) | Gentle Agents helpers: status, time, turns, tools, task, last step, and the thread (Activity tab). The app keeps finished helpers after Gentle Shell drops them. "Ask to stop" asks el Gentleman to run `subagent_cancel`, because Gentle Shell has no host-side stop. Configuration → Background subagents (Off / On) runs Gentle Shell's own `/gentle:background-subagents enable|disable` in the active agent tab: with it on, helpers start in the background and their answers arrive when they finish. The row shows which source decides (project file, global file, `GENTLE_PI_BACKGROUND_SUBAGENTS`, default), in the shell's order. |
| Profiles | `/gentle:profiles` rebuilt: profiles, per-agent model, effort and Fast tier. Supports Use in this session, Make default, Publish and Clear binding. |
| Permissions | Normal / Auto / YOLO / Super YOLO picker. Gentle Shell only changes a live session's mode from its terminal UI, so the app saves the default (`permission-mode.json`) and starts a new session. |
| Sessions (⌘⇧O) | Resume previous conversations for this project or any project; sessions from another project open in a new tab. ⌘N starts a new session. |
| Usage & stats | Subscription windows (ChatGPT/Codex, NaN) and local token/cost stats from session files, in place of `/gentle:usage` and `/gentle:stats`. |
| Session chips | As in Tern, what a session tracks sits in chips over the composer instead of a side panel, so every pane keeps its full width: background **Jobs**, **Helpers 1/3** (each subagent's status, task and last step, with a button to the full Agents window), extension widgets and the last response's tokens. Click a chip for its card. The task list is a Dynamic-Island-style pill at the top of the conversation: when a session has tasks, **Todos 2/7** appears there with a small progress equalizer. Click it to unfold the task list downward over the conversation, then click again to fold it; the list shows each task's status (done, in progress, pending, blocked, dropped) and note. It floats over the chat, so the conversation never moves, and nothing is added around the composer. |
| Command palette (⌘K) | Categories: All, Commands, Workspaces, Files, Branches. Includes every Gentle Shell slash command and skill. ↑↓ move, ↵ runs, Tab or ←→ (while the search is empty) switch category, Esc closes. Gentle Shell commands whose screen only exists in the terminal open the app's own: `/gentle:usage` and `/gentle:stats` open Usage (Subscription / Local stats), `/gentle:agents` and `/gentle:jobs` the Agents window, `/gentle:profiles` (no arguments) Profiles, `/gentle:commands` ⌘K, `/gentle:changes` Files › Changes. `/gentle:customize`, `/gentle:guide`, `/gentle:voice`, `/gentle:music`, `/gentle:panel` and `/history` do nothing over RPC and are hidden. Double-click an agent tab to rename its session. |
| Configuration | Your existing Gentle Shell setup: home, models, profiles, sign-ins, permission mode, persona, MCP, packages (install and remove), Claude bridge plan, and the theme picker: Gentle Shell's themes, your Pi themes (`<agent dir>/themes/`), popular terminal schemes and **OLED Black** (true black that stays solid at any transparency, so an OLED screen keeps those pixels off; soft pearl text and the Gentle rose), applied app-wide and to terminals, remembered across launches. A Transparency slider (default 60%) sets how much of the blurred desktop shows through the window, as in Tern; menus, the palette and screens such as Profiles stay solid at any setting. **Setup & requirements** is the first-run checklist, always here: the same checks, installs, terminal commands and sign-in. When a newer Gentle Shell is published, a card offers Update now (runs `gentle-shell upgrade`), Release notes and Skip this version. When the upgrade ends the card says how it went, from what the app actually runs afterwards: installed (open agent tabs restart on it and resume their conversations; a tab mid-turn switches when the turn ends), installed but an older launcher still comes first (with the command to remove it), or failed (with Gentle Shell's output). The check mirrors the launcher's own startup check: npm `gentle-pi/latest`, stable versions only, cached for a day, with the cache and the skipped version shared with the terminal in `~/.gentle-shell/config.json`. Agent tabs run Gentle Shell's self-contained install when there is one (`~/.gentle-shell/bin/gentle-shell`; `%LOCALAPPDATA%\gentle-shell\bin\gentle-shell.cmd` on Windows), which `gentle-shell upgrade` sets up since 4.7, ahead of an older global `gentle-pi` on your PATH; otherwise whichever `gentle-shell` is on your PATH, so updating outside the app works too. |

## Requirements

- Gentle Shell: `npm install -g gentle-pi --allow-scripts=gentle-pi` (npm 11 blocks its postinstall otherwise), then `gentle-shell setup` once. Setup installs Engram through Homebrew; on macOS that may first need `brew trust gentleman-programming/tap`.
- Node.js on your PATH. The app merges in your login shell's PATH, so nvm, fnm and mise work even when you launch the app from Finder.
- Rust 1.88 or newer.
- Linux also needs the GPUI system libraries and WebKitGTK (the in-app browser), for example on Debian or Ubuntu: `libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev libwebkit2gtk-4.1-dev libgtk-3-dev`.

## Install and run

### From a release

Download the asset for your system from [Releases](https://github.com/ismaelosuna7824/gentle-shell-gpui/releases) (names under [Packages and releases](#packages-and-releases)).

- **macOS:** open `gentle-shell-gpui-<version>-macos-aarch64.dmg` (Apple silicon) or `…-macos-x86_64.dmg` (Intel) and drag **Gentle Shell** onto **Applications**. The app is ad-hoc signed, not notarized, so the first launch is blocked: right-click the app → **Open** → **Open**, or allow it in System Settings → Privacy & Security → **Open Anyway**. From a terminal, `xattr -dr com.apple.quarantine /Applications/"Gentle Shell.app"` does the same. Later versions install from inside the app ([Updating the app](#updating-the-app)).
- **Windows:** run `gentle-shell-gpui-<version>-windows-x86_64-setup.exe`. It installs for your user only (no admin rights) into `%LOCALAPPDATA%\Programs\Gentle Shell`, adds Gentle Shell to the Start menu (and the desktop if you tick it) and to Apps → Installed apps for uninstalling, and puts the `gsd` command on your user PATH; installing a newer setup over it upgrades in place. SmartScreen may warn about an unknown publisher: **More info** → **Run anyway**. The `…-windows-x86_64.zip` holds the bare exe (the in-app updater uses it). The in-app browser uses the Microsoft Edge WebView2 Runtime, which Windows 11 and updated Windows 10 already have.
- **Linux:** extract the tarball and run `gentle-shell-gpui-<version>/gentle-shell-gpui`. Copy the `.desktop` file to `~/.local/share/applications/` (and fix its `Exec=` path) to get a launcher entry. The app links WebKitGTK 4.1 for its in-app browser: on Debian or Ubuntu (22.04 or newer) `sudo apt install libwebkit2gtk-4.1-0` if it isn't installed yet.

### From source (no release installed)

1. Install the [requirements](#requirements): Gentle Shell, Node.js, Rust (`curl https://sh.rustup.rs -sSf | sh`), and on Linux the system libraries.
2. Build and run:

   ```bash
   git clone https://github.com/ismaelosuna7824/gentle-shell-gpui.git
   cd gentle-shell-gpui
   cargo run --release -- .
   ```

   The first build takes a few minutes. `.` opens the current folder as a workspace; pass any folder instead, or nothing to reopen the last workspace. `--prompt "first message"` starts a new agent tab with that message.
3. Optional, macOS: to run it as a normal app (rose icon in the Dock, Finder, Spotlight), bundle it and copy it to `~/Applications`:

   ```bash
   packaging/macos/bundle.sh aarch64-apple-darwin 0.1.0   # x86_64-apple-darwin on Intel
   cp -R "dist/Gentle Shell.app" ~/Applications/
   open ~/Applications/"Gentle Shell.app"
   ```

A build from source never updates itself; `git pull` and run `cargo run --release` again.

### Options

```bash
cargo run --release -- [FOLDER] [--prompt "first message"]
```

| Variable | Effect |
| --- | --- |
| `GENTLE_SHELL_BIN` | Path of the launcher to use instead of `gentle-shell` on `PATH`. |
| `GENTLE_SHELL_ARGS` | Launcher options placed before `--mode rpc`, such as `--link` or `--home "<dir>"` (quote paths with spaces). The app also uses them to find `auth.json` and the config home. |

Only one app runs per user, whatever the build: a second launch hands its folder and `--prompt` to the running window and exits.

Gentle Shell's own variables (`GENTLE_PI_CONFIG_HOME`, `GENTLE_SHELL_NO_AUTO_SETUP`, provider API keys, and the rest) pass through to the backend.

## Packages and releases

The app icon (the Gentle Shell pixel rose) lives in `packaging/icons/`. `generate.py` builds it from the `ROSE` art in `crates/app/src/shared/banner.rs`; run `packaging/icons/generate.sh` on macOS after editing and commit the outputs (`AppIcon.icns`, `icon.ico`, PNGs). A bare `cargo run` shows a generic icon in the Dock; the bundled app shows the rose.

Build packages locally:

- macOS: `packaging/macos/bundle.sh aarch64-apple-darwin 0.1.0` → `dist/Gentle Shell.app`, `dist/gentle-shell-gpui-0.1.0-macos-aarch64.zip` and `.dmg` (`x86_64-apple-darwin` for Intel; it cross-compiles from Apple silicon). `GENTLE_BIN=<path>` packages an existing build instead.
- Linux: `packaging/linux/package.sh x86_64-unknown-linux-gnu 0.1.0` → `dist/gentle-shell-gpui-0.1.0-linux-x86_64.tar.gz`.
- Windows: the icon and version info are embedded in the exe by `crates/app/build.rs`; the installer is `packaging/windows/installer.iss` (Inno Setup 6: `ISCC /DAppVersion=<v> /DSourceExe=<exe> /DOutputDir=dist /DIconFile=packaging\icons\icon.ico packaging\windows\installer.iss`).

Build profiles: `cargo run --release` uses Cargo's default release settings, so local optimized builds stay quick. Packages use `--profile dist` (thin LTO, one codegen unit, stripped): about a third smaller, much slower to build. The scripts and the Release workflow build `dist`.

Workflows: **CI** runs clippy and the tests on macOS, Linux and Windows for pushes to `main` and pull requests (Markdown-only changes skip it). **Release** builds and publishes packages for `v*` tags. **Cache warm** builds the `dist` profile on `main` only when `Cargo.lock`, a `Cargo.toml` or the release workflows change, and saves the dependency cache that tag builds restore (GitHub doesn't let one tag reuse another tag's cache).

Cut a release:

1. Bump `version` under `[workspace.package]` in the root `Cargo.toml`, run `cargo check` so `Cargo.lock` updates, and commit.
2. Optional dry run: Actions → Release → Run workflow. It builds all four targets and uploads the assets and `SHA256SUMS` as an artifact without publishing.
3. `git tag v0.2.0 && git push origin main v0.2.0`. The Release workflow checks the tag against the workspace version, builds macOS (arm64 and x86_64), Windows and Linux, writes `SHA256SUMS` and publishes the GitHub release with generated notes. A version with `-` (`0.2.0-rc.1`) is a prerelease.

Assets: `gentle-shell-gpui-<v>-macos-aarch64.dmg` / `.zip` and `…-macos-x86_64.dmg` / `.zip` (the same ad-hoc signed `Gentle Shell.app`: the DMG to install by dragging, the zip for the in-app updater), `…-windows-x86_64-setup.exe` (the installer) and `…-windows-x86_64.zip` (the bare exe, for the in-app updater), `…-linux-x86_64.tar.gz` (binary, `.desktop`, icon) and `SHA256SUMS`. The in-app updater relies on these names.

### Updating the app

On startup (at most every 6 hours) the app checks this repo's GitHub releases and shows an **Update** pill in the title bar when a newer version exists. Configuration → App shows the current version and has Check for updates, Release notes, **Download & install** and Skip this version. Nothing installs without your click; the download is verified against the release's `SHA256SUMS` first. Then **Restart now** saves your workspaces, quits and opens the new version.

- macOS: the `.app` is replaced in place; if its folder isn't writable (e.g. `/Applications` without permission) the app says so. Builds run with `cargo run` don't self-update.
- Windows: the old exe is kept as `gentle-shell-gpui.exe.old` until the next start.
- Linux: the binary is replaced in place, keeping its permissions.

The check cache and the skipped version are in `<data dir>/gentle-shell-gpui/app-update.json`.

## Limits of an RPC host

- Gentle Shell does not publish model, tokens or cost for live helpers.
- Claude subscription windows come only from response headers inside the terminal UI.
- Helpers and background jobs can only be stopped by the agent.
- Auto permission mode may refuse actions: Gentle Shell does not count RPC input as a live human decision.

## Not done yet

Known gaps and things not tested yet, so you know what to expect:

- **First release:** v0.1.0 is the first one, so the in-app updater (download, checksum, replace, restart) can only be tested end to end once v0.1.1 exists. The Windows and Linux builds first compile in that release's CI; they have only been type-checked on macOS.
- **Signing:** macOS builds are ad-hoc signed, not notarized, and Windows builds are unsigned, hence the first-launch warnings above.
- **Claude usage:** Usage & stats shows nothing for Claude through `pi-claude-bridge`. Gentle Shell keeps those 5h/7d windows in its terminal UI only; the app sees the bridge's high-usage warnings (the composer's `7d 57%` chip) but not the full windows. Reading Claude Code's login from the Keychain to ask Anthropic's usage endpoint is possible but not implemented.
- **Quitting with unsaved files:** closing a pane or tab with unsaved edits asks first, but ⌘Q does not.
- **Commit message generation** has no timeout or cancel; if `gentle-shell` hangs, the spinner stays.
- **Worktrees and submodules** in the Branches tab are shown but have no actions.
- **Symlinked folders:** a workspace opened through a symlink shows no change markers in the file tree (git reports the real path).
- **Workspace switcher** (⌃⌘K) doesn't show that an agent is still working in another workspace.
- **Hidden tabs** restored at launch start their backend only when shown, so their title shows the folder name until then.
- **Markdown preview:** links to other `.md` files open in the default app, not in a pane, and `#anchor` links do nothing.
- **Visual checks:** several screens were only checked from code, not on screen: the chips' cards, the App section of Configuration, the update pill, the Background subagents switch, and the Changes / Branches tabs with real repositories.

## Architecture and structure

The app is a modular monolith organized by feature. The developer documentation lives in [`docs/`](docs/README.md):

- [`docs/architecture.md`](docs/architecture.md): boundaries, dependency rules, testing, and where each thing goes.
- [`docs/design-system.md`](docs/design-system.md): colors, typography, components, animations and screen patterns.
- [`docs/integrations.md`](docs/integrations.md): how the app talks to Gentle Shell and other services.
- [`docs/development.md`](docs/development.md): building, isolated testing, CI and releases.
- [`docs/adr/`](docs/adr/): architecture decisions.

[`AGENTS.md`](AGENTS.md) is the short entry point for new developers and AI agents.

| Crate / module | Role |
| --- | --- |
| `crates/core` | Pure logic with no GPUI or IO (`gentle-core`): transcript and subagent activity, tool summaries, permission mode / jobs / ODD phase, slash-command routing, notifications policy, profiles routing, usage parsing, git parsers, session summaries, fuzzy ranking, versions. |
| `crates/pi-rpc` | Launcher and home resolution (matching Gentle Shell's launcher, bundled installs included), the JSONL client, typed extension UI requests, and the sign-in helper. |
| `crates/app/src/main.rs`, `app/`, `cli.rs` | Startup, the composition root (GPUI, keys, menus, window) and the command line. |
| `crates/app/src/features/` | One folder per feature: `session` (one tab), `workspace` (window, tabs, screens), `welcome`, `onboarding`, `setup`, `providers`, `profiles`, `settings`, `usage`, `sessions`, `palette`, `memory`, `git`, `files`, `terminal`, `browser`, `devices`, `notices`, `alerts`, `updates`, `app_update`, `workspaces`. |
| `crates/app/src/shared/` | `ui.rs` (design system), `floating.rs` (shared card geometry and drag), `themes.rs` (theme catalog and persistence), `banner.rs`, `assets.rs`, `surge.rs`, `pane.rs`, `gentle_state.rs`. |
| `crates/app/src/platform/` | Single instance and OS-specific icons. |

```bash
cargo test
```
