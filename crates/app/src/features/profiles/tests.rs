//! File effects end to end, in a scratch directory: the store, seeding,
//! settings, Make default, managed agent files and pins.

use std::fs;
use std::path::PathBuf;

use gentle_core::profiles::{
    AgentSource, PinRead, RouteEntry, Routing, assignment_names, bootstrap_profiles,
    create_profile, evaluate_pin, serialize_profiles, update_frontmatter_routing,
};
use pi_rpc::desktop_rpc::{
    ConfigErrorCode, ConfigLayer, ConfigLayerSource, ConfigScope, ConfigState, ConfigStatus,
    ConfigStore, ConfigWrite, ConfigWriteTarget, ConfigWriteTargetState, PinSource,
    SetConfigResult,
};
use serde_json::{Value, json};

use super::apply::{
    SettingsWrite, apply_orchestrator_settings, make_default, sha256_hex, update_managed_ownership,
};
use super::env::{
    ConfigChange, ConfigFailure, ConfigOutcome, ConfigPort, ConfigTicket, Env, Landed, Remote,
    SCREEN_STORES, discover_agents,
};
use super::files::{
    RpcLoad, StoreRead, fallbacks_write, files_job_start, follow_fallbacks, followed_fallbacks,
    profiles_write, read_store, rpc_load, seed_profiles, unique_token, write_atomic,
};
use super::pins::{local_pin_write, pin_status_at, renamed_pin_write, rpc_pin_status, write_pin};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gsg-profiles-{tag}-{}", unique_token()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn env(&self) -> Env {
        Env {
            cwd: self.0.join("project"),
            agent_home: self.0.join("agent"),
            config_home: self.0.join("config"),
            home: self.0.join("home"),
            package_root: None,
        }
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn entry(model: Option<&str>, thinking: Option<&str>, tier: Option<&str>) -> RouteEntry {
    RouteEntry {
        model: model.map(str::to_string),
        thinking: thinking.map(str::to_string),
        tier: tier.map(str::to_string),
    }
}

#[test]
fn save_is_atomic_and_keeps_unknown_top_level_keys() {
    let scratch = Scratch::new("save");
    let path = scratch.write(
        "config/profiles.json",
        r#"{"kind":"gentle-pi.agent_model_profiles","version":1,"futureKey":{"a":1},"profiles":{"p":{"w":"a/b"}}}"#,
    );
    let StoreRead::Valid(file, _) = read_store(&path) else {
        panic!("store should parse")
    };
    let next = create_profile(
        &file,
        "q",
        Routing(vec![(
            "orchestrator".into(),
            entry(Some("x/y"), Some("max"), None),
        )]),
    )
    .unwrap();
    write_atomic(&path, &serialize_profiles(&next)).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("}\n"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["futureKey"], json!({"a": 1}));
    assert_eq!(value["profiles"]["p"]["w"], json!({"model": "a/b"}));
    assert_eq!(
        value["profiles"]["q"]["orchestrator"],
        json!({"model": "x/y", "thinking": "max"})
    );
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["kind", "version", "profiles", "futureKey"]);
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    // Identical bytes are not rewritten.
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    write_atomic(&path, &text).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
}

#[test]
fn seeds_current_from_saved_and_materialized_routing() {
    let scratch = Scratch::new("seed");
    let env = scratch.env();
    scratch.write(
        "config/models.json",
        r#"{"gentle-ai-worker":{"model":"openai/gpt-6-sol","thinking":"high"}}"#,
    );
    scratch.write(
        "agent/agents/gentle-ai-explore.md",
        "---\nname: gentle-ai-explore\ndescription: d\nmodel: anthropic/haiku\nthinking: low\n---\nbody\n",
    );
    scratch.write(
        "agent/agents/gentle-ai-worker.md",
        "---\nname: gentle-ai-worker\nmodel: ignored/model\n---\n",
    );
    scratch.write("agent/agents/plain.md", "---\nname: plain\n---\n");
    scratch.write(
        "agent/subagents.json",
        r#"{"model_profiles":{"gentle-ai-explore":{"effort":"medium"}}}"#,
    );
    scratch.write(
        "project/.pi/agents/proj.md",
        "---\nname: \"proj\"\npackage: team\n---\n",
    );
    let agents = discover_agents(&env);
    let names = assignment_names(&agents);
    assert_eq!(&names[..2], ["review-refuter", "review-validator"]);
    assert!(names.contains(&"team.proj".to_string()));
    let file = seed_profiles(&env, &agents);
    assert_eq!(file.active.as_deref(), Some("current"));
    let current = file.get("current").unwrap();
    assert_eq!(
        current.get("gentle-ai-worker"),
        Some(&entry(Some("openai/gpt-6-sol"), Some("high"), None))
    );
    // subagents.json effort wins over frontmatter; model falls back to it.
    assert_eq!(
        current.get("gentle-ai-explore"),
        Some(&entry(Some("anthropic/haiku"), Some("medium"), None))
    );
    assert!(!current.contains("plain"));
    // Nothing routed: seeded but not active.
    let empty = Scratch::new("seed-empty");
    let env = empty.env();
    let file = seed_profiles(&env, &discover_agents(&env));
    assert_eq!(file.active, None);
    assert_eq!(file.get("current"), Some(&Routing::default()));
}

