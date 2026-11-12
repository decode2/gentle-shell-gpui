//! Credentials (`auth.json`) → what each provider card can show, and Pi's
//! config-value resolution for a stored API key.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use gentle_core::usage::subscription::{
    ANTHROPIC_PROVIDER, CODEX_PROVIDER, NAN_PROVIDER, OPENAI_PROVIDER, ProviderUsage,
    account_id_from_token,
};
use pi_rpc::desktop_rpc::UsageRefresh;
use serde_json::{Map, Value};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub(super) enum CardState {
    Loading,
    Ready(ProviderUsage),
    /// Informational, nothing to fetch.
    Note(String),
    /// The stored credential cannot be used until the user signs in again.
    SignIn(String),
    Failed(String),
    /// RPC: the server is still fetching; the last good data stays when present.
    Pending(Option<ProviderUsage>),
    /// RPC: the data is stale but still shown, marked stale.
    Stale(Option<ProviderUsage>),
    /// RPC: no usable data for this provider.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ProviderCard {
    pub(super) id: String,
    pub(super) title: &'static str,
    pub(super) state: CardState,
    /// RPC only: whether the server can refresh this provider's usage on
    /// request. `None` on the legacy path (which always fetches directly).
    pub(super) refresh: Option<UsageRefresh>,
}

/// A pending request. Holds a secret, so it deliberately has no `Debug`.
pub(super) enum Fetch {
    Codex {
        token: String,
        account_id: String,
    },
    Nan {
        key: String,
        env: HashMap<String, String>,
    },
}

pub(super) struct Discovery {
    pub(super) cards: Vec<(ProviderCard, Option<Fetch>)>,
    /// Signed-in providers that expose no usage endpoint.
    pub(super) others: Vec<String>,
}

fn provider_title(id: &str) -> &'static str {
    match id {
        CODEX_PROVIDER => "ChatGPT · Codex",
        OPENAI_PROVIDER => "OpenAI · ChatGPT",
        ANTHROPIC_PROVIDER => "Anthropic · Claude",
        NAN_PROVIDER => "NaN Cloud",
        _ => "Provider",
    }
}

