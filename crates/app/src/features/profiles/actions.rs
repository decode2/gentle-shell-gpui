//! Store actions: writing `profiles.json`, Save, Use in this session, Make
//! default, the repository pin, Publish, and the name and leave flows. On the
//! RPC path the same actions are whole-store `set_config` writes guarded by
//! the last `writeRevision`. Make default and Use in this session are Gentle
//! Shell's `apply_profile` when it advertises it; otherwise Make default
//! stays on the files.

use gentle_core::profiles::{
    PinRead, ProfilesFile, Routing, create_profile, delete_profile, duplicate_profile,
    normalize_routing, parse_model_ref, parse_profiles, rename_profile, serialize_profiles,
    update_profile,
};
use gpui_kit::*;
use pi_rpc::desktop_rpc::{
    ApplyProfileResult, ConfigStore, ProfileApplyMode, ProfileNote, ProfileNoteLevel,
};

use super::apply::{make_default, plan_make_default};
use super::env::{ApplyAnswer, ProfileApply};
use super::files::{
    fallbacks_write, files_job_start, follow_fallbacks, profiles_write, read_orchestrator_settings,
    write_atomic,
};
use super::pins::{
    clear_pin, follow_renamed_pin, local_pin_write, read_pin_status, renamed_pin_write, write_pin,
};
use super::{Backend, Confirm, Leave, NameMode, ProfilesEvent, ProfilesView, Tone};

/// The committed declaration, as the RPC path names it.
const DECLARATION: &str = ".pi/gentle-ai/profile.json";

/// The stores Make default may have written through `apply_profile`.
const APPLIED_STORES: [ConfigStore; 5] = [
    ConfigStore::Profiles,
    ConfigStore::Pins,
    ConfigStore::Models,
    ConfigStore::Subagents,
    ConfigStore::ManagedAssets,
];

