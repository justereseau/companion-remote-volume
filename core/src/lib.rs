//! Remote Volume: OS audio access plus the all-Rust WebSocket app built on it.
//!
//! - `backend`, `protocol`, `volume`: the audio core and its stdio JSON contract (`CONTRACT.md`).
//! - `wsproto`, `config`, `audio`, `server`, `settings`, `cli`: the WebSocket server for Bitfocus Companion
//!   (protocol v1 + v2) and the local settings page.

pub mod audio;
pub mod backend;
pub mod cli;
pub mod config;
pub mod log;
pub mod protocol;
pub mod server;
pub mod settings;
pub mod volume;
pub mod wsproto;