#[test]
fn make_default_settings_merge_keeps_other_keys() {
    let scratch = Scratch::new("settings");
    let path = scratch.write(
        "agent/settings.json",
        "{\n  \"theme\": \"dark\",\n  \"defaultModel\": \"old\",\n  \"defaultThinkingLevel\": \"low\",\n  \"packages\": [\"npm:x\"]\n}\n",
    );
    let written = apply_orchestrator_settings(
        &path,
        &entry(Some("openai/gpt-6-sol"), Some("high"), Some("fast")),
    )
    .unwrap();
    assert_eq!(written, SettingsWrite::Written);
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "theme",
            "defaultModel",
            "defaultThinkingLevel",
            "packages",
            "defaultProvider"
        ]
    );
    assert_eq!(value["defaultProvider"], "openai");
    assert_eq!(value["defaultModel"], "gpt-6-sol");
    assert_eq!(value["defaultThinkingLevel"], "high");
    assert_eq!(value["packages"], json!(["npm:x"]));
    assert_eq!(
        apply_orchestrator_settings(&path, &entry(Some("openai/gpt-6-sol"), Some("high"), None))
            .unwrap(),
        SettingsWrite::Unchanged
    );
    apply_orchestrator_settings(&path, &entry(Some("openai/gpt-6-sol"), None, None)).unwrap();
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(value.get("defaultThinkingLevel").is_none());
    assert_eq!(
        apply_orchestrator_settings(&path, &entry(None, Some("high"), None)).unwrap(),
        SettingsWrite::Unchanged
    );
    assert!(apply_orchestrator_settings(&path, &entry(Some("no-provider"), None, None)).is_err());
    let broken = scratch.write("agent/broken.json", "{nope");
    assert!(apply_orchestrator_settings(&broken, &entry(Some("a/b"), None, None)).is_err());
    assert_eq!(fs::read_to_string(&broken).unwrap(), "{nope");
}

#[test]
fn make_default_writes_store_routing_agents_and_settings() {
    let scratch = Scratch::new("default");
    let env = scratch.env();
    fs::create_dir_all(&env.cwd).unwrap();
    let worker = scratch.write(
        "agent/agents/worker.md",
        "---\nname: worker\ndescription: w\nmodel: old/model\n---\nbody\n",
    );
    scratch.write(
        "agent/agents/other.md",
        "---\nname: other\nthinking: low\n---\n",
    );
    scratch.write(
        "agent/subagents.json",
        r#"{"keep":true,"model_profiles":{"other":{"effort":"low"}}}"#,
    );
    scratch.write("agent/settings.json", r#"{"theme":"x"}"#);
    let config = Routing(vec![
        (
            "orchestrator".into(),
            entry(Some("openai/gpt-6-sol"), Some("high"), None),
        ),
        (
            "worker".into(),
            entry(Some("anthropic/opus"), Some("max"), None),
        ),
    ]);
    let file = create_profile(&bootstrap_profiles(&Routing::default()), "deep", config).unwrap();
    let outcome = make_default(&env, &file, "deep").unwrap();
    assert_eq!(outcome.file.active.as_deref(), Some("deep"));
    assert_eq!(
        outcome.orchestrator,
        Some(entry(Some("openai/gpt-6-sol"), Some("high"), None))
    );
    let StoreRead::Valid(stored, _) = read_store(&env.profiles_path()) else {
        panic!()
    };
    assert_eq!(stored.active.as_deref(), Some("deep"));
    let models: Value =
        serde_json::from_str(&fs::read_to_string(env.models_path()).unwrap()).unwrap();
    assert_eq!(
        models["worker"],
        json!({"model": "anthropic/opus", "thinking": "max"})
    );
    assert_eq!(
        fs::read_to_string(&worker).unwrap(),
        "---\nname: worker\ndescription: w\nmodel: anthropic/opus\nthinking: max\n---\nbody\n"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("agent/agents/other.md")).unwrap(),
        "---\nname: other\n---\n"
    );
    let subagents: Value =
        serde_json::from_str(&fs::read_to_string(env.subagents_path(AgentSource::User)).unwrap())
            .unwrap();
    assert_eq!(
        subagents,
        json!({"keep": true, "model_profiles": {"worker": {"model": "anthropic/opus", "effort": "max"}}})
    );
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(env.settings_path()).unwrap()).unwrap();
    assert_eq!(
        settings,
        json!({"theme": "x", "defaultProvider": "openai", "defaultModel": "gpt-6-sol", "defaultThinkingLevel": "high"})
    );
}