/// Gentle Shell's notes as one status line, in order.
fn notes_text(notes: &[ProfileNote]) -> String {
    notes
        .iter()
        .map(|note| note.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl ProfilesView {
    /// Write `next` to profiles.json (or replace the `profiles` store over
    /// RPC), then adopt it.
    fn write_store(
        &mut self,
        next: ProfilesFile,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        if self.is_rpc() {
            let write = match profiles_write(&next) {
                Ok(write) => write,
                Err(error) => return self.say(format!("Not saved: {error}"), Tone::Bad),
            };
            self.rpc_write(write, cx, move |this, result, cx| {
                // The Shell's normalized store is the truth; the draft it was
                // built from stands in only if that cannot be read back.
                this.file = result
                    .state
                    .value
                    .as_ref()
                    .and_then(|value| parse_profiles(&value.to_string()))
                    .map_or(next, |(file, _)| file);
                done(this, cx);
                // A pin's evaluation depends on which profiles exist.
                this.reread(&[ConfigStore::Pins], cx);
            });
            return;
        }
        let Some(env) = self.env.clone() else { return };
        let text = serialize_profiles(&next);
        self.run_io(
            cx,
            move || write_atomic(&env.profiles_path(), &text).map_err(|e| e.to_string()),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.file = next;
                    cx.emit(ProfilesEvent::Changed);
                    done(this, cx);
                }
                Err(error) => {
                    this.say(format!("profiles.json was not written: {error}"), Tone::Bad)
                }
            },
        );
    }

    /// The draft as the store would hold it (create or update), or `None`
    /// when it already matches.
    fn saved_draft(&self) -> Result<Option<ProfilesFile>, String> {
        let Some(name) = &self.selected else {
            return Ok(None);
        };
        let normalized = normalize_routing(&self.draft.to_json()).unwrap_or_default();
        let stored = self
            .file
            .get(name)
            .map(|r| normalize_routing(&r.to_json()).unwrap_or_default());
        if stored.as_ref() == Some(&normalized) {
            return Ok(None);
        }
        if self.file.has(name) {
            update_profile(&self.file, name, self.draft.clone()).map(Some)
        } else {
            create_profile(&self.file, name, self.draft.clone()).map(Some)
        }
    }

    /// Save pending edits, then continue with `then` (told whether it saved).
    pub(super) fn save_then(
        &mut self,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
    ) {
        match self.saved_draft() {
            Err(error) => self.say(format!("Not saved: {error}"), Tone::Bad),
            Ok(None) => then(self, false, cx),
            Ok(Some(next)) => self.write_store(next, cx, move |this, cx| {
                this.stored = this.draft.clone();
                then(this, true, cx);
            }),
        }
        cx.notify();
    }

    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        if self.change_count() == 0 {
            self.say("Nothing to save.", Tone::Info);
            cx.notify();
            return;
        }
        self.save_then(cx, |this, saved, _| {
            let name = this.selected.clone().unwrap_or_default();
            this.say(
                if saved {
                    format!("Saved \"{name}\".")
                } else {
                    "Nothing to save.".into()
                },
                Tone::Good,
            );
        });
    }

    pub(super) fn use_in_session(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.selected.clone() else {
            return;
        };
        if self.shell_applies(cx) {
            self.save_then(cx, move |this, saved, cx| {
                if saved {
                    this.say(format!("Saved \"{name}\"."), Tone::Good);
                }
                this.shell_apply(ProfileApply::new(name, ProfileApplyMode::Session), cx);
            });
            return;
        }
        self.save_then(cx, move |this, saved, cx| {
            let orchestrator = this.draft.orchestrator();
            let model = orchestrator
                .and_then(|e| e.model.as_deref())
                .and_then(parse_model_ref);
            let thinking = model
                .as_ref()
                .and_then(|_| orchestrator.and_then(|e| e.thinking.clone()));
            let prefix = if saved {
                format!("Saved \"{name}\". ")
            } else {
                String::new()
            };
            let live = match &model {
                Some((provider, id)) => format!(" Switching this session to {provider}/{id}."),
                None => " The orchestrator is left untouched.".into(),
            };
            this.say(
                format!("{prefix}Using \"{name}\" in this session.{live}"),
                Tone::Good,
            );
            cx.emit(ProfilesEvent::UseInSession {
                profile: name,
                model,
                thinking,
            });
        });
    }

    pub(super) fn request_make_default(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.selected.clone() else {
            return;
        };
        if self.shell_applies(cx) {
            self.save_then(cx, move |this, saved, cx| {
                if saved {
                    this.say(format!("Saved \"{name}\"."), Tone::Good);
                }
                this.shell_apply(ProfileApply::new(name, ProfileApplyMode::Default), cx);
            });
            return;
        }
        self.save_then(cx, move |this, saved, cx| {
            let Some(env) = this.env.clone() else { return };
            let file = this.file.clone();
            let config = file.get(&name).cloned().unwrap_or_default();
            if saved {
                this.say(format!("Saved \"{name}\"."), Tone::Good);
            }
            this.run_io(
                cx,
                move || plan_make_default(&env, &file, &name, &config),
                |this, plan, _| this.confirm = Some(Confirm::MakeDefault(plan)),
            );
        });
    }

    pub(super) fn make_default(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(env) = self.env.clone() else { return };
        // Make default writes profiles.json itself, so on the RPC path it
        // must not race the Shell's writer in either direction: it starts
        // only from the freshest store (no write open, no read owed), and
        // holds every store write until it finishes.
        let file = match &mut self.backend {
            Backend::Rpc(remote) => match files_job_start(remote) {
                Ok(file) => file,
                Err(error) => {
                    self.say(error, Tone::Info);
                    cx.notify();
                    return;
                }
            },
            _ => self.file.clone(),
        };
        self.run_io(
            cx,
            move || {
                let outcome = make_default(&env, &file, &name);
                (
                    outcome,
                    read_pin_status(&env.cwd),
                    read_orchestrator_settings(&env.settings_path()),
                )
            },
            |this, (outcome, pin, pi_default), cx| {
                if let Backend::Rpc(remote) = &mut this.backend {
                    remote.release();
                    // Make default wrote profiles.json and the pin directly:
                    // re-read them through the Shell so the next RPC write is
                    // guarded by the revisions it left, never by older ones.
                    this.reread(
                        &[
                            ConfigStore::Profiles,
                            ConfigStore::Pins,
                            ConfigStore::Models,
                            ConfigStore::Subagents,
                            ConfigStore::ManagedAssets,
                        ],
                        cx,
                    );
                } else {
                    this.pin = pin;
                }
                this.pi_default = pi_default;
                match outcome {
                    Ok(outcome) => {
                        this.file = outcome.file;
                        this.say(outcome.message, Tone::Good);
                        cx.emit(ProfilesEvent::Changed);
                        if let Some(entry) = outcome.orchestrator
                            && let Some(model) = entry.model.as_deref().and_then(parse_model_ref)
                        {
                            cx.emit(ProfilesEvent::SwitchOrchestrator {
                                model,
                                thinking: entry.thinking,
                            });
                        }
                    }
                    Err(error) => {
                        this.say(error, Tone::Bad);
                        cx.emit(ProfilesEvent::Changed);
                    }
                }
            },
        );
    }

    /// Ask Gentle Shell to apply a saved profile (`apply_profile`). Every
    /// store write waits until it answers.
    pub(super) fn shell_apply(&mut self, request: ProfileApply, cx: &mut Context<Self>) {
        let name = request.profile.clone();
        let sent = self.with_remote(cx, |remote, port| remote.apply(port, request));
        match sent {
            Some(Ok(_)) => self.say(format!("Applying \"{name}\" in Gentle Shell…"), Tone::Info),
            Some(Err(failure)) => self.say(
                format!("\"{name}\" was not applied: {}", failure.message()),
                Tone::Bad,
            ),
            None => {}
        }
        self.sync_busy();
        cx.notify();
    }

    /// Gentle Shell answered an apply: ask the human with its exact dialog,
    /// or show its notes. Nothing is ever confirmed here.
    pub(super) fn apply_answered(
        &mut self,
        request: ProfileApply,
        result: ApplyProfileResult,
        cx: &mut Context<Self>,
    ) {
        let name = request.profile.clone();
        let mode = request.mode;
        match ApplyAnswer::new(&request, result) {
            ApplyAnswer::Confirm {
                title,
                message,
                retry,
            } => {
                self.status = None;
                self.confirm = Some(super::Confirm::ApplyProfile {
                    title,
                    message,
                    retry,
                });
            }
            ApplyAnswer::Done { applied, notes } => {
                let text = notes_text(&notes);
                if applied {
                    let text = if text.is_empty() {
                        format!("Applied \"{name}\".")
                    } else {
                        text
                    };
                    self.say(text, Tone::Good);
                    cx.emit(ProfilesEvent::Changed);
                    if mode == ProfileApplyMode::Default {
                        self.refresh_after_shell_apply(cx);
                    }
                } else {
                    let text = if text.is_empty() {
                        format!("Gentle Shell did not apply \"{name}\".")
                    } else {
                        text
                    };
                    let failed = notes
                        .iter()
                        .any(|note| note.level != ProfileNoteLevel::Info);
                    self.say(text, if failed { Tone::Bad } else { Tone::Info });
                    // A rollback may have restored part of what was written.
                    if mode == ProfileApplyMode::Default {
                        self.refresh_after_shell_apply(cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Gentle Shell's Make default wrote the stores and Pi's settings: read
    /// them again, so the screen and the next write follow what it left.
    fn refresh_after_shell_apply(&mut self, cx: &mut Context<Self>) {
        self.reread(&APPLIED_STORES, cx);
        let Some(env) = self.env.clone() else { return };
        self.run_io(
            cx,
            move || read_orchestrator_settings(&env.settings_path()),
            |this, pi_default, _| this.pi_default = pi_default,
        );
    }

    pub(super) fn toggle_repo_pin(&mut self, cx: &mut Context<Self>) {
        let (Some(name), Some(env)) = (self.selected.clone(), self.env.clone()) else {
            return;
        };
        if self.is_rpc() {
            self.save_then(cx, move |this, _, cx| {
                let (write, pinned) = match local_pin_write(this.pin.as_ref(), &name) {
                    Ok(write) => write,
                    Err(error) => return this.say(error, Tone::Bad),
                };
                let message = if pinned {
                    "Removed the clone pin. This clone falls back to the repository declaration, then to the global routing.".to_string()
                } else {
                    format!(
                        "Pinned \"{name}\" for this clone. Subagent launches here resolve it; the orchestrator and other repositories keep their global routing."
                    )
                };
                this.rpc_write(write, cx, move |this, _, _| this.say(message, Tone::Good));
            });
            return;
        }
        self.save_then(cx, move |this, _, cx| {
            this.run_io(
                cx,
                move || {
                    let Some(status) = read_pin_status(&env.cwd) else {
                        return (Err("This folder is not inside a Git worktree, so nothing was pinned.".to_string()), None);
                    };
                    let pinned = status.local == PinRead::Valid(name.clone());
                    let result = if pinned {
                        clear_pin(&status.local_path).map_err(|e| e.to_string()).map(|_| {
                            format!(
                                "Removed the local pin from {}. This clone falls back to the repository declaration, then to the global routing.",
                                status.local_path.display()
                            )
                        })
                    } else {
                        write_pin(&status.local_path, &name).map(|_| {
                            format!(
                                "Pinned \"{name}\" for this clone in {}. Subagent launches here resolve it; the orchestrator and other repositories keep their global routing.",
                                status.local_path.display()
                            )
                        })
                    };
                    (result, read_pin_status(&env.cwd))
                },
                |this, (result, pin), cx| {
                    match result {
                        Ok(message) => {
                            this.pin = pin;
                            this.say(message, Tone::Good);
                            cx.emit(ProfilesEvent::Changed);
                        }
                        Err(error) => this.say(error, Tone::Bad),
                    }
                },
            );
        });
    }

    pub(super) fn publish(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.selected.clone() else {
            return;
        };
        self.save_then(cx, move |this, _, cx| {
            this.say(
                format!("Sent /gentle:profiles publish {name}. Refresh to see the declaration."),
                Tone::Info,
            );
            cx.emit(ProfilesEvent::RunCommand(format!(
                "/gentle:profiles publish {name}"
            )));
        });
    }

    pub(super) fn clear_session(&mut self, cx: &mut Context<Self>) {
        self.say("Sent /gentle:profiles clear.", Tone::Info);
        cx.emit(ProfilesEvent::RunCommand("/gentle:profiles clear".into()));
        cx.notify();
    }

    /// Leaving the shown profile with unsaved edits asks first.
    pub(super) fn leave(&mut self, leave: Leave, window: &mut Window, cx: &mut Context<Self>) {
        if self.change_count() > 0 {
            self.confirm = Some(Confirm::Unsaved(leave));
        } else {
            self.proceed(leave, window, cx);
        }
        cx.notify();
    }

    pub(super) fn proceed(&mut self, leave: Leave, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm = None;
        match leave {
            Leave::Switch(name) => {
                self.select(Some(name.clone()), false);
                self.say(format!("Showing \"{name}\"."), Tone::Info);
            }
            Leave::Close => cx.emit(ProfilesEvent::Close),
            Leave::New => self.open_name(NameMode::New, window, cx),
            Leave::Duplicate => self.open_name(NameMode::Duplicate, window, cx),
        }
        cx.notify();
    }

    pub(super) fn open_name(
        &mut self,
        mode: NameMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name_mode = Some(mode);
        self.name_input.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn submit_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = self.name_mode else { return };
        let answer = self.name_input.read(cx).value().trim().to_string();
        let current = self.selected.clone().unwrap_or_default();
        let result = match mode {
            NameMode::New if answer.is_empty() => {
                Err("No profile created: no name entered.".to_string())
            }
            NameMode::New => {
                create_profile(&self.file, &answer, Routing::default()).map(|f| (f, answer.clone()))
            }
            NameMode::Duplicate => {
                let to = if answer.is_empty() {
                    format!("{current}-copy")
                } else {
                    answer.clone()
                };
                duplicate_profile(&self.file, &current, &to).map(|f| (f, to))
            }
            NameMode::Rename if answer.is_empty() => Err(format!(
                "Profile \"{current}\" unchanged: no new name entered."
            )),
            NameMode::Rename => {
                rename_profile(&self.file, &current, &answer).map(|f| (f, answer.clone()))
            }
        };
        self.name_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        match result {
            Err(error) => self.say(error, Tone::Bad),
            Ok((next, name)) => {
                self.name_mode = None;
                let from = current.clone();
                self.write_store(next, cx, move |this, cx| {
                    match mode {
                        NameMode::Rename => {
                            let keep = this.change_count() > 0;
                            this.select(Some(name.clone()), keep);
                            this.say(format!("Profile \"{from}\" renamed to \"{name}\"."), Tone::Good);
                            if this.is_rpc() {
                                this.follow_rename_rpc(&from, &name, cx);
                            } else if let Some(env) = this.env.clone() {
                                let (old, new) = (from.clone(), name.clone());
                                this.run_io(
                                    cx,
                                    move || {
                                        follow_fallbacks(&env.config_home, &old, Some(&new));
                                        let follow = follow_renamed_pin(&env.cwd, &old, &new);
                                        (follow, read_pin_status(&env.cwd))
                                    },
                                    move |this, ((followed, declared), pin), _| {
                                        this.pin = pin;
                                        let mut message = format!("Profile \"{from}\" renamed to \"{name}\".");
                                        for path in followed {
                                            message.push_str(&format!(" Updated the clone pin {}.", path.display()));
                                        }
                                        for path in declared {
                                            message.push_str(&format!(
                                                " The committed declaration {} still names \"{from}\"; Publish to repo republishes it.",
                                                path.display()
                                            ));
                                        }
                                        this.say(message, Tone::Good);
                                    },
                                );
                            }
                        }
                        NameMode::New | NameMode::Duplicate => {
                            this.select(Some(name.clone()), false);
                            let verb = if mode == NameMode::New { "created".to_string() } else { format!("duplicated from \"{from}\"") };
                            this.say(format!("Profile \"{name}\" {verb}."), Tone::Good);
                        }
                    }
                });
            }
        }
        cx.notify();
    }

    pub(super) fn request_delete(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.selected.clone() else {
            return;
        };
        if self.file.active.as_deref() == Some(name.as_str()) {
            self.say(
                format!("Profile {name} is active. Apply another profile before deleting it."),
                Tone::Bad,
            );
        } else if let Some(status) = &self.pin
            && let layers = [
                ("local", &status.local, &status.local_path),
                ("repo", &status.repo, &status.repo_path),
            ]
            .into_iter()
            .filter(|(_, read, _)| **read == PinRead::Valid(name.clone()))
            .map(|(layer, _, path)| format!("{layer} pin at {}", path.display()))
            .collect::<Vec<_>>()
            && !layers.is_empty()
        {
            self.say(
                format!(
                    "Profile \"{name}\" is pinned for this repository ({}). Remove the pin first (Use in this repo for the clone pin, Publish to repo for the declaration), then delete it.",
                    layers.join(" and ")
                ),
                Tone::Bad,
            );
        } else {
            self.confirm = Some(Confirm::Delete(name));
        }
        cx.notify();
    }

    pub(super) fn delete(&mut self, name: String, cx: &mut Context<Self>) {
        self.confirm = None;
        match delete_profile(&self.file, &name) {
            Err(error) => self.say(format!("Profile not deleted: {error}"), Tone::Bad),
            Ok(next) => self.write_store(next, cx, move |this, cx| {
                let first = this.file.names().next().map(str::to_string);
                this.select(first, false);
                this.say(format!("Deleted \"{name}\"."), Tone::Good);
                if this.is_rpc() {
                    this.follow_fallbacks_rpc(&name, None, cx);
                } else if let Some(env) = this.env.clone() {
                    this.run_io(
                        cx,
                        move || follow_fallbacks(&env.config_home, &name, None),
                        |_, _, _| {},
                    );
                }
            }),
        }
        cx.notify();
    }

    /// A rename over RPC: the fallbacks and the clone pin follow it; the
    /// committed declaration is only reported, like the direct-file path.
    fn follow_rename_rpc(&mut self, from: &str, to: &str, cx: &mut Context<Self>) {
        self.follow_fallbacks_rpc(from, Some(to), cx);
        let mut message = format!("Profile \"{from}\" renamed to \"{to}\".");
        if self.pin.as_ref().map(|pin| &pin.repo) == Some(&PinRead::Valid(from.to_string())) {
            message.push_str(&format!(
                " The committed declaration {DECLARATION} still names \"{from}\"; Publish to repo republishes it."
            ));
        }
        self.say(message.clone(), Tone::Good);
        if let Some(write) = renamed_pin_write(self.pin.as_ref(), from, to) {
            self.rpc_write(write, cx, move |this, _, _| {
                this.say(format!("{message} Updated the clone pin."), Tone::Good);
            });
        }
    }

    /// `profile_fallbacks` follows a rename or delete over RPC, from the
    /// store as last read; a missing or invalid store is left alone.
    fn follow_fallbacks_rpc(&mut self, from: &str, to: Option<&str>, cx: &mut Context<Self>) {
        let write = self
            .remote()
            .and_then(|remote| remote.state(ConfigStore::ProfileFallbacks))
            .and_then(|state| fallbacks_write(state, from, to));
        match write {
            None => {}
            Some(Err(error)) => self.say(
                format!("The profile fallbacks were not updated: {error}"),
                Tone::Bad,
            ),
            Some(Ok(write)) => {
                self.rpc_write(write, cx, |_, _, _| {});
            }
        }
    }

    /// `proceed` for paths that run after async I/O and have no window.
    pub(super) fn proceed_without_window(&mut self, leave: Leave, cx: &mut Context<Self>) {
        match leave {
            Leave::Switch(name) => {
                self.select(Some(name.clone()), false);
                self.say(format!("Saved. Showing \"{name}\"."), Tone::Info);
            }
            Leave::Close => cx.emit(ProfilesEvent::Close),
            Leave::New => self.name_mode = Some(NameMode::New),
            Leave::Duplicate => self.name_mode = Some(NameMode::Duplicate),
        }
        cx.notify();
    }
}
