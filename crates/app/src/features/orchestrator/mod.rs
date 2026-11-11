//! Fleet: the multi-agent orchestration panel. Talks to the local
//! orchestrator daemon (`orchestratord`) — a separate Go service that owns
//! worker worktrees, fact-derived lanes, CI/review routing and evidence
//! receipts. The conversation with each agent stays in this app's regular
//! Gentle Shell tabs; this panel is the fleet view on top.

pub mod client;
pub mod sidecar;
pub mod stream;
mod view;

pub use view::{FleetEvent, FleetView};