#[test]
fn managed_agent_files_keep_their_manifest_hash() {
    let scratch = Scratch::new("managed");
    let env = scratch.env();
    let original = "---\nname: worker\n---\n";
    let path = scratch.write("agent/agents/worker.md", original);
    scratch.write(
        "agent/gentle-ai/managed-assets.json",
        &json!({"schemaVersion": 1, "assets": {"agents/worker.md": sha256_hex(original.as_bytes())}}).to_string(),
    );
    let next = update_frontmatter_routing(original, &entry(Some("a/b"), None, None));
    assert!(update_managed_ownership(&env.agent_home, &path, original, &next).unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), next);
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(scratch.0.join("agent/gentle-ai/managed-assets.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["assets"]["agents/worker.md"],
        json!(sha256_hex(next.as_bytes()))
    );
    assert!(
        !scratch
            .0
            .join("agent/gentle-ai/managed-assets.lock")
            .exists()
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn pins_resolve_local_then_repo() {
    let scratch = Scratch::new("pin");
    let file = create_profile(
        &bootstrap_profiles(&Routing::default()),
        "team",
        Routing::default(),
    )
    .unwrap();
    let status = pin_status_at(scratch.0.join("root"), &scratch.0.join("git"));
    write_pin(&status.repo_path, "team").unwrap();
    scratch.write("git/gentle-ai/profile-pin.json", "{}");
    let status = pin_status_at(scratch.0.join("root"), &scratch.0.join("git"));
    let eval = evaluate_pin(Some(&status), &file);
    assert_eq!(
        eval.winner.as_ref().map(|w| (w.0, w.1.as_str())),
        Some(("repo", "team"))
    );
    assert_eq!(eval.invalid.len(), 1);
    write_pin(&status.local_path, "gone").unwrap();
    let status = pin_status_at(scratch.0.join("root"), &scratch.0.join("git"));
    let eval = evaluate_pin(Some(&status), &file);
    assert_eq!(eval.stale.len(), 1);
    assert_eq!(eval.winner.map(|w| w.0), Some("repo"));
    assert_eq!(
        fs::read_to_string(&status.repo_path).unwrap(),
        "{\n  \"kind\": \"gentle-pi.agent_model_profile_pin\",\n  \"version\": 1,\n  \"profile\": \"team\"\n}\n"
    );
}

// ---------------------------------------------------------------------------
// The Gentle Shell config backend: the screen's stores over Desktop RPC,
// driven through a recording port so no process, file or home is involved.
// ---------------------------------------------------------------------------

/// The session side of the seam, recorded: every request the screen makes.
#[derive(Default)]
struct FakePort {
    next: ConfigTicket,
    gets: Vec<(ConfigTicket, ConfigStore)>,
    sets: Vec<(ConfigTicket, ConfigWrite, String)>,
    refuse: Option<ConfigFailure>,
}

impl ConfigPort for FakePort {
    fn get(&mut self, store: ConfigStore) -> Result<ConfigTicket, ConfigFailure> {
        if let Some(failure) = self.refuse.clone() {
            return Err(failure);
        }
        self.next += 1;
        self.gets.push((self.next, store));
        Ok(self.next)
    }

    fn set(
        &mut self,
        write: ConfigWrite,
        expected_revision: String,
    ) -> Result<ConfigTicket, ConfigFailure> {
        if let Some(failure) = self.refuse.clone() {
            return Err(failure);
        }
        self.next += 1;
        self.sets.push((self.next, write, expected_revision));
        Ok(self.next)
    }
}

impl FakePort {
    fn ticket_for(&self, store: ConfigStore) -> ConfigTicket {
        self.gets
            .iter()
            .rev()
            .find(|(_, s)| *s == store)
            .map(|(t, _)| *t)
            .expect("store was read")
    }
}

fn profiles_value(names: &[&str], active: Option<&str>) -> Value {
    let profiles: serde_json::Map<String, Value> = names
        .iter()
        .map(|name| {
            (
                (*name).to_string(),
                json!({"orchestrator": {"model": "openai/gpt-6-sol"}}),
            )
        })
        .collect();
    let mut value = json!({
        "kind": "gentle-pi.agent_model_profiles",
        "version": 1,
        "profiles": profiles,
    });
    if let Some(active) = active {
        value["active"] = json!(active);
    }
    value
}

fn writable(
    store: ConfigStore,
    status: ConfigStatus,
    value: Option<Value>,
    revision: &str,
    targets: &[(ConfigWriteTarget, &str)],
) -> ConfigState {
    ConfigState {
        store,
        scope: store.scope(),
        read_only: false,
        status,
        revision: revision.into(),
        value,
        layers: match store.scope() {
            ConfigScope::LocalRepo => vec![ConfigLayerSource::Local, ConfigLayerSource::Repo],
            ConfigScope::GlobalProject => {
                vec![ConfigLayerSource::Global, ConfigLayerSource::Project]
            }
            ConfigScope::Global => vec![ConfigLayerSource::Global],
        }
        .into_iter()
        .map(|source| ConfigLayer { source, status })
        .collect(),
        write_targets: Some(
            targets
                .iter()
                .map(|(target, write_revision)| ConfigWriteTargetState {
                    target: *target,
                    status,
                    write_revision: (*write_revision).into(),
                })
                .collect(),
        ),
    }
}

fn read_only(store: ConfigStore, value: Value) -> ConfigState {
    ConfigState {
        store,
        scope: store.scope(),
        read_only: true,
        status: ConfigStatus::Ok,
        revision: format!("sha256:{store}"),
        value: Some(value),
        layers: Vec::new(),
        write_targets: None,
    }
}

fn profiles_state(names: &[&str], write_revision: &str) -> ConfigState {
    writable(
        ConfigStore::Profiles,
        ConfigStatus::Ok,
        Some(profiles_value(names, Some(names[0]))),
        &format!("sha256:read-{write_revision}"),
        &[(ConfigWriteTarget::Global, write_revision)],
    )
}

fn fallbacks_state(write_revision: &str) -> ConfigState {
    writable(
        ConfigStore::ProfileFallbacks,
        ConfigStatus::Ok,
        Some(json!({
            "kind": "gentle-pi.profile_model_fallbacks",
            "version": 1,
            "profiles": {"deep": {"orchestrator": ["anthropic/opus"]}, "other": {}},
        })),
        "sha256:fallbacks",
        &[(ConfigWriteTarget::Global, write_revision)],
    )
}

fn pins_state(local: Option<&str>, repo: Option<&str>) -> ConfigState {
    let layer = |pin: Option<&str>| {
        if pin.is_some() {
            ConfigStatus::Ok
        } else {
            ConfigStatus::Missing
        }
    };
    let winner = local.or(repo).map(|profile| {
        json!({"source": if local.is_some() { "local" } else { "repo" }, "profile": profile})
    });
    let mut value = json!({"resolved": winner.is_some(), "invalid": [], "stale": []});
    if let Some(winner) = winner {
        value["winner"] = winner;
    }
    ConfigState {
        store: ConfigStore::Pins,
        scope: ConfigScope::LocalRepo,
        read_only: false,
        status: if local.or(repo).is_some() {
            ConfigStatus::Ok
        } else {
            ConfigStatus::Missing
        },
        revision: "sha256:pins".into(),
        value: Some(value),
        layers: vec![
            ConfigLayer {
                source: ConfigLayerSource::Local,
                status: layer(local),
            },
            ConfigLayer {
                source: ConfigLayerSource::Repo,
                status: layer(repo),
            },
        ],
        write_targets: Some(vec![
            ConfigWriteTargetState {
                target: ConfigWriteTarget::Local,
                status: layer(local),
                write_revision: "sha256:local".into(),
            },
            ConfigWriteTargetState {
                target: ConfigWriteTarget::Repo,
                status: layer(repo),
                write_revision: "sha256:repo".into(),
            },
        ]),
    }
}

fn written(state: ConfigState, target: ConfigWriteTarget, write_revision: &str) -> SetConfigResult {
    let mut state = state;
    if let Some(targets) = state.write_targets.as_mut() {
        for entry in targets.iter_mut().filter(|entry| entry.target == target) {
            entry.write_revision = write_revision.into();
        }
    }
    SetConfigResult {
        state,
        write_target: target,
        write_revision: write_revision.into(),
        changed: true,
    }
}

/// A screen whose six stores were read and answered.
fn opened(port: &mut FakePort) -> Remote<&'static str> {
    let mut remote = Remote::new();
    remote.read(port, &SCREEN_STORES).unwrap();
    let answers = [
        profiles_state(&["deep", "other"], "sha256:g1"),
        fallbacks_state("sha256:f1"),
        pins_state(Some("deep"), None),
        read_only(
            ConfigStore::Models,
            json!({"worker": {"model": "anthropic/opus"}}),
        ),
        read_only(ConfigStore::Subagents, json!({"defaultMode": "task"})),
        read_only(
            ConfigStore::ManagedAssets,
            json!({"schemaVersion": 1, "assets": {"agents/worker.md": "ab"}}),
        ),
    ];
    for state in answers {
        let ticket = port.ticket_for(state.store);
        let store = state.store;
        assert!(matches!(
            remote.land(port, ticket, ConfigOutcome::Read(state)),
            Landed::Read(read) if read == store
        ));
    }
    assert!(!remote.loading());
    remote
}

fn rejected(code: ConfigErrorCode) -> ConfigOutcome {
    ConfigOutcome::Failed(ConfigFailure::Rejected {
        code,
        message: format!("{code} from the server"),
    })
}

#[test]
fn rpc_screen_reads_every_store_by_name() {
    let mut port = FakePort::default();
    let remote = opened(&mut port);
    let mut asked: Vec<ConfigStore> = port.gets.iter().map(|(_, store)| *store).collect();
    asked.sort_by_key(|store| store.as_str());
    let mut every = ConfigStore::ALL.to_vec();
    every.sort_by_key(|store| store.as_str());
    assert_eq!(asked, every, "each store once, by name");
    assert!(port.sets.is_empty(), "reading writes nothing");

    let RpcLoad::Ready { file, pin, notices } = rpc_load(&remote).unwrap() else {
        panic!("a present store needs no seed")
    };
    assert_eq!(file.names().collect::<Vec<_>>(), ["deep", "other"]);
    assert_eq!(file.active.as_deref(), Some("deep"));
    let pin = pin.expect("inside a worktree");
    assert_eq!(pin.local, PinRead::Valid("deep".into()));
    assert_eq!(pin.repo, PinRead::Missing);
    // Desktop resolves no pin file: the labels are logical, never absolute.
    assert!(pin.local_path.is_relative() && pin.repo_path.is_relative());
    // The read-only stores are shown, not edited.
    let summary = notices.join("\n");
    assert!(summary.contains("read-only"), "{summary}");
    assert!(summary.contains("1 saved agent route"), "{summary}");
    assert!(summary.contains("1 installer-managed file"), "{summary}");
}

#[test]
fn rpc_missing_profiles_store_is_seeded_from_the_saved_routing() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    let missing = writable(
        ConfigStore::Profiles,
        ConfigStatus::Missing,
        None,
        "sha256:none",
        &[(ConfigWriteTarget::Global, "sha256:absent")],
    );
    remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
    let ticket = port.ticket_for(ConfigStore::Profiles);
    remote.land(&mut port, ticket, ConfigOutcome::Read(missing));
    let RpcLoad::Seed(seed) = rpc_load(&remote).unwrap() else {
        panic!("a missing store is seeded")
    };
    assert_eq!(seed.active.as_deref(), Some("current"));
    assert_eq!(
        seed.get("current").and_then(|r| r.get("worker")),
        Some(&entry(Some("anthropic/opus"), None, None))
    );
    // The seed is a whole-store write guarded by the missing target's revision.
    remote
        .write(&mut port, profiles_write(&seed).unwrap(), "seed")
        .unwrap();
    let (_, write, expected) = port.sets.last().unwrap();
    assert_eq!(write.store(), ConfigStore::Profiles);
    assert_eq!(expected, "sha256:absent");

    // An invalid store is an error, never a seed or a write.
    let invalid = writable(
        ConfigStore::Profiles,
        ConfigStatus::Invalid,
        None,
        "sha256:bad",
        &[(ConfigWriteTarget::Global, "sha256:bad")],
    );
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
    let ticket = port.ticket_for(ConfigStore::Profiles);
    remote.land(&mut port, ticket, ConfigOutcome::Read(invalid));
    assert!(rpc_load(&remote).is_err());
    assert!(port.sets.is_empty());
}

