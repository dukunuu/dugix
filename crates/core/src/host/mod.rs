//! `dugix host` — the daemon surface: an `HTTP`/`SSE` session API, provider
//! gateway, worker routes, and tunnel/sleep-guard process management.

mod admin;
mod api;
mod apps;
pub mod cloudflare;
pub mod process;
pub mod wire;

pub use api::{HostConfig, HostServer, host_discovery_id};

mod management;

mod media;

mod research_view;

mod browser_sync;
mod jobs;
mod sessions_view;
