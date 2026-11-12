# Integrations

How Gentle Shell GPUI connects to Gentle Shell (the `gentle-shell --mode rpc` backend of each agent tab) and to external services: which process it launches, which protocol it speaks, which files it reads or writes, and who owns them. At the end, how to add a new integration without breaking the boundaries in [`architecture.md`](architecture.md).

## Map

| Integration | Where it lives | Transport |
| --- | --- | --- |
| Backend of each tab | `crates/pi-rpc/src/lib.rs` (`PiProcess`), `crates/app/src/features/session/` | Child process, JSONL over stdin/stdout |
| Launcher and PATH resolution | `crates/pi-rpc/src/launch.rs` | File system, login shell |
| Homes and config folder | `crates/pi-rpc/src/home.rs` | File system |
| Provider sign-in | `crates/pi-rpc/src/login.rs`, `login-helper.mjs`, `features/providers/` | Node process, JSONL |
| Launcher subcommands (`upgrade`, `install`, `remove`, `setup`) | `features/settings/upgrade.rs`, `settings/packages.rs`, `setup/gentle.rs` | Child process |
| Engram (memory) | `features/memory/client.rs`, `gentle_core::engram` | Local HTTP |
| Maestro (UI testing tools for the agent) | `features/settings/maestro.rs` (the section), `features/setup/maestro.rs`, `setup/fetch.rs` | Downloads into the toolchain; `gentle-shell mcp add` |
| Music (gentle-stream) | `features/music/`, `features/settings/music.rs`, `features/setup/music.rs`, `gentle_core::music` | Unix socket (`multiprocessing.connection`); a Python helper in the player's environment |
| Subscription usage | `features/usage/` | HTTPS |
| Gentle Shell update | `features/updates/` | HTTPS (npm registry) |
| App update | `features/app_update/` | HTTPS (GitHub releases) |
| First-run installers | `features/setup/`, `features/onboarding/` | Processes, HTTPS |
| Git | `features/git/` | `git` process |
| Gentle Design canvas | `features/browser/` | System web view |
| Web apps (`gentle-open`, `/web`, dev-server URLs) | `features/browser/`, `gentle_core::web` | Single-instance request, system web view |
| Devices (Android emulators, iOS simulators) | `features/devices/`, `features/settings/devices.rs`, `features/setup/devices.rs`, `gentle_core::devices` | Expo's device hub (Node child, loopback HTTP and its web viewer), `agent-device`, `gentle-device` |

Common rule: every child process is created with `pi_rpc::command` (never `std::process::Command::new` directly) and, if it needs to find the user's tools, it gets `pi_rpc::child_path()` as `PATH`.

## Launcher: `crates/pi-rpc/src/launch.rs`

### `resolve_backend(cwd) -> LaunchSpec`

Program lookup order:

1. `GENTLE_SHELL_BIN`, if not empty.
2. `bundled_launcher()`: the self-contained Gentle Shell install (4.7, the one created by its installer and by `gentle-shell upgrade`). It wins over a global npm `gentle-pi` left on the PATH.
3. `gentle-shell` on the combined PATH (`which::which_in` with `search_path()`).

If nothing is found, the error asks for `npm i -g gentle-pi` or `GENTLE_SHELL_BIN`.

`LaunchSpec { program, args, cwd, env }`:

| Field | Value |
| --- | --- |
| `args` | `GENTLE_SHELL_ARGS` split by `split_args()` (spaces separate; `"…"`/`'…'` keep spaces; backslashes literal), then `--mode rpc` |
| `env` | `GENTLE_SHELL_INTERACTIVE_HOST=1` (enables `ask_user_question` dialogs and live helper activity) and `PATH` = combined PATH |
| `cwd` | The tab's folder |

Launcher subcommands reuse the same `LaunchSpec`, but take only the arguments before `--mode` (`spec.args.iter().take_while(|a| *a != "--mode")`) and append their own: `upgrade` (`settings/upgrade.rs`), `install`/`remove` (`settings/packages.rs`), `setup` (`setup/gentle.rs`, `launcher_command`).

### Self-contained install

`bundled_candidates` defines where the launcher is looked up:

| OS | Launcher |
| --- | --- |
| macOS and Linux | `~/.gentle-shell/bin/gentle-shell` |
| Windows | `%LOCALAPPDATA%\gentle-shell\bin\gentle-shell.cmd`, then `%USERPROFILE%\.gentle-shell-bundle\bin\gentle-shell.cmd` |

`bundled_install(launcher) -> Option<BundledInstall { node, package }>` recognizes the generated script by the `BUNDLED_LAUNCHER_MARK` marker (`"gentle-shell bundled launcher (generated; do not edit)"`) and derives the layout:

```text
<prefix>/
├── bin/gentle-shell[.cmd]        # generated script
├── runtime/node-<v>/bin/node     # or node.exe on Windows
├── current                       # link to the active version; on Windows, a file with the folder name
├── versions/<v>/node_modules/gentle-pi
└── config/gentle-ai              # config folder (see Homes)
```