#[test]
fn rpc_writes_carry_the_last_write_revision() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);

    // profiles: whole-store replacement guarded by the global target.
    let RpcLoad::Ready { file, pin, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    let next = create_profile(&file, "fresh", Routing::default()).unwrap();
    let ticket = remote
        .write(&mut port, profiles_write(&next).unwrap(), "save")
        .unwrap();
    let (sent, write, expected) = port.sets.last().unwrap().clone();
    assert_eq!(sent, ticket);
    assert_eq!(expected, "sha256:g1");
    let ConfigWrite::Profiles(data) = &write else {
        panic!("a whole profiles store")
    };
    assert!(data.as_value()["profiles"]["fresh"].is_object());
    assert!(data.as_value()["profiles"]["deep"].is_object());
    // One write per store at a time: a second one waits for the answer.
    assert!(
        remote
            .write(&mut port, profiles_write(&next).unwrap(), "again")
            .is_err()
    );
    let result = written(
        profiles_state(&["deep", "other", "fresh"], "sha256:g2"),
        ConfigWriteTarget::Global,
        "sha256:g2",
    );
    assert!(matches!(
        remote.land(&mut port, ticket, ConfigOutcome::Written(result)),
        Landed::Written("save", _)
    ));
    // The next write sends the revision the last write returned.
    remote
        .write(&mut port, profiles_write(&next).unwrap(), "next")
        .unwrap();
    assert_eq!(port.sets.last().unwrap().2, "sha256:g2");

    // profile_fallbacks: follows a rename, and a delete clears.
    let fallbacks = remote.state(ConfigStore::ProfileFallbacks).unwrap().clone();
    let renamed = fallbacks_write(&fallbacks, "deep", Some("deeper"))
        .expect("the renamed profile has chains")
        .unwrap();
    remote.write(&mut port, renamed, "follow").unwrap();
    let (_, write, expected) = port.sets.last().unwrap();
    assert_eq!(expected, "sha256:f1");
    let ConfigWrite::ProfileFallbacks(data) = write else {
        panic!("a whole fallbacks store")
    };
    assert_eq!(
        data.as_value()["profiles"],
        json!({"other": {}, "deeper": {"orchestrator": ["anthropic/opus"]}})
    );
    let cleared = fallbacks_write(&fallbacks, "other", None).unwrap().unwrap();
    let ConfigWrite::ProfileFallbacks(data) = &cleared else {
        panic!()
    };
    assert_eq!(
        data.as_value()["profiles"],
        json!({"deep": {"orchestrator": ["anthropic/opus"]}})
    );
    // A profile without chains needs no write at all.
    assert!(fallbacks_write(&fallbacks, "nobody", None).is_none());

    // pins: the clone layer only, set and cleared, guarded by its own target.
    let (clear, pinned) = local_pin_write(pin.as_ref(), "deep").unwrap();
    assert!(pinned, "deep is pinned here, so the toggle clears it");
    assert_eq!(clear, ConfigWrite::clear_pin(PinSource::Local));
    remote.write(&mut port, clear, "unpin").unwrap();
    assert_eq!(port.sets.last().unwrap().2, "sha256:local");
    let (set, pinned) = local_pin_write(pin.as_ref(), "other").unwrap();
    assert!(!pinned);
    assert_eq!(set, ConfigWrite::pin(PinSource::Local, "other"));
    assert!(
        local_pin_write(None, "other").is_err(),
        "outside a worktree"
    );
    // A rename follows the clone pin only.
    assert_eq!(
        renamed_pin_write(pin.as_ref(), "deep", "deeper"),
        Some(ConfigWrite::pin(PinSource::Local, "deeper"))
    );
    assert_eq!(renamed_pin_write(pin.as_ref(), "other", "x"), None);
}

