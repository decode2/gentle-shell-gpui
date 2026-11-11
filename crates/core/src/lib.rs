//! Pure Gentle Shell logic shared by the app: protocol models, parsers and
//! state transitions. No GPUI, no RPC process, no filesystem, no processes,
//! no OS APIs (see docs/adr/0002-nucleo-puro-incremental.md).

pub mod activity;
pub mod alert_timing;
pub mod commands;
pub mod devices;
pub mod engram;
pub mod fuzzy;
pub mod gentle_state;
pub mod git;
pub mod music;
pub mod notifications;
pub mod orchestrator;
pub mod profiles;
pub mod scroll;
pub mod sessions;
pub mod text;
pub mod tools;
pub mod transcript;
pub mod usage;
pub mod version;
pub mod web;
pub mod workspaces;
