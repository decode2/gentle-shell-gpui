//! Orchestrator: the fleet model for the Gentle-AI orchestrator daemon
//! (`orchestratord`). This is the pure side of the feature — the wire types
//! of the daemon's HTTP protocol and the state transitions the UI needs.
//! The HTTP client and the GPUI view live in `crates/app/src/features/
//! orchestrator/`; the daemon itself (worktrees, routing, receipts, CI
//! watchers) is the Go service this module talks to.
//!
//! Protocol surface (all localhost): `GET /board`, `GET /board/live` (SSE),
//! `GET /agents`, `GET /sessions/{id}/events`, `GET /sessions/{id}/diff`,
//! `GET /sessions/{id}/evidence`, `POST /sessions`, `POST /sessions/{id}/
//! chat`, `POST /sessions/{id}/stop`, `POST /projects`, `GET /projects`,
//! `GET /version`, `GET/POST /config`.

use serde::{Deserialize, Serialize};

/// The four fact-derived lanes a session sorts itself into. The daemon
/// derives them; clients never set a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lane {
    #[serde(rename = "working")]
    Working,
    #[serde(rename = "needs-you")]
    NeedsYou,
    #[serde(rename = "in-review")]
    InReview,
    #[serde(rename = "ready-to-merge")]
    ReadyToMerge,
}

impl Lane {
    /// Attention-first display order: needs-you on top, then working,
    /// review, ready.
    pub const ALL: [Lane; 4] = [
        Lane::NeedsYou,
        Lane::Working,
        Lane::InReview,
        Lane::ReadyToMerge,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Lane::Working => "Trabajando",
            Lane::NeedsYou => "Te necesita",
            Lane::InReview => "En revisión",
            Lane::ReadyToMerge => "Lista para mergear",
        }
    }
}

/// One session card: the board's projection of a worker session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCard {
    pub id: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub pr: Option<u64>,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(rename = "gatePending", default)]
    pub gate_pending: bool,
}

/// The board: every lane with its cards. The daemon sends all four lanes so
/// a renderer draws it without nil checks; unknown lanes decode as absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    #[serde(default)]
    pub working: Option<Vec<SessionCard>>,
    #[serde(rename = "needs-you", default)]
    pub needs_you: Option<Vec<SessionCard>>,
    #[serde(rename = "in-review", default)]
    pub in_review: Option<Vec<SessionCard>>,
    #[serde(rename = "ready-to-merge", default)]
    pub ready_to_merge: Option<Vec<SessionCard>>,
}

impl Board {
    /// The cards of one lane; absent lanes read empty.
    pub fn lane(&self, lane: Lane) -> &[SessionCard] {
        match lane {
            Lane::Working => self.working.as_deref().unwrap_or(&[]),
            Lane::NeedsYou => self.needs_you.as_deref().unwrap_or(&[]),
            Lane::InReview => self.in_review.as_deref().unwrap_or(&[]),
            Lane::ReadyToMerge => self.ready_to_merge.as_deref().unwrap_or(&[]),
        }
    }

    /// Total sessions across lanes.
    pub fn total(&self) -> usize {
        Lane::ALL.iter().map(|lane| self.lane(*lane).len()).sum()
    }

    /// Cards for one project; an empty project means every session.
    pub fn for_project(&self, project: &str) -> Board {
        if project.is_empty() {
            return self.clone();
        }
        let keep = |cards: Option<Vec<SessionCard>>| -> Option<Vec<SessionCard>> {
            cards.map(|list| {
                list.into_iter()
                    .filter(|card| card.project.as_deref() == Some(project))
                    .collect()
            })
        };
        Board {
            working: keep(self.working.clone()),
            needs_you: keep(self.needs_you.clone()),
            in_review: keep(self.in_review.clone()),
            ready_to_merge: keep(self.ready_to_merge.clone()),
        }
    }
}

/// One journal entry: the worker's recorded life (launched, output,
/// chat-in, chat-out, decision, summary, exited, stopped, receipt).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub at: String,
    pub kind: String,
    pub text: String,
}

/// One changed file in a session's worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// The worktree diff against the session's base branch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiffInfo {
    #[serde(default)]
    pub files: Vec<DiffFile>,
    #[serde(default)]
    pub patch: String,
}