#[test]
fn rpc_conflict_rereads_the_store_and_never_retries() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    let ticket = remote
        .write(&mut port, profiles_write(&file).unwrap(), "save")
        .unwrap();
    let (gets, sets) = (port.gets.len(), port.sets.len());
    assert!(matches!(
        remote.land(
            &mut port,
            ticket,
            rejected(ConfigErrorCode::RevisionConflict)
        ),
        Landed::Conflict("save", ConfigStore::Profiles)
    ));
    // Exactly one fresh read of that store, and no write.
    assert_eq!(port.gets.len(), gets + 1);
    assert_eq!(port.gets.last().unwrap().1, ConfigStore::Profiles);
    assert_eq!(port.sets.len(), sets, "never a blind retry");

    // The fresh read lands as the refresh that recomputes the draft.
    let fresh = profiles_state(&["deep", "other", "theirs"], "sha256:g9");
    let reread = port.ticket_for(ConfigStore::Profiles);
    assert!(matches!(
        remote.land(&mut port, reread, ConfigOutcome::Read(fresh)),
        Landed::Refreshed(ConfigStore::Profiles)
    ));
    let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    assert!(file.has("theirs"));
    // A new write is guarded by the fresh revision.
    remote
        .write(&mut port, profiles_write(&file).unwrap(), "again")
        .unwrap();
    assert_eq!(port.sets.last().unwrap().2, "sha256:g9");
}

