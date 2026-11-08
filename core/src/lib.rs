//! Remote Volume: OS audio access plus the all-Rust WebSocket app built on it.
//!
//! - `backend`, `protocol`, `volume`: the audio core and its stdio JSON contract (`CONTRACT.md`).
//! - `wsproto`, `config`, `audio`, `apps`, `server`, `settings`, `cli`: the WebSocket server for
//!   Bitfocus Companion (protocol v1 + v2) and the local settings page.

pub mod apps;
pub mod audio;
pub mod backend;
pub mod cli;
pub mod config;
pub mod install;
pub mod log;
pub mod protocol;
pub mod server;
pub mod settings;
pub mod tray;
pub mod update;
pub mod volume;
pub mod wsproto;

/// The release version (release-please keeps `Cargo.toml` in step with each release). Debug builds,
/// i.e. a plain `cargo run` or `cargo build`, add `-dev` so they are never mistaken for a release.
pub const VERSION: &str = if cfg!(debug_assertions) {
    concat!(env!("CARGO_PKG_VERSION"), "-dev")
} else {
    env!("CARGO_PKG_VERSION")
};
