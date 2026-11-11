//! Fleet client: blocking HTTP to the local orchestrator daemon
//! (`orchestratord`). Runs on the background executor like every other
//! effectful client in this app. Protocol model: `gentle_core::orchestrator`.

use std::time::Duration;

use gentle_core::orchestrator::{
    Board, ChatRequest, DaemonVersion, DetectedAgent, DiffInfo, Evidence, JournalEntry,
    LaunchRequest, Project,
};

/// `ORCHESTRATOR_URL`, else the daemon's default loopback address.
fn base_url_from(env: impl Fn(&str) -> Option<String>) -> String {
    match env("ORCHESTRATOR_URL") {
        Some(url) if !url.trim().is_empty() => url.trim_end_matches('/').to_string(),
        _ => "http://127.0.0.1:8094".to_string(),
    }
}

fn base_url() -> String {
    base_url_from(|key| std::env::var(key).ok())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
}

fn decode<T: serde::de::DeserializeOwned>(status: u16, body: String) -> Result<T, String> {
    let value: T = serde_json::from_str(&body)
        .map_err(|_| format!("orchestratord answered {status} without valid JSON"))?;
    if !(200..300).contains(&status) {
        return Err(
            value_error(&body).unwrap_or_else(|| format!("orchestratord answered {status}"))
        );
    }
    Ok(value)
}

fn read_body(response: &mut ureq::http::Response<ureq::Body>) -> Result<(u16, String), String> {
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((status, body))
}

fn value_error(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

fn get<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let mut response = agent()
        .get(format!("{}{path}", base_url()))
        .call()
        .map_err(|_| {
            "orchestratord is not running. It serves the fleet board on port 8094.".to_string()
        })?;
    let (status, body) = read_body(&mut response)?;
    decode(status, body)
}

fn post<T: serde::de::DeserializeOwned>(
    path: &str,
    body: &impl serde::Serialize,
) -> Result<T, String> {
    let payload = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let mut response = agent()
        .post(format!("{}{path}", base_url()))
        .header("Content-Type", "application/json")
        .send(payload)
        .map_err(|e| e.to_string())?;
    let (status, body) = read_body(&mut response)?;
    decode(status, body)
}

fn post_empty(path: &str) -> Result<(), String> {
    let mut response = agent()
        .post(format!("{}{path}", base_url()))
        .header("Content-Type", "application/json")
        .send("{}")
        .map_err(|e| e.to_string())?;
    let (status, body) = read_body(&mut response)?;
    if !(200..300).contains(&status) {
        return Err(
            value_error(&body).unwrap_or_else(|| format!("orchestratord answered {status}"))
        );
    }
    Ok(())
}

pub fn get_board() -> Result<Board, String> {
    get("/board")
}

pub fn get_agents() -> Result<Vec<DetectedAgent>, String> {
    get("/agents")
}

pub fn get_projects() -> Result<Vec<Project>, String> {
    get("/projects")
}

pub fn get_events(session: &str) -> Result<Vec<JournalEntry>, String> {
    get(&format!("/sessions/{}/events", percent_encode(session)))
}

pub fn get_diff(session: &str) -> Result<DiffInfo, String> {
    get(&format!("/sessions/{}/diff", percent_encode(session)))
}

pub fn get_evidence(session: &str) -> Result<Evidence, String> {
    get(&format!("/sessions/{}/evidence", percent_encode(session)))
}

pub fn get_version() -> Result<DaemonVersion, String> {
    get("/version")
}

pub fn launch(request: &LaunchRequest) -> Result<(), String> {
    post("/sessions", request)
}

pub fn chat(session: &str, message: &str) -> Result<(), String> {
    post(
        &format!("/sessions/{}/chat", percent_encode(session)),
        &ChatRequest {
            message: message.to_string(),
        },
    )
}

pub fn stop(session: &str) -> Result<(), String> {
    post_empty(&format!("/sessions/{}/stop", percent_encode(session)))
}

pub fn add_project(project: &Project) -> Result<(), String> {
    post("/projects", project)
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: Vec<(&'static str, &'static str)>) -> impl Fn(&str) -> Option<String> {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn base_url_prefers_the_env_and_trims_the_slash() {
        assert_eq!(
            base_url_from(env_of(vec![("ORCHESTRATOR_URL", "http://127.0.0.1:9999/")])),
            "http://127.0.0.1:9999"
        );
    }

    #[test]
    fn base_url_falls_back_to_the_daemon_default() {
        assert_eq!(base_url_from(env_of(vec![])), "http://127.0.0.1:8094");
        assert_eq!(
            base_url_from(env_of(vec![("ORCHESTRATOR_URL", "   ")])),
            "http://127.0.0.1:8094"
        );
    }

    #[test]
    fn percent_encode_keeps_session_ids_unaffected() {
        assert_eq!(percent_encode("metering"), "metering");
        assert_eq!(percent_encode("a b"), "a%20b");
    }
}