#[test]
fn rpc_rejections_are_reported_and_never_fall_back() {
    let failures = [
        rejected(ConfigErrorCode::InvalidData),
        rejected(ConfigErrorCode::WriteFailed),
        rejected(ConfigErrorCode::ConfigUnavailable),
        rejected(ConfigErrorCode::ReadOnly),
        ConfigOutcome::Failed(ConfigFailure::Timeout),
        ConfigOutcome::Failed(ConfigFailure::Dropped),
        ConfigOutcome::Failed(ConfigFailure::NotReady),
        ConfigOutcome::Failed(ConfigFailure::Failure("transport".into())),
    ];
    for failure in failures {
        let mut port = FakePort::default();
        let mut remote = opened(&mut port);
        let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
            panic!()
        };
        let ticket = remote
            .write(&mut port, profiles_write(&file).unwrap(), "save")
            .unwrap();
        let (gets, sets) = (port.gets.len(), port.sets.len());
        let label = format!("{failure:?}");
        match remote.land(&mut port, ticket, failure) {
            Landed::Rejected("save", reported) => {
                assert!(!reported.is_legacy(), "{label}");
                assert!(!reported.message().is_empty(), "{label}");
            }
            _ => panic!("{label} must be reported as a rejection"),
        }
        assert_eq!((port.gets.len(), port.sets.len()), (gets, sets), "{label}");
        // The store keeps the state it had: nothing was adopted.
        assert_eq!(
            remote
                .state(ConfigStore::Profiles)
                .unwrap()
                .write_revision(ConfigWriteTarget::Global),
            Some("sha256:g1")
        );
    }
}

#[test]
fn rpc_falls_back_only_when_the_methods_are_unsupported() {
    for failure in [ConfigFailure::Unsupported, ConfigFailure::UnsupportedMethod] {
        // A refused read and a refused write both mean the direct-file path.
        let mut port = FakePort::default();
        let mut remote: Remote<&str> = Remote::new();
        remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
        let ticket = port.ticket_for(ConfigStore::Profiles);
        assert!(matches!(
            remote.land(&mut port, ticket, ConfigOutcome::Failed(failure.clone())),
            Landed::Legacy(reported) if reported == failure
        ));

        let mut port = FakePort::default();
        let mut remote = opened(&mut port);
        let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
            panic!()
        };
        let ticket = remote
            .write(&mut port, profiles_write(&file).unwrap(), "save")
            .unwrap();
        assert!(matches!(
            remote.land(&mut port, ticket, ConfigOutcome::Failed(failure.clone())),
            Landed::Legacy(_)
        ));
        assert!(failure.is_legacy());
    }
    // A failed read that is not "unsupported" stays an error for that store.
    let mut port = FakePort::default();
    let mut remote: Remote<&str> = Remote::new();
    remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
    let ticket = port.ticket_for(ConfigStore::Profiles);
    assert!(matches!(
        remote.land(
            &mut port,
            ticket,
            rejected(ConfigErrorCode::ConfigUnavailable)
        ),
        Landed::ReadFailed(ConfigStore::Profiles, _)
    ));
    assert!(rpc_load(&remote).is_err());
    // The port refusing up front is reported the same way, without a ticket.
    let mut port = FakePort {
        refuse: Some(ConfigFailure::NotReady),
        ..FakePort::default()
    };
    let mut remote: Remote<&str> = Remote::new();
    assert_eq!(
        remote.read(&mut port, &[ConfigStore::Profiles]),
        Err(ConfigFailure::NotReady)
    );
}

#[test]
fn rpc_config_changes_reread_only_what_moved() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    let before = port.gets.len();
    // Our own write's echo: the revision is the one already held.
    let held = remote
        .state(ConfigStore::Profiles)
        .unwrap()
        .revision
        .clone();
    remote
        .changed(
            &mut port,
            &[ConfigChange {
                store: ConfigStore::Profiles,
                revision: Some(held),
            }],
        )
        .unwrap();
    assert_eq!(port.gets.len(), before);
    // Another revision, or a gap with none: re-read that store.
    remote
        .changed(
            &mut port,
            &[
                ConfigChange {
                    store: ConfigStore::Pins,
                    revision: Some("sha256:new".into()),
                },
                ConfigChange {
                    store: ConfigStore::Models,
                    revision: None,
                },
            ],
        )
        .unwrap();
    let asked: Vec<ConfigStore> = port.gets[before..].iter().map(|(_, s)| *s).collect();
    assert_eq!(asked, [ConfigStore::Pins, ConfigStore::Models]);
    // A store already being re-read is not asked twice.
    remote
        .changed(
            &mut port,
            &[ConfigChange {
                store: ConfigStore::Pins,
                revision: None,
            }],
        )
        .unwrap();
    assert_eq!(port.gets.len(), before + 2);
    assert!(port.sets.is_empty());
}