/// The per-worker audit package: identity, status, diff totals, commits,
/// decisions, the worker's summary, and the frozen evidence receipt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub receipt: Option<String>,
    #[serde(default)]
    pub valid: bool,
    #[serde(default)]
    pub commits: Vec<String>,
    #[serde(rename = "totalAdditions", default)]
    pub total_additions: u64,
    #[serde(rename = "totalDeletions", default)]
    pub total_deletions: u64,
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
}

/// One detected agent on the machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectedAgent {
    pub agent: String,
    pub command: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub found: bool,
}

/// One managed repository in a multi-repo fleet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub repo: String,
    #[serde(rename = "repoPath", default)]
    pub repo_path: Option<String>,
}

/// The daemon's build identity from `GET /version`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonVersion {
    pub version: String,
}

/// A launch request body for `POST /sessions` (launch: true makes the daemon
/// create the worktree and spawn the agent CLI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub id: String,
    pub task: String,
    pub agent: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub launch: bool,
}

/// A chat message body for `POST /sessions/{id}/chat`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

/// Decides whether the published release tag is newer than the running
/// version. Both sides tolerate a leading `v`; any unparsable side answers
/// false so a broken tag never triggers an update.
pub fn update_available(current: &str, latest: &str) -> bool {
    let (Some(cur), Some(upd)) = (parse_version(current), parse_version(latest)) else {
        return false;
    };
    upd > cur
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    nums: [u64; 3],
    pre: Option<u64>,
}

fn parse_version(tag: &str) -> Option<Version> {
    let tag = tag.trim();
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    if tag.is_empty() {
        return None;
    }
    let (core, pre) = match tag.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (tag, None),
    };
    let mut nums = [0u64; 3];
    for (i, part) in core.split('.').take(3).enumerate() {
        nums[i] = part.parse().ok()?;
    }
    let pre_num = pre.and_then(|p| {
        let numeric: String = p
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if numeric.is_empty() {
            None
        } else {
            numeric.parse().ok()
        }
    });
    Some(Version { nums, pre: pre_num })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_with(lane: Lane, id: &str, project: &str) -> Board {
        let card = SessionCard {
            id: id.to_string(),
            project: Some(project.to_string()),
            ..Default::default()
        };
        let mut board = Board::default();
        match lane {
            Lane::Working => board.working = Some(vec![card]),
            Lane::NeedsYou => board.needs_you = Some(vec![card]),
            Lane::InReview => board.in_review = Some(vec![card]),
            Lane::ReadyToMerge => board.ready_to_merge = Some(vec![card]),
        }
        board
    }

    #[test]
    fn board_lane_reads_absent_lanes_as_empty() {
        let board = Board::default();
        assert!(board.lane(Lane::NeedsYou).is_empty());
        assert_eq!(board.total(), 0);
    }

    #[test]
    fn board_decodes_the_daemon_payload() {
        let payload = r#"{"in-review":null,"needs-you":[{"id":"m","task":"t","agent":"pi","branch":"orch/m","gatePending":true}],"ready-to-merge":null,"working":null}"#;
        let board: Board = serde_json::from_str(payload).unwrap();
        assert_eq!(board.lane(Lane::NeedsYou).len(), 1);
        assert!(board.lane(Lane::NeedsYou)[0].gate_pending);
        assert_eq!(board.total(), 1);
    }

    #[test]
    fn board_filters_by_project() {
        let mut board = board_with(Lane::Working, "a", "core");
        let other = SessionCard {
            id: "b".into(),
            project: Some("web".into()),
            ..Default::default()
        };
        board.working.as_mut().unwrap().push(other);

        let core = board.for_project("core");
        assert_eq!(core.lane(Lane::Working).len(), 1);
        assert_eq!(core.lane(Lane::Working)[0].id, "a");
        assert_eq!(board.for_project("").lane(Lane::Working).len(), 2);
    }

    #[test]
    fn update_comparison_matches_the_daemon_rules() {
        assert!(!update_available("0.1.0-alpha.1", "v0.1.0-alpha.1"));
        assert!(update_available("0.1.0-alpha.1", "v0.1.0-alpha.2"));
        assert!(update_available("0.1.0", "v0.2.0"));
        assert!(!update_available("0.2.0", "v0.1.0"));
        assert!(!update_available("0.1.0", "not-a-version"));
    }

    #[test]
    fn lane_order_is_attention_first() {
        assert_eq!(Lane::ALL[0], Lane::NeedsYou);
        assert_eq!(Lane::ALL[3], Lane::ReadyToMerge);
        assert_eq!(Lane::NeedsYou.label(), "Te necesita");
    }
}