pub(super) fn discover(auth: &Map<String, Value>, now: i64) -> Discovery {
    let mut cards = Vec::new();
    let mut card = |id: &str, state: CardState, fetch: Option<Fetch>| {
        cards.push((
            ProviderCard {
                id: id.to_string(),
                title: provider_title(id),
                state,
                refresh: None,
            },
            fetch,
        ));
    };
    for id in [CODEX_PROVIDER, OPENAI_PROVIDER] {
        let Some(cred) = auth.get(id).and_then(Value::as_object) else {
            continue;
        };
        if cred.get("type").and_then(Value::as_str) != Some("oauth") {
            if id == CODEX_PROVIDER {
                card(
                    id,
                    CardState::Note(
                        "API key credential — subscription usage needs a ChatGPT sign-in.".into(),
                    ),
                    None,
                );
            }
            continue;
        }
        let expires = cred.get("expires").and_then(Value::as_f64);
        let Some(token) = cred
            .get("access")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        else {
            card(
                id,
                CardState::SignIn("No access token stored — sign in again.".into()),
                None,
            );
            continue;
        };
        if expires.is_some_and(|at| (at as i64) < now) {
            card(
                id,
                CardState::SignIn("The ChatGPT token expired — sign in again (a chat turn with this provider also refreshes it).".into()),
                None,
            );
            continue;
        }
        let account_id = cred
            .get("accountId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| account_id_from_token(token));
        match account_id {
            Some(account_id) => card(
                id,
                CardState::Loading,
                Some(Fetch::Codex {
                    token: token.to_string(),
                    account_id,
                }),
            ),
            None => card(
                id,
                CardState::SignIn(
                    "The token carries no ChatGPT account id — sign in again.".into(),
                ),
                None,
            ),
        }
    }
    if auth.contains_key(ANTHROPIC_PROVIDER) {
        card(
            ANTHROPIC_PROVIDER,
            CardState::Note(
                "Claude reports its 5h and weekly windows only in response headers, so they are available while chatting in the terminal UI."
                    .into(),
            ),
            None,
        );
    }
    if let Some(cred) = auth.get(NAN_PROVIDER).and_then(Value::as_object) {
        let key = cred
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty());
        match (cred.get("type").and_then(Value::as_str), key) {
            (Some("api_key"), Some(key)) => {
                let env = cred
                    .get("env")
                    .and_then(Value::as_object)
                    .map(|env| {
                        env.iter()
                            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                card(
                    NAN_PROVIDER,
                    CardState::Loading,
                    Some(Fetch::Nan {
                        key: key.to_string(),
                        env,
                    }),
                );
            }
            _ => card(
                NAN_PROVIDER,
                CardState::Failed("No NaN API key stored.".into()),
                None,
            ),
        }
    }
    let mut others: Vec<String> = auth
        .keys()
        .filter(|id| {
            ![
                CODEX_PROVIDER,
                OPENAI_PROVIDER,
                ANTHROPIC_PROVIDER,
                NAN_PROVIDER,
            ]
            .contains(&id.as_str())
        })
        .cloned()
        .collect();
    if let Some(cred) = auth.get(OPENAI_PROVIDER)
        && cred.get("type").and_then(Value::as_str) != Some("oauth")
    {
        others.push(OPENAI_PROVIDER.into());
    }
    others.sort();
    Discovery { cards, others }
}

fn is_env_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_env_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && is_env_name_start(bytes[0])
        && bytes
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// Pi's config-value template: `$VAR`/`${VAR}` interpolate (stored env map
/// first, then the process env), `$$` and `$!` escape. `None` when a
/// referenced variable is missing.
fn resolve_template(config: &str, env: &HashMap<String, String>) -> Option<String> {
    let lookup = |name: &str| {
        env.get(name)
            .filter(|v| !v.is_empty())
            .cloned()
            .or_else(|| std::env::var(name).ok().filter(|v| !v.is_empty()))
    };
    let mut out = String::new();
    let mut index = 0;
    while let Some(offset) = config[index..].find('$') {
        let dollar = index + offset;
        out.push_str(&config[index..dollar]);
        let rest = &config[dollar + 1..];
        match rest.as_bytes().first() {
            Some(c @ (b'$' | b'!')) => {
                out.push(*c as char);
                index = dollar + 2;
            }
            Some(b'{') => match rest.find('}') {
                None => {
                    out.push('$');
                    index = dollar + 1;
                }
                Some(end) => {
                    let name = &rest[1..end];
                    if is_env_name(name) {
                        out.push_str(&lookup(name)?);
                    } else {
                        out.push_str(&config[dollar..=dollar + 1 + end]);
                    }
                    index = dollar + 2 + end;
                }
            },
            Some(c) if is_env_name_start(*c) => {
                let len = rest
                    .bytes()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                    .count();
                out.push_str(&lookup(&rest[..len])?);
                index = dollar + 1 + len;
            }
            _ => {
                out.push('$');
                index = dollar + 1;
            }
        }
    }
    out.push_str(&config[index..]);
    Some(out)
}

/// `!command` runs through the shell (stdout, trimmed); anything else is a template.
pub(super) fn resolve_config_value(config: &str, env: &HashMap<String, String>) -> Option<String> {
    let Some(command) = config.strip_prefix('!') else {
        return resolve_template(config, env).filter(|v| !v.is_empty());
    };
    use std::process::Stdio;
    #[cfg(windows)]
    let mut cmd = pi_rpc::command("cmd");
    #[cfg(windows)]
    cmd.args(["/C", command]);
    #[cfg(not(windows))]
    let mut cmd = pi_rpc::command("sh");
    #[cfg(not(windows))]
    cmd.args(["-c", command]);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < COMMAND_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string()).filter(|v| !v.is_empty())
}

pub(super) fn read_auth(path: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(format!("{} is not a JSON object.", path.display())),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::{CardState, Fetch, discover, resolve_config_value};
    use gentle_core::usage::subscription::account_id_from_token;
    use serde_json::json;
    use std::collections::HashMap;

    const NOW: i64 = 1_762_400_000_000;

    #[test]
    fn account_id_and_discovery_never_fetch_expired_tokens() {
        let payload = r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"}}"#;
        let encoded: String = {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let bytes = payload.as_bytes();
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let n = chunk
                    .iter()
                    .enumerate()
                    .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
                for i in 0..=chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                }
            }
            out
        };
        let token = format!("e30.{encoded}.sig");
        assert_eq!(account_id_from_token(&token).as_deref(), Some("acct_123"));
        assert_eq!(account_id_from_token("not-a-jwt"), None);

        let auth = json!({
            "openai-codex": { "type": "oauth", "access": token, "refresh": "r", "expires": NOW + 1000 },
            "openai": { "type": "oauth", "access": "x.y.z", "refresh": "r", "expires": NOW - 1, "accountId": "a" },
            "anthropic": { "type": "oauth", "access": "a", "refresh": "r", "expires": NOW + 1 },
            "nan": { "type": "api_key", "key": "$NAN_KEY", "env": { "NAN_KEY": "k" } },
            "openrouter": { "type": "api_key", "key": "k" }
        });
        let discovery = discover(auth.as_object().unwrap(), NOW);
        let states: Vec<(&str, bool, &CardState)> = discovery
            .cards
            .iter()
            .map(|(card, fetch)| (card.id.as_str(), fetch.is_some(), &card.state))
            .collect();
        assert_eq!(states[0].0, "openai-codex");
        assert!(states[0].1);
        assert!(
            matches!(&discovery.cards[0].1, Some(Fetch::Codex { account_id, .. }) if account_id == "acct_123")
        );
        assert_eq!(states[1].0, "openai");
        assert!(
            !states[1].1 && matches!(states[1].2, CardState::SignIn(_)),
            "expired tokens are not fetched"
        );
        assert!(matches!(
            states[2],
            ("anthropic", false, CardState::Note(_))
        ));
        assert!(matches!(states[3], ("nan", true, CardState::Loading)));
        assert_eq!(discovery.others, vec!["openrouter".to_string()]);

        let env = HashMap::from([("NAN_KEY".to_string(), "secret".to_string())]);
        assert_eq!(
            resolve_config_value("$NAN_KEY", &env).as_deref(),
            Some("secret")
        );
        assert_eq!(
            resolve_config_value("pre-${NAN_KEY}-$$x", &env).as_deref(),
            Some("pre-secret-$x")
        );
        assert_eq!(
            resolve_config_value("$GENTLE_USAGE_TEST_UNSET_VAR", &env),
            None
        );
        assert_eq!(
            resolve_config_value("plain", &env).as_deref(),
            Some("plain")
        );
    }
}