#[test]
fn rpc_late_and_unknown_answers_change_nothing() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    let state = profiles_state(&["intruder"], "sha256:x");
    // A ticket this screen never held (an old screen's, or another tab's).
    assert!(matches!(
        remote.land(&mut port, 999, ConfigOutcome::Read(state.clone())),
        Landed::Ignored
    ));
    // An answer to an already-answered ticket.
    let answered = port.ticket_for(ConfigStore::Profiles);
    assert!(matches!(
        remote.land(&mut port, answered, ConfigOutcome::Read(state)),
        Landed::Ignored
    ));
    let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    assert!(!file.has("intruder"));
    // A read answered with another store's state is not adopted.
    remote.read(&mut port, &[ConfigStore::Pins]).unwrap();
    let ticket = port.ticket_for(ConfigStore::Pins);
    assert!(matches!(
        remote.land(
            &mut port,
            ticket,
            ConfigOutcome::Read(profiles_state(&["intruder"], "sha256:x"))
        ),
        Landed::ReadFailed(ConfigStore::Pins, _)
    ));
    // Forgetting (the session was replaced) drops every outstanding ticket.
    remote.read(&mut port, &[ConfigStore::Models]).unwrap();
    let ticket = port.ticket_for(ConfigStore::Models);
    remote.forget();
    assert!(!remote.loading() && !remote.writing());
    assert!(matches!(
        remote.land(
            &mut port,
            ticket,
            ConfigOutcome::Read(read_only(ConfigStore::Models, json!({})))
        ),
        Landed::Ignored
    ));
}

#[test]
fn rpc_read_only_stores_are_never_written() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    // The read-only stores offer no write revision, so the screen has
    // nothing to guard a write with; the typed writes cannot name them.
    for store in [
        ConfigStore::Models,
        ConfigStore::Subagents,
        ConfigStore::ManagedAssets,
    ] {
        assert!(remote.state(store).unwrap().write_targets.is_none());
    }
    // Pins outside a worktree offer no target: a pin is refused up front.
    let outside = ConfigState {
        store: ConfigStore::Pins,
        scope: ConfigScope::LocalRepo,
        read_only: false,
        status: ConfigStatus::Invalid,
        revision: "sha256:00".into(),
        value: Some(
            json!({"resolved": false, "invalid": [], "stale": [], "reason": "worktree_unresolved"}),
        ),
        layers: Vec::new(),
        write_targets: Some(Vec::new()),
    };
    remote.read(&mut port, &[ConfigStore::Pins]).unwrap();
    let ticket = port.ticket_for(ConfigStore::Pins);
    remote.land(&mut port, ticket, ConfigOutcome::Read(outside.clone()));
    let (status, _) = rpc_pin_status(&outside);
    assert!(status.is_none(), "no worktree, no pin layers");
    assert!(
        remote
            .write(&mut port, ConfigWrite::pin(PinSource::Local, "deep"), "pin")
            .is_err()
    );
    // Every write the screen ever sent named a writable store.
    let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    remote
        .write(&mut port, profiles_write(&file).unwrap(), "save")
        .unwrap();
    assert!(
        port.sets
            .iter()
            .all(|(_, write, _)| !write.store().is_read_only())
    );
}

#[test]
fn rpc_pin_layers_map_without_paths_and_say_what_they_hide() {
    // A stale clone pin over a valid declaration.
    let mut state = pins_state(Some("gone"), Some("team"));
    state.value = Some(json!({
        "resolved": true,
        "winner": {"source": "repo", "profile": "team"},
        "invalid": [],
        "stale": [{"source": "local", "profile": "gone"}],
    }));
    let (status, notice) = rpc_pin_status(&state);
    let status = status.unwrap();
    assert_eq!(status.local, PinRead::Valid("gone".into()));
    assert_eq!(status.repo, PinRead::Valid("team".into()));
    assert!(notice.is_none());

    // An invalid clone pin file.
    let mut state = pins_state(None, Some("team"));
    state.layers[0].status = ConfigStatus::Invalid;
    state.value = Some(json!({
        "resolved": true,
        "winner": {"source": "repo", "profile": "team"},
        "invalid": [{"source": "local"}],
        "stale": [],
    }));
    let (status, _) = rpc_pin_status(&state);
    assert_eq!(status.unwrap().local, PinRead::Invalid);

    // A declaration shadowed by the clone pin is not reported by the store:
    // the screen says so instead of claiming there is none.
    let state = pins_state(Some("deep"), Some("team"));
    let (status, notice) = rpc_pin_status(&state);
    assert_eq!(status.unwrap().local, PinRead::Valid("deep".into()));
    assert!(notice.unwrap().contains("declaration"));
}