It is used by `pi_package` (to find `@earendil-works/pi-coding-agent`) and by `LoginHelper::spawn` (to use the install's own Node).

### Combined PATH

| Function | What it does |
| --- | --- |
| `login_shell_path()` (Unix only) | Runs `$SHELL -l -i -c "printf '__GENTLE_PATH_START__%s__GENTLE_PATH_END__' \"$PATH\""` with a 5 s limit; the markers isolate the value from whatever the rc files print (nvm, fnm, mise). On Windows it returns `None`. |
| `base_path()` | Login shell PATH first, then the inherited one without duplicates. Computed once (`BASE_PATH`). |
| `set_extra_path(dirs)` | Folders that go before everything else. `setup::apply_toolchain_path` puts the toolchains installed by the app here (`<data dir>/gentle-shell-gpui/toolchain/{node,npm-global,bin,maestro/bin,music/bin}` and the Java runtime's `bin`), plus the Android SDK's `emulator` and `platform-tools` when an SDK is installed (`devices::android_tool_dirs`: Android Studio doesn't put them on the PATH, so an agent asked to start an emulator wouldn't find them). |
| `search_path()` / `child_path()` | Extra + base. This is the PATH every child receives. |
| `find_program(name)`, `resolve_node(cwd)` | Lookup on that PATH. |
| `forget_cached()` | Forgets the login PATH and the cached `--version` outputs; call it after installing something. |

### Backend version

`version_output(spec)` runs `<launcher> --version` and caches it per launcher, invalidating when the `Stamp` of the launcher file or of `~/.gentle-shell/config.json` changes. `backend_version` extracts the `gentle-shell X.Y.Z` line. `forget_versions()` is used after `gentle-shell upgrade`.

### `pi_rpc::command`

On Windows it adds `CREATE_NO_WINDOW` (`0x0800_0000`): without it, every console child (git, node, shells, the backend) would open its own window in a release build, and in debug it would attach to the terminal that launched the app. On other platforms it is `Command::new`.

## Homes: `crates/pi-rpc/src/home.rs`

`resolve_homes() -> Homes { agent, config }` replicates gentle-pi's `resolveHome` (`lib/gentle-shell-launcher.ts`). It is cached until the `Stamp` of `~/.gentle-shell/config.json` changes.

`agent` (the Pi directory: `auth.json`, `models.json`, `settings.json`, `sessions/`), in order:

| Source | Result | Isolated? |
| --- | --- | --- |
| `--link` in `GENTLE_SHELL_ARGS` | `user_pi_home()`: `GENTLE_SHELL_USER_PI_HOME`, `PI_CODING_AGENT_DIR` or `~/.pi/agent` | No |
| `--isolated` | `GENTLE_SHELL_HOME` or `~/.gentle-shell/agent` | Yes |
| `--home <dir>` | `<dir>` | No |
| `~/.gentle-shell/config.json` `mode`: `link` / `path` (`dir`) / `isolated` | Same as above | Depends on mode |
| Nothing | Isolated by default | Yes |

`config` (Gentle AI config: `profiles.json`, `permission-mode.json`, …), in `config_home`:

1. `GENTLE_PI_CONFIG_HOME`, if not empty.
2. If there is no `GENTLE_SHELL_BIN` and the self-contained install exists: `<prefix>/config/gentle-ai`.
3. If the home is isolated and is exactly `~/.gentle-shell/agent`: `~/.gentle-shell/gentle-ai`.
4. Otherwise: `~/.pi/gentle-ai`.

`adopt_standalone_config()` is called once in `main.rs` before the window opens. The 4.7 install no longer reads `~/.gentle-shell/gentle-ai`; this function copies the missing files into the new folder (recursively, skipping `.lock` and `.tmp`, never overwriting) and leaves the `.adopted-standalone-config` marker so it does not run again. The old folder is left untouched.

Any feature that reads Gentle Shell configuration starts from `pi_rpc::resolve_homes()`; paths are never rebuilt by hand.

## RPC protocol

### Process and transport (`crates/pi-rpc/src/lib.rs`, `record.rs`)

`PiProcess::spawn(&spec)` launches the child with stdin/stdout/stderr on pipes and starts two threads:

- `pi-rpc-stdout`: reads records separated **by LF only** (`read_until(b'\n')`); Unicode separators (U+2028) are valid inside JSON strings. `parse_record` strips the trailing `\r` and classifies by `type`.
- `pi-rpc-stderr`: each line is `Incoming::Stderr`, never protocol data.

| stdout record | `Record` | Destination |
| --- | --- | --- |
| `type: "response"` with a pending `id` | `Response` | The waiting `request` |
| `type: "response"` with no owner and failed | `Response` | `Incoming::Stderr("<command> failed: …")` |
| `type: "extension_ui_request"` | `UiRequest` | `Incoming::Ui` |
| Any other `type` | `Event(Value)` | `Incoming::Event` (kept as JSON: Pi adds events over time) |
| Empty line | `Empty` | Ignored |
| Invalid JSON | — | `Incoming::Stderr("unparseable RPC record: …")` |

When stdout closes, pending requests are flushed (their callers get the error "gentle-shell exited before answering") and `Incoming::Exited(code)` arrives.

`request(command)` assigns `id` = `gs-<n>` and writes one line at once; the returned future holds only the response channel (`success: false` becomes `Err(error)`), so a pending request never keeps the process alive. `respond_ui(id, UiAnswer)` answers dialogs with `extension_ui_response` (`value`, `confirmed` or `cancelled: true`). `shutdown(grace)` is idempotent and returns at once: on Unix it sends SIGTERM to the launcher (forwarded to Pi, whose handler stops its agents' detached children; stdin stays open until then, because Pi drops that handler on stdin EOF), elsewhere it closes stdin; when the backend exits or after `grace`, it kills the backend's process group (Unix, spawned with `process_group(0)`) or tree (`taskkill /T /F`, Windows) and reaps it. `Drop` calls `shutdown(3 s)`; app quit calls it for every tab (`Workspace` `on_app_quit` → `SessionView::shutdown_backend`).

### Tab lifecycle (`features/session/lifecycle.rs`)

- `start` clears the tab state, resolves the `LaunchSpec` on the background executor, appends `--session <path>` if there is a conversation to resume (`backend.pending_session`), queries the version and calls `PiProcess::spawn`. Then `refresh_state`, the pending prompt (`-p`), and a loop that batches queued `Incoming` items (`drain_queued`) and applies them with `handle_batch` in a single update.
- Resuming happens at startup (`--session`), not with `switch_session` afterwards: Gentle Shell treats the startup session as new and starts it in the saved permission mode; a switched session starts in Normal.
- `restart_when_idle` / `backend.restart_pending`: restarts in the middle of a turn (or with a dialog open) wait for it to finish. Used by `reload` (`/reload`), `reload_models` (after signing in or out of a provider), `restart_for_update` (after `gentle-shell upgrade`) and `choose_mode` (permission change).
- `retry_start` retries tabs whose backend never started (after onboarding).

### Permission mode (`state::desktop::permission`)

The picker's selection is the consent. When the backend's `capabilities` advertise `set_permission_mode`, the app sends `set_permission_mode` with `persist: true` over the Gentle Desktop RPC channel and switches the live session without writing `permission-mode.json` or restarting. The server's answer is the truth: `changed: false` is a no-op, a `persist_error` is live success plus a "default not saved" warning, and a timeout/transport/server failure reverts and reports. Events move the confirmed state only for the exact `permission_mode_changed` name that the capabilities `emits`; a sequence gap resyncs once through the channel's `get_snapshot`.

Two cases keep the legacy path (save `permission-mode.json` and restart the backend on the same conversation, exactly once):

- the capabilities are known and do not list `set_permission_mode`;
- the server answers `unsupported_method` for the `set_permission_mode` it advertised.

While capabilities are still loading, a picker click reports "try again" and never writes the default file or restarts. `choose_mode` in `lifecycle.rs` is the only caller of either path; every other restart caller is unchanged.

### Agents and jobs (`state::desktop::supervision`)

When the backend's `capabilities` advertise `list_subagents` and/or `list_jobs`, the app seeds a full list once the probe (capabilities → snapshot) and the permission seed have settled, then reconciles `agents_update`/`jobs_update` events by id. The structured lists are the primary source once seeded; the legacy `gentle-agents` widget and `details.gentleJobs` stay as the fallback for older Shells, and the panel/jobs chip never shows both, so the two sources cannot double a helper or a job.

- `list_subagents` / `list_jobs`: empty params, answered with the typed `agents`/`jobs` arrays. Only these full-list responses mark a list as seeded. A list that cannot be decoded leaves the legacy fallback in place and is never retried in a loop; a malformed record never fabricates an empty list.
- `agents_update` / `jobs_update`: full bounded projections, so the event replaces the list (ids absent from the projection drop rather than growing forever). They never mark a list as seeded, so a pre-seed event is replaced by the real seed. A sequence gap forgets the seeded markers so the lists re-seed behind the channel's `get_snapshot` resync.
- `cancel_subagent` / `cancel_job`: params `{ id }`. One cancel in flight at a time, with the same timeout/rollback discipline as permission changes. The server's post-action projection is the truth: `cancel_subagent` answers `requested`/`changed` only when supplied (never defaulted, so an async cancel is not read as unchanged), and `cancel_job` answers `changed`. There is no job start method and no restart.

A new/open session (`new_session`/`switch_session`) resets the Desktop channel: it forgets the pending request and the event baseline without rewinding request ids and returns to Idle, so the following `refresh_state` re-probes capabilities and the supervision lists re-seed exactly once behind it. Late answers from the previous session are uncorrelated and benign.

The Agents panel's Stop button uses `cancel_subagent` when the peer offers it; a peer without the method (or a server `unsupported_method` answer) keeps the legacy "ask the agent to run `subagent_cancel`" prompt exactly once. Jobs cancel over `cancel_job` with no legacy prompt. Model, tokens and cost are shown per helper from the typed `usage` window, with an honest "unavailable" marker where the input/output/cache/reasoning breakdown is not recorded; prompt, tool output, paths and error text never cross the wire.

### Commands the app sends

| `type` | Fields | Where | Use |
| --- | --- | --- | --- |
| `prompt` | `message`, `images?`, `streamingBehavior: "steer"?` | `attachments::prompt_command`, `rpc.rs` `send_prompt_with` | Send; in the middle of a turn it is sent as steer |
| `prompt` | `message` (`/gentle:…`) | `rpc.rs` `run_extension_command` | Extension command without a turn; success only if `disposition == "handled"` |
| `clear_queue`, `abort` | — | `rpc.rs` `stop` | Stop the turn (⌘. / Ctrl+. or the Stop button): clear the queue, then abort. Esc does not stop turns; it closes panels and dialogs |
| `bash` | `command`, `excludeFromContext` | `submit.rs` `run_shell` | `!cmd` / `!!cmd`; output arrives via `bash_execution_update` |
| `get_state` | — | `refresh_state`, `set_model` | `model`, `thinkingLevel`, `sessionName`, `sessionFile`, `autoCompactionEnabled` |
| `get_available_models` | — | `refresh_state` | Model picker |
| `get_available_thinking_levels` | — | `refresh_thinking_levels` | Effort slider |
| `get_commands` | — | `refresh_state` | `/` menu and palette |
| `get_messages` | — | `refresh_state` | Rebuild the conversation (`Transcript::load_messages`) |
| `get_session_stats` | — | `refresh_stats` | Cost and `contextUsage.percent` |
| `set_model` | `provider`, `modelId` | `set_model`, `workspace/overlays.rs` | Change model |
| `set_thinking_level` | `level` | `view/effort.rs`, `workspace/overlays.rs` | Effort |
| `new_session` | — | `new_session` | ⌘N |
| `switch_session` | `sessionPath` | `open_session` | Open a saved session in a live backend |
| `set_session_name` | `name` | `rename` | `/name`, double-click on the tab |
| `compact` | `customInstructions?` | `compact` | `/compact` |
| `set_auto_compaction` | `enabled` | `set_auto_compaction` | Context chip |
| `get_last_assistant_text` | — | `copy_last_answer` | `/copy` |
| `export_html` | `outputPath` | `export_to` | `/export` |

Window screens that need another command use `SessionView::send_command(Value)`, which reports errors in the conversation and refreshes the state.

### Events the app handles

`gentle_core::transcript::Transcript::apply` builds the conversation (`agent_start`, `agent_settled`, `message_start`, `message_update` with its `assistantMessageEvent` `text_*`/`thinking_*`/`toolcall_start`, `message_end`, `tool_execution_start`/`_update`/`_end`, `compaction_start`/`_end`). On top of that, `events.rs` `handle_event` reacts to:

| Event | Effect |
| --- | --- |
| `queue_update` | "N queued" chip (`steering` + `followUp`) |
| `compaction_start` / `compaction_end` | Compaction state; refreshes stats |
| `session_info_changed` | Session name |
| `thinking_level_changed` | Effort level |
| `bash_execution_update` | Live output of the active `!cmd` |
| `tool_execution_start` | ODD phase (`gentle_state::infer_phase`); job label for `bash_background` / `monitor`; device launches and device commands of `bash` / `bash_background` → `SessionEvent::Device` / `DeviceCommand` |
| `tool_execution_end` | Task list (`todo`), jobs (`details.gentleJobs`), `gentle_design` canvas → `SessionEvent::DesignCanvas` |
| `message_end` with `role: "custom"` | Updates jobs from `gentle-jobs.notice` |
| Settled turn (`effects.settled`) | Refreshes stats and Git; `TurnEnded` alert unless `aborted` |

Custom message cards (`gentle-agents.result`, `gentle-agents.message`, `gentle-jobs.notice`) are drawn in `session/view/cards.rs`.

### Session files

Gentle Shell writes them. The app only reads them:

- `features/sessions/discovery.rs`: `settings.json` `sessionDir` (with `~` expanded) or `<agent>/sessions/<session_dir_name(cwd)>/*.jsonl`; `summary.rs` does a light cached scan.
- `features/usage/stats.rs`: local token and cost stats.
- The active session path comes from `get_state.sessionFile`; the window stores it in `workspaces.json` to resume with `--session`.

## Devices: Android emulators and iOS simulators

Configuration → Devices has two switches, saved in the app's `settings.json` under `devices` (`enabled`, `agents`):

- **Use devices** installs Expo's device hub (`expo-device-hub` 0.22.1, the version T3 Code pins: serve-sim for iOS, serve-emu for Android) with npm into `toolchain/devices/expo-device-hub-0.22.1`, and shows the device panel's button in the title bar.
- **Agents can use devices** installs Callstack's `agent-device` 0.21.36 the same way (its `node_modules/.bin` joins the agents' PATH) and adds the device instructions to new agent sessions.

Both need Node 22.12 or newer. iOS streaming needs Apple silicon and Xcode (`simctl`); Android needs the SDK, and emulator video needs `ffmpeg` with libx264 (the section offers the install command in a terminal tab). All of this is `setup/devices.rs`.

### The hub (`features/devices/hub.rs`)

One process per app, started on first use (the panel shows, or `gentle-device` asks): `node <entry> --port <free port> --host 127.0.0.1 --hide-sidebar --hide-boot-device`, plus `--platform android` off Apple-silicon macOS, with `ANDROID_HOME` and `DEVELOPER_DIR` (when `xcode-select` points at the Command Line Tools) set, output in `<data>/gentle-shell-gpui/devices/hub.log`. Ready when `GET /readyz` answers; its port and pid go in `devices/hub.json` for `gentle-device`. It stops with SIGTERM (helpers first) when "Use devices" goes off or the app quits; devices the app booted are shut down first.

API used: `GET /api/devices` (`gentle_core::devices::hub_devices` reads it and adds AVDs from `emulator -list-avds` the hub doesn't list yet), `POST /api/devices/boot` and `/api/devices/shutdown` with `{platform, id, name}`; booting is headless (no Simulator.app, no emulator window). The list is polled every 3 s while a panel shows and every second while a device boots; not at all otherwise.

### The panel (`features/devices/panel.rs`)

One `DevicePanel` per window: a tab per device, docked as a 400 px column at the right of the panes (default) or floating as a card (`shared/floating.rs`); a header button switches. Each tab is the hub's own viewer page (`/`, live stream with touch, keyboard and hardware buttons) in a `browser::WebSurface`, pinned to its device by an initialization script that sets `window.__EXPO_DEVICE_HUB_SINGLE_DEVICE__` (what the hub's `--single-device` does). Only the active tab has a web view; hiding the panel drops it, and a screen over the window hides it. The + button lists every device (running first) and opens one, booting it when it's off. ⌘K → Devices shows the panel.

### Agents

- **`gentle-device`** (`devices/cli.rs`): a shim in `toolchain/bin` that runs the app binary with `--device`. `list` asks the app to start the hub (`OpenRequest.device`, through the single-instance server; the window doesn't come forward), waits for `hub.json` and prints the list. `open <name|id>` asks the app to open the device (it boots it and shows its tab), waits until it runs and prints `agent-device --platform ios --udid <id> …` / `--platform android --serial <serial> …` calls. `close <name|id> [--shutdown]` closes the tab (and turns it off).
- **Instructions** (`devices/instructions.rs`): `<data>/gentle-shell-gpui/agent-instructions.md`, appended to every new RPC session's system prompt with `--append-system-prompt <file>` (`pi_rpc::set_system_prompt`; a file because Windows' `.cmd` launcher mangles quotes and newlines in arguments). Pages always: `gentle-open <url>`, never an outside browser. With both switches on, devices too: `gentle-device` to open, `agent-device` for every launch, tap, text and screenshot, never `simctl boot`, `open -a Simulator`, `emulator -avd` or `adb shell input`.
- **Commands agents run anyway**: `SessionView` reads each `bash`/`bash_background`/`monitor` command. `gentle_core::devices::launch_intent` recognizes `emulator -avd <name>`, `emulator @<name>`, `[xcrun] simctl boot …` and the Windows forms (`start "" …emulator.exe`, `cmd /c start`, `Start-Process emulator -ArgumentList …`); the panel then opens that device's tab once it runs (for 3 minutes). `device_command` names the last `adb`/`simctl`/`maestro`/… command, shown under the tabs.

The Android SDK's `emulator` and `platform-tools` are on the agents' PATH (`devices::android_tool_dirs`): Android Studio doesn't put them there.

## Web apps and the in-app browser

`features/browser/` is a general system web view now, not only Gentle Design's canvas. It shows any `http(s)` page in its own floating card; the design canvas, web apps and device cards all live in the same tileable stage list, and the ones an agent opened wait in the instance tray (see Stages and the instance tray).

| Entry point | What happens |
| --- | --- |
| `gentle-open <url>` | A generated shim in `<data dir>/gentle-shell-gpui/toolchain/bin` (`.cmd` on Windows), written at startup by `platform/shims.rs` with `gentle-device`. It runs `<app> --open-url <url>`, which forwards an `OpenRequest` with `url` through the single-instance server to the running window. The agent's shell PATH already contains that `toolchain/bin` via `pi_rpc::child_path()`. |
| `/web <url>` | A composer built-in. It emits `SessionEvent::OpenWeb` without sending a prompt to Gentle Shell; the card shows at once. |
| A shell tool prints a URL | `gentle_core::web::loopback_url` scans the tool's command, result text and extension `notify` messages; only `localhost`, `127.0.0.1` and `[::1]` URLs count (`SessionEvent::AgentWeb`), so a dev server (`Vite`, `Next`, `python -m http.server`, …) appears in the instance tray without asking. Public URLs require the explicit tool. A URL that is already open reuses its card; a new URL adds another one, so the agent can keep a web build and a mobile build visible together. |

Only `http://` and `https://` are accepted by `--open-url` and `/web`. The generated shim is app-owned; no Gentle Shell configuration is modified. The app must be running to forward the request (in the agent context it always is); if it is not, the CLI becomes the primary and opens the window with the page. A canvas `Stopped` notice only closes the card when it is still showing the design, not after it navigated to a web app.

## Extension UI requests

`UiRequest { id, timeout, method: UiMethod }` (`crates/pi-rpc/src/record.rs`). Unknown methods decode as `UiMethod::Unknown` and are ignored. `UiMethod::is_dialog()` identifies the ones that block the extension.

| Method | Mapping in the app (`session/events.rs` `handle_ui`) |
| --- | --- |
| `select`, `confirm`, `input`, `editor` | `extension.dialogs` queue; the first one is drawn (`session/view/dialogs.rs`) and answered with `answer_dialog` → `respond_ui`. Triggers the `NeedsYou` alert. If it has a `timeout`, it is removed from the queue when it expires (the agent already resolved it). Esc answers `Cancelled`. |
| `notify` | `notifyType` → `NoticeLevel` (`error`, `warning`, everything else `Info`). "Design canvas…" text → `browser::canvas_notice` → canvas. Usage limit notices (`parse_rate_limit`, "`N% used (window)`") → chip in `extension.rate_limits`. Everything else: capture for the running `/` command (`builtins::Capture`), toast only for warning/error (no repeats, `first_toast`) and `SessionEvent::Notice` to the notification center (`features/notices/`). |
| `setStatus` | `extension.statuses[key] = text`; empty or missing text removes the key. |
| `setWidget` | `gentle-agents` → `gentle_core::activity::parse` + `retain_finished` (the legacy helper source, used only until `list_subagents` seeds; see “Agents and jobs”). Any other key → `extension.widgets` (one chip per widget with its lines). `widgetPlacement` is ignored. |
| `setTitle` | Tab title. |
| `set_editor_text` | Replaces the composer text. |

Known keys:

| Key | Type | Handling |
| --- | --- | --- |
| `gentle:mode` | status | Parsed with `PermissionMode::from_status` ("Permissions: …") for the permissions chip; hidden as text. |
| `gentle:yolo` | status and widget | Hidden: the permissions chip already shows the mode. |
| `gentle-jobs` | status | Hidden: jobs have their own chip, built from `details.gentleJobs`. |
| `gentle-agents` | widget | Helper activity model, not a generic chip. |
| Any other | status / widget | Generic text next to the composer (`view/activity.rs`); in narrow panels only an icon with a tooltip, in minimal panels it collapses into "+N". The code gives no special handling to any Engram key. |

The filters are in `session/view/activity.rs` (`statuses` and `render_widget_chips`). `lifecycle.rs` `start` clears statuses, widgets and dialogs on every start.

## `/` commands and native screens

Gentle Shell screens are TUI components (`ctx.ui.custom()` returns nothing over RPC). `builtins::classify(text)` decides, in this order:

1. `Submit::Builtin`: Pi built-ins that the app runs over RPC (`BUILTINS`: `compact`, `name`, `copy`, `export`, `reload`).
2. `Submit::Open(Screen)`: `gentle_core::commands::route` (`gentle:usage`, `gentle:stats`, `gentle:agents`, `gentle:jobs`, `gentle:profiles` without arguments, `gentle:commands`, `gentle:changes`). The tab emits `SessionEvent::OpenScreen` and `workspace/overlays.rs` `open_screen` opens the screen.
3. `Submit::Prompt`: sent as is.

`gentle_core::commands::hidden` hides from the `/` menu and the palette the commands that have no effect over RPC (`gentle:customize`, `gentle:guide`, `gentle:voice`, `gentle:music`, `gentle:panel`, `history`). The palette applies the same rules in `palette/commands.rs` `command_row`.

Native screens that act on the backend through the shell's own commands: Profiles (`ProfilesEvent::RunCommand`, e.g. `/gentle:profiles publish <name>`), Background subagents (`SettingsEvent::ShellCommand` → `run_extension_command("/gentle:background-subagents enable|disable")`).

## Files the app reads and writes

App paths: `dirs::data_dir()/gentle-shell-gpui/` (on macOS, `~/Library/Application Support/gentle-shell-gpui/`).

| File | Owner | Read / write | Module |
| --- | --- | --- | --- |
| `<data>/gentle-shell-gpui/settings.json` (`theme`, `transparency`, `onboarding`) | App | R/W | `shared/themes.rs` (`store_path`, `write_key`), `features/setup/mod.rs` (`first_run`, `mark_done`) |
| `<data>/gentle-shell-gpui/workspaces.json` | App | R/W | `features/workspaces/store.rs` |
| `<data>/gentle-shell-gpui/app-update.json` | App | R/W | `features/app_update/cache.rs` |
| `<data>/gentle-shell-gpui/instance.json` (port and token, 0600) | App | R/W | `platform/instance.rs` |
| `<data>/gentle-shell-gpui/toolchain/` | App | W | `features/setup/` |
| Launcher `config.json` (`mode`, `dir`): `pi_rpc::launcher_config_path()`, `GENTLE_SHELL_CONFIG` or `~/.gentle-shell/config.json` | Launcher | R | `pi-rpc/home.rs` |
| Launcher `config.json` (`startupReleaseCheck`, `skippedStartupRelease`) | Launcher, shared | R/W | `features/updates/` (same `launcher_config_path()`) |
| `<agent>/auth.json` | Pi | R (Pi writes it via `login-helper.mjs`) | `usage/credentials.rs`, `settings/snapshot.rs`, `setup/extras.rs` |
| `<agent>/settings.json` | Pi | R/W | `themes.rs` (`theme`), `sessions/discovery.rs` (`sessionDir`), `profiles/files.rs` (orchestrator), `setup/gentle.rs` (`packages`) |
| `<agent>/models.json`, `<agent>/mcp.json` | Pi | R (`mcp.json` changes only through `gentle-shell mcp add`, so Pi writes it) | `settings/snapshot.rs`, `setup/extras.rs`, `setup/maestro.rs` |
| `<agent>/subagents.json`, `<cwd>/.pi/subagents.json` | Gentle AI | R/W | `profiles/env.rs`, `profiles/apply.rs` |
| `<agent>/gentle-ai/managed-assets.json` | Gentle AI installer | R/W | `profiles/apply.rs` |
| `<agent>/claude-bridge.json` | Claude bridge | R/W | `settings/claude.rs` |
| `<agent>/themes/*.json` | User | R | `shared/themes.rs` |
| `<config>/profiles.json`, `<config>/models.json`, `<config>/profile-fallbacks.json` | Gentle AI | R/W | `profiles/files.rs`, `profiles/env.rs` |
| `<config>/permission-mode.json` (2 keys, 0600, atomic write) | Gentle AI | R/W | `shared/gentle_state.rs` |
| `<config>/notifications.json` | Gentle AI | R/W | `features/alerts/policy.rs` |
| `<config>/background-subagents.json`, `<cwd>/.pi/gentle-ai/background-subagents.json` | Gentle AI | R (changes via shell command) | `settings/background.rs` |
| `<config>/banner.json` | Gentle AI | R | `shared/banner.rs`, `settings/snapshot.rs` |
| `<config>/persona.json`, `<cwd>/.pi/gentle-ai/persona.json` | Gentle AI | R | `settings/snapshot.rs` |
| `<cwd>/.pi/gentle-ai/profile.json`, `<git common>/gentle-ai/profile-pin.json` | Gentle AI | R/W | `profiles/pins.rs` |
| `*.jsonl` sessions | Pi | R | `sessions/`, `usage/stats.rs` |
| gentle-stream's runtime folder (`$XDG_RUNTIME_DIR/gentle-stream`, else `~/.cache/gentle-stream/run`): `authkey`, `player.sock` | gentle-stream | R (the key, to authenticate) | `music/client.rs` |
| `~/.cache/gentle-stream/FluidR3_GM.sf2` | gentle-stream (`setup-samples`) | R (exists?) | `setup/music.rs` |

`<agent>` and `<config>` are `pi_rpc::resolve_homes()`. When writing a Gentle Shell file, its exact schema is respected (e.g. `permission-mode.json` rejects extra keys) and it is written atomically (temp file + `rename`), as `shared/gentle_state.rs` and `profiles/files.rs` do.

## Other integrations

### Provider sign-in (`crates/pi-rpc/src/login.rs`)

Pi has no RPC login command. `LoginHelper::spawn(spec, homes)` writes `login-helper.mjs` (embedded with `include_str!`) to the temp directory as `gentle-shell-gpui-login-<version>.mjs` and runs it with Node: the one from the self-contained install (`bundled_install`) or `resolve_node`. Arguments: the script and the Pi package (`pi_package`); environment: `PI_CODING_AGENT_DIR=<agent>` and the combined PATH. The helper uses Pi's `ModelRuntime.login` and writes `auth.json` itself.

Its own JSONL protocol: the app sends `list`, `login` (`provider`, `authType: oauth|api_key`), `logout`, `response` (`id`, `value`), `cancel_prompt`, `cancel`; it receives `ready`, `providers`, `prompt` (`text`, `secret`, `select`, `manual_code`), `prompt_dismissed`, `event` (`info`, `auth_url`, `device_code`, `progress`), `done`, `error`. The UI is in `features/providers/`. After a change, the window calls `reload_models` on every tab: a backend only reads credentials at startup.

### Engram (`features/memory/client.rs`)

Blocking HTTP GET (ureq, 8 s, no redirects) on the background executor. Base URL: `ENGRAM_URL`, otherwise `http://127.0.0.1:${ENGRAM_PORT}`, default port `7437`. `ENGRAM_HTTP_TOKEN` adds `Authorization: Bearer`. Routes: `/project/current?cwd=`, `/observations/recent?…&limit=60`, `/search?q=…&limit=40` (with `project=` or `all_projects=true`). The `Observation` model, `encode` and `parse_observations` are in `gentle_core::engram`. The app does not start Engram: Gentle Shell does it with the first session.

### Subscription usage (`features/usage/`)

`credentials.rs` reads `<agent>/auth.json` and resolves keys the way Pi does (`$VAR`, `!command`); `fetch.rs` makes one GET per card (20 s, no redirects so the token never leaves the origin, `User-Agent: gentle-pi`): Codex at `https://chatgpt.com/backend-api/wham/usage` and NaN at `https://cloud-api.nan.builders/api/usage/quota`. The parsers are `gentle_core::usage::subscription`.

### Gentle Shell update (`features/updates/`)

Same as the launcher's startup check (`lib/gentle-shell-startup-update.ts`): `registry.rs` requests `https://registry.npmjs.org/gentle-pi/latest` (5 s, stable versions only); `cache.rs` stores `startupReleaseCheck` (`checkedAt`, `latest`, valid for one day) and `skippedStartupRelease` in the launcher config, so the terminal and the app agree. The offer is decided in `gentle_core::version::offer_for`. `settings/upgrade.rs` performs the install with `gentle-shell upgrade`, then `forget_cached` and `SettingsEvent::BackendUpdated` → `restart_for_update` on every tab.

### App update (`features/app_update/`)

`network.rs` queries `https://api.github.com/repos/ismaelosuna7824/gentle-shell-gpui/releases/latest`; `release.rs` (pure) accepts only published `vX.Y.Z` tags, picks the asset with `asset_name` (`gentle-shell-gpui-<v>-macos-<arch>.zip`, `-windows-x86_64.zip`, `-linux-x86_64.tar.gz`) and verifies it against `SHA256SUMS`. `archive.rs` extracts, `install.rs` replaces the binary per OS and `relaunch.rs` reopens the app on exit. 6 h cache and skipped version in `app-update.json`. Dev versions (`is_dev`) are not checked automatically.

### Installers (`features/setup/`)

`run_checks()` evaluates each `CheckId` in `check_order` order (Node, Go on Windows only, GentleShell, Setup, Provider, Git, ClaudeBridge, ClaudeLogin, Engram) with minimums `MIN_NODE` 22.19.0, `MIN_GO` 1.25.10, `MIN_PI` 1.2.0, `MIN_ENGRAM` 3.0.0. Each `Check` carries a `Status` and a `Fix` (`Auto`, `Terminal(cmd)`, `SignIn`, `None`). `install(id, log)`:

| Check | Install |
| --- | --- |
| Node | `fetch.rs` `install_node`: Node 22 LTS from `https://nodejs.org/dist`, checksum-verified, into `toolchain/node` |
| GentleShell | `npm install -g gentle-pi --allow-scripts=gentle-pi --foreground-scripts`; `install_prefix` adds `--prefix` (the managed Node's, or `toolchain/npm-global` if the global one requires root) |
| Setup | `gentle-shell setup` |
| ClaudeBridge | `gentle-shell install npm:<bridge>` |
| Engram | `Gentleman-Programming/engram` release from GitHub, verified, into `toolchain/bin` |
| Maestro (Settings → UI testing, not part of this checklist) | Java 17+ is looked up (`toolchain/java`, `JAVA_HOME`, macOS `/usr/libexec/java_home -v 17+`, the `java` on the PATH, each checked by its `release` file); without one, the latest Temurin 21 JRE from `api.adoptium.net`, checked against Adoptium's sha256, into `toolchain/java`. The CLI is looked up (`toolchain/maestro/bin`, PATH, `~/.maestro/bin`); without one, `maestro.zip` from `mobile-dev-inc/maestro`'s latest GitHub release, checked against its `checksums_sha256.txt`, into `toolchain/maestro`. Then `gentle-shell mcp add maestro --env JAVA_HOME=<home> --description … -- <maestro> mcp` (`cmd /c <maestro.bat> mcp` on Windows). `status` reports the CLI (and whether it is the app's own), Java and whether Pi's `mcp.json` has an enabled `maestro` server; `test_connection` runs `gentle-shell mcp list` and returns the `maestro:` line; `disconnect` runs `gentle-shell mcp remove maestro`; `remove` disconnects and deletes `toolchain/maestro` and `toolchain/java`. Install and disconnect emit `SettingsEvent::Changed`, so tabs restart when idle and load or drop the tools. |
| Music (Settings → Music, not part of this checklist) | macOS and Linux only. The player is looked up (`toolchain/music/bin`, `gentle-stream` on the PATH, Gentle Shell's `~/.gentle-shell/music/bin`) with the Python of its own virtualenv next to its real path. Install mirrors Gentle Shell's `/gentle:music` offer: uv 0.13.6 (its pinned sha256, `fetch.rs` `install_uv`) into `toolchain/music/uv`, then `uv tool install --python 3.13 'gentle-stream[samples] @ <pinned archive>'` with every uv folder inside `toolchain/music` and inherited `UV_*`/`PYTHON*`/`PIP_*` dropped; without the system FluidSynth library it retries without `[samples]`. Sampled instruments run `gentle-stream setup-samples`. Remove stops the player and deletes `toolchain/music`. |
| Provider | Providers screen |

Anything that needs `sudo` or an interactive login is delivered as `Fix::Terminal`, and `features/onboarding/` opens it in a terminal tab. After installing, `refresh_paths` (= `forget_cached` + `apply_toolchain_path`).

### Music (`features/music/`)

gentle-stream's background player (one per user, `gentle-stream daemon`) owns the audio and keeps playing after its clients leave. The app talks to it the way gentle-stream's own TUI does:

- **Socket.** `client.rs` connects to `player.sock` in the runtime folder (`gentle_core::music::runtime_dir`, same rule as gentle-stream) and speaks Python's `multiprocessing.connection`: 4-byte big-endian length frames, then a mutual HMAC-SHA256 challenge with the `authkey` file (the player challenges first; the app's 40-byte nonce comes from `/dev/urandom`). Requests are JSON: `state` (with `bands` for the spectrum), `toggle`, `next`, `set_volume`, `stop`. The player authenticates one client at a time with a listen queue of one, so a refused connect is retried briefly.
- **Helper.** Starting the player and turning a mood into a style run a short Python script (`HELPER` in `music/mod.rs`) with the player's own Python: `client.ensure()` (gentle-stream's startup, ready pipe included), then `GentleShell(binary=<launcher>).interpret_with_usage(mood, player.style)` and `set_style`. The launcher is `pi_rpc::resolve_backend`'s program; the call uses Gentle Shell's default model and reports its tokens and cost.
- **Polling.** Every 4 s while nothing plays (a player started from a terminal shows up), every 1.5 s while one plays, every 70 ms with 40 bands while the popover is open. One connection is kept and reopened after a drop.
- **Quitting.** The app stops a player it started; one started elsewhere keeps playing.

These are gentle-stream internals, not a public CLI (its CLI only has `ctl play|pause|toggle|next|stop|status`); the archive the app installs is pinned, so a player installed some other way may differ.

### Git (`features/git/`)

`git(cwd, args)` for reads (stderr discarded) and `run(cwd, args)` for side effects, with `GIT_TERMINAL_PROMPT=0` because there is no terminal for credentials. The parsers are in `gentle_core::git`. The session refreshes the snapshot when the turn ends and, at most every 1.5 s, when tools touch files (`refresh_git_soon`).

### Browser for Gentle Design (`features/browser/`)

The canvas is detected from the `gentle_design` result (`details.url`, `refreshed`, `stopped`) or from a `notify` "Design canvas: <url>" / "Design canvas refreshed: <url>" / "Design canvas stopped."; only loopback URLs are accepted (`http://127.0.0.1:` or `http://localhost:`). The window shows it in `BrowserPanel`. Implementation per OS: `macos.rs` (WKWebView as a subview), `windows.rs` (WebView2 in its own popup that follows the window), `linux.rs` (WebKitGTK as a child of the X11 window; on Wayland the canvas stays in the browser that Gentle Shell opened).

## Limits of an RPC host

- Helpers' model, tokens and cost arrive only from backends whose `capabilities` offer `list_subagents`; the token breakdown is unavailable while a helper is in flight. Older Shells keep the legacy `gentle-agents` widget without model/usage/cost.
- Claude subscription windows only arrive as response headers inside the TUI; the app sees the limit notices (`notify`) but not the full windows.
- Helpers and background jobs stop directly only when the backend offers `cancel_subagent`/`cancel_job`; without them the Agents panel keeps the legacy "ask the agent to run `subagent_cancel`" prompt, and jobs have no cancel.
- Auto mode may reject actions: input over RPC does not count as a live human decision.
- The permission mode of a live session changes over the Gentle Desktop RPC channel when the backend advertises `set_permission_mode`; a backend without that method keeps the legacy path (save `permission-mode.json` and restart). See “Permission mode” under RPC protocol.
- Gentle Shell does not play sounds for RPC hosts (`notificationContextAllowed`): `features/alerts/` plays them.
- `ctx.ui.custom()` screens do not arrive: they are rebuilt natively or hidden.
- New credentials are only read when the backend starts.

## How to add an integration

### A new status chip

If the extension already publishes `setStatus` or `setWidget`, it shows up on its own as a generic chip. To give it custom handling:

1. `session/events.rs` `handle_ui`: interpret the key (like `gentle:mode` or `gentle-agents`). The pure interpretation goes in `gentle_core` (like `PermissionMode::from_status` or `activity::parse`) with tests.
2. `session/state.rs`: the new state inside the matching group (`ExtensionUi` or `TurnActivity`).
3. `session/view/activity.rs`: exclude the key from the generic filter and draw the chip with `chip_popover` or `ui::chip`.
4. `session/lifecycle.rs` `start`: clear that state on every start.

If the data comes from an event or a tool result (like jobs), read it in `handle_event` / `TurnActivity`, not in `handle_ui`.

### A native screen for a `/` command

1. `crates/core/src/commands.rs`: a variant in `Screen` and a case in `route`; remove it from `hidden` if it was there. Update the tests (`routed_and_hidden_never_overlap`).
2. `workspace/overlays.rs` `open_screen`: open the screen for the new variant.
3. The screen, in its own `features/<name>/` folder, reading files via `pi_rpc::resolve_homes()`.
4. If it must ask the backend for something, emit an event that the window turns into `run_prompt` or `run_extension_command` (the `ProfilesEvent::RunCommand` and `SettingsEvent::ShellCommand` pattern), or use `send_command` for an RPC command.

The palette and the composer already use `route`/`hidden`; there is no need to touch them. A new Pi built-in goes in `session/builtins.rs` (`BUILTINS`, `Builtin`, `parse_builtin`) and `submit.rs` `run_builtin`.

### A new extension UI method

1. `crates/pi-rpc/src/record.rs`: a variant in `UiMethod` with the protocol field names (`#[serde(rename = …)]`); if it blocks, add it to `is_dialog`. Test in `ui_requests_map_wire_names`.
2. If it is a dialog: rendering and keyboard in `session/view/dialogs.rs`, focus in `events.rs` `prepare_dialog`, answer with an existing `UiAnswer` (add one in `lib.rs` only if the protocol requires it).
3. If it does not block: a case in `events.rs` `handle_ui`.

### A new RPC command

1. A method in `session/rpc.rs` that calls `self.request(json!({"type": …}), cx, |this, result, cx| …)` and uses `report` to show errors.
2. If it changes state read at startup, add it to `refresh_state`.
3. If the backend confirms it with an event, handle it in `handle_event` instead of assuming the result.

### A new Gentle Shell config file

1. Path built from `pi_rpc::resolve_homes()` (`agent` or `config`), never hard-coded paths; consider the project variant (`<cwd>/.pi/gentle-ai/`) if the shell has one.
2. Format and validation in `gentle_core` when they are pure (like `notifications`); reading, caching by `pi_rpc::file_cache::Stamp` and atomic writing in the owning feature.
3. If the shell has a command that changes the file, prefer it over writing the file (the `background.rs` pattern).
4. If the backend only reads it at startup, restart the tabs with `restart_when_idle` or offer `/reload`.
5. Add it to the file table in this document.

### A new HTTP service

Follow `memory/client.rs` and `usage/fetch.rs`: `ureq` with a global timeout, `max_redirects(0)` if there are credentials, `http_status_as_error(false)` and readable errors; always on the background executor; parsers and models in `gentle_core`.

## Checklist

- [ ] Child processes with `pi_rpc::command`; `PATH` = `pi_rpc::child_path()` when they look for user tools.
- [ ] Launcher subcommands with the arguments before `--mode` and the `LaunchSpec` `env`.
- [ ] Gentle Shell paths from `resolve_homes()`; exact schemas and atomic writes.
- [ ] Pure logic (parsers, decisions) in `gentle_core` with tests; IO in `app`.
- [ ] New `UiMethod` variants or keys with a decoding test; state cleared in `start`.
- [ ] Backend restarts with `restart_when_idle`, never in the middle of a turn.
- [ ] Visible RPC errors (`report`) and networking that tolerates being offline.
- [ ] File table and limits in this document kept up to date.