#[test]
fn legacy_fallbacks_follow_a_rename_through_the_shared_rule() {
    let scratch = Scratch::new("fallbacks");
    let env = scratch.env();
    let path = scratch.write(
        "config/profile-fallbacks.json",
        r#"{"kind":"gentle-pi.profile_model_fallbacks","version":1,"profiles":{"deep":{"orchestrator":["a/b"]}}}"#,
    );
    follow_fallbacks(&env.config_home, "deep", Some("deeper"));
    let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        value["profiles"],
        json!({"deeper": {"orchestrator": ["a/b"]}})
    );
    assert_eq!(
        followed_fallbacks(&value, "deeper", None),
        Some(json!({"kind": "gentle-pi.profile_model_fallbacks", "version": 1, "profiles": {}}))
    );
    assert_eq!(
        followed_fallbacks(&json!({"kind": "other"}), "deeper", None),
        None
    );
}

#[test]
fn rpc_writes_are_never_sent_while_make_default_holds_the_files() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    let RpcLoad::Ready { file, pin, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    let started = files_job_start(&mut remote).expect("nothing open: Make default may start");
    assert_eq!(started.names().collect::<Vec<_>>(), ["deep", "other"]);

    // Every write kind is refused at the entry point, and none reaches the
    // session.
    let edited = create_profile(&file, "draft", Routing::default()).unwrap();
    let fallbacks = remote.state(ConfigStore::ProfileFallbacks).unwrap().clone();
    let writes = [
        profiles_write(&edited).unwrap(),
        fallbacks_write(&fallbacks, "deep", Some("deeper"))
            .unwrap()
            .unwrap(),
        local_pin_write(pin.as_ref(), "other").unwrap().0,
        ConfigWrite::clear_pin(PinSource::Local),
    ];
    for write in writes {
        let refused = remote.write(&mut port, write, "held").unwrap_err();
        assert_eq!(refused, ConfigFailure::Held);
        assert!(
            refused
                .message()
                .contains("wait for Make default to finish")
        );
        assert!(!refused.is_legacy(), "a hold never switches to files");
    }
    assert!(port.sets.is_empty(), "nothing was sent while held");
    // Nothing was adopted, so the draft built on the held store is still
    // valid against it and can be saved once the hold ends.
    assert_eq!(
        remote
            .state(ConfigStore::Profiles)
            .unwrap()
            .write_revision(ConfigWriteTarget::Global),
        Some("sha256:g1")
    );
    assert!(
        files_job_start(&mut remote).is_err(),
        "one files job at a time"
    );

    remote.release();
    remote
        .write(&mut port, profiles_write(&edited).unwrap(), "after")
        .unwrap();
    assert_eq!(port.sets.len(), 1);
    assert_eq!(port.sets[0].2, "sha256:g1");
}

#[test]
fn make_default_starts_only_from_the_freshest_profiles_store() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    let next = create_profile(&file, "fresh", Routing::default()).unwrap();
    let ticket = remote
        .write(&mut port, profiles_write(&next).unwrap(), "save")
        .unwrap();
    // A write still open: refused, and nothing is held.
    assert!(files_job_start(&mut remote).is_err());
    assert!(!remote.held());

    // Once it lands, Make default starts from the store it produced.
    let result = written(
        profiles_state(&["deep", "other", "fresh"], "sha256:g2"),
        ConfigWriteTarget::Global,
        "sha256:g2",
    );
    remote.land(&mut port, ticket, ConfigOutcome::Written(result));
    let started = files_job_start(&mut remote).unwrap();
    assert!(started.has("fresh"));
    remote.release();

    // A re-read of the store still open: refused until it lands.
    remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
    assert!(files_job_start(&mut remote).is_err());
    assert!(!remote.held());
    let reread = port.ticket_for(ConfigStore::Profiles);
    remote.land(
        &mut port,
        reread,
        ConfigOutcome::Read(profiles_state(&["deep", "theirs"], "sha256:g3")),
    );
    let started = files_job_start(&mut remote).unwrap();
    assert!(started.has("theirs") && !started.has("fresh"));
}

#[test]
fn a_reread_asked_while_a_read_is_open_follows_it_exactly_once() {
    let mut port = FakePort::default();
    let mut remote = opened(&mut port);
    remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
    let first = port.ticket_for(ConfigStore::Profiles);
    let before = port.gets.len();
    // Make default finished and asks again, twice: no concurrent read.
    remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
    remote.read(&mut port, &[ConfigStore::Profiles]).unwrap();
    assert_eq!(port.gets.len(), before);

    // The open read answers with content from before Make default's write:
    // exactly one follow-up read goes out, and the screen keeps waiting.
    remote.land(
        &mut port,
        first,
        ConfigOutcome::Read(profiles_state(&["deep"], "sha256:before")),
    );
    assert_eq!(port.gets.len(), before + 1);
    assert_eq!(port.gets.last().unwrap().1, ConfigStore::Profiles);
    assert!(remote.loading());

    let follow_up = port.ticket_for(ConfigStore::Profiles);
    remote.land(
        &mut port,
        follow_up,
        ConfigOutcome::Read(profiles_state(&["deep", "made-default"], "sha256:after")),
    );
    assert_eq!(port.gets.len(), before + 1, "no third read");
    assert!(!remote.loading());
    assert_eq!(
        remote
            .state(ConfigStore::Profiles)
            .unwrap()
            .write_revision(ConfigWriteTarget::Global),
        Some("sha256:after")
    );
    let RpcLoad::Ready { file, .. } = rpc_load(&remote).unwrap() else {
        panic!()
    };
    assert!(file.has("made-default"));
}
