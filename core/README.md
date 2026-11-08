# Audio core

Rust binary that owns the OS audio calls and talks line-delimited JSON over stdio. The wire contract is in [CONTRACT.md](CONTRACT.md).

## Local setup

| OS | You need |
|---|---|
| all | [rustup](https://rustup.rs) (`winget install Rustlang.Rustup` on Windows). `rust-toolchain.toml` selects the stable channel and clippy. |
| Windows | Visual Studio Build Tools with the "Desktop development with C++" workload (MSVC + Windows SDK). |
| macOS | Xcode command line tools (`xcode-select --install`). |
| Linux | `sudo apt install libpulse-dev pkg-config`, plus a running PulseAudio or PipeWire server to test against. |

## Installing and updating

`src/install.rs` is `--install` / `--uninstall`: one program file in a per-user folder that starts at login with `--service` (Windows `Run` key, macOS LaunchAgent, Linux systemd user service). `installer/` holds the Windows setup (Inno Setup, which only delivers the program and runs `--install`) and `install.sh`.

`src/update.rs` is the self-update of a program running with `--service` from that folder. Not a program started by hand, and not a debug build (its `-dev` version never updates). CI signs each release's `update.json` with the Ed25519 private key in the repository secret `UPDATE_SIGNING_KEY`; the public key is `PUBLIC_KEY` in `update.rs` and `UPDATE_PUBLIC_KEY` in `ci.yml`. To change the key, ship a release signed with the old key that contains the new public key, then change the secret.

## Commands

Run from `core/`:

```sh
cargo test                                # unit + protocol tests, no audio device needed
RV_INTEGRATION=1 cargo test               # also drives the REAL default output device
RV_INTEGRATION=no-device cargo test       # for machines without an output device
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`RV_INTEGRATION=1` briefly changes the system volume and mute state and restores both. On Linux you can add `RV_EXPECT_SINK=<sink name>` to require a specific default sink.

To test Linux without a sound card, mirror CI:

```sh
pulseaudio --start --exit-idle-time=-1
pactl load-module module-null-sink sink_name=ci_sink
pactl set-default-sink ci_sink
RV_INTEGRATION=1 RV_EXPECT_SINK=ci_sink cargo test
```

Try the binary by hand:

```sh
echo '{"id":1,"action":"listOutputDevices"}' | cargo run -q
```

## remote-volume: the all-Rust app

`remote-volume` is the app: one binary that talks to the OS audio API, serves the WebSocket protocol Bitfocus Companion uses, and serves a settings page. The `remote-volume-core` sidecar (stdio JSON, `CONTRACT.md`) was the audio layer of the Electron app, which is gone; it still builds from the same crate.

```sh
cargo run --release --bin remote-volume -- --open
```

| What | Where |
|---|---|
| WebSocket | `ws://<host>:2501` (port, bind address, token and allowed origins are configurable) |
| Settings page | `http://127.0.0.1:2502/` (`--settings-port` or `RV_SETTINGS_PORT`; `--open` opens it in the browser) |
| Config file | `config.json` in `%APPDATA%\RemoteVolume`, `~/Library/Application Support/RemoteVolume` or `~/.config/RemoteVolume` (`--config-dir` or `RV_CONFIG_DIR`) |

Stop it with Ctrl+C. Logs go to stderr.

**Same as the old Electron app:** the v1 WebSocket protocol (messages, replies and state broadcasts, byte for byte; the only addition is a trailing `capabilities` key in the state sent on connect), and the security model: bind address option, access token (`?token=` or `Authorization: Bearer`), `Origin` rejection with an allow list, 1 KB payload limit (close code 1009), 20 messages/s per client, 32 clients. Changing the token or origins disconnects only the clients that no longer pass; changing the port or bind address restarts the listener. On the first start the Electron app's `config.json` (`settings`) is imported if it is found.

**Protocol v2** (see the main readme): `getCapabilities`, `capabilities` in the initial state, string targets, structural diffs for list-valued state, and the per-app actions (`src/apps.rs` groups the sessions a backend reports; `Backend::list_app_sessions`, `set_app_volume`, `set_app_muted`). Windows (`src/backend/wasapi.rs`, one session per WASAPI audio session) and Linux (`src/backend/pulse.rs`, one session per sink-input) implement per-app audio; Linux also has change events (`backend::watch` calls `Server::change_notifier`, which refreshes the state without polling); media and function keys are `Backend::press_media_key` / `press_function_key` (Windows `SendInput`; Linux MPRIS through `dbus-send` in `src/backend/mpris.rs` and `xdotool` in `src/backend/xdotool.rs`); device selection will plug into `server::Extensions`.

**Different from the old Electron app:** no tray icon, no run-on-startup setting (use your OS's autostart for the binary), and the settings window is a web page. That page can change the access token, so it listens on `127.0.0.1` only, answers only to the loopback `Host` names (DNS rebinding), and accepts a save only as JSON from its own origin (cross-site requests). Anyone who can use your computer's loopback interface can still open it.

Tests: `cargo test` runs the protocol, config and settings tests and drives the real server over loopback with a fake audio backend (`tests/ws.rs`, `tests/settings.rs`); no sound card is needed.

## Optional: self-hosted Mac in CI

Hosted macOS runners only have a virtual sound device. To also run the core on a Mac with real audio:

1. Use a **dedicated** Mac or VM with nothing personal on it, install [rustup](https://rustup.rs) and the Xcode command line tools, and register a runner (Settings > Actions > Runners > New self-hosted runner > macOS ARM64) as a launchd service under a non-admin user. Prefer an ephemeral runner.
2. Put it in a runner group restricted to this repository, with **Workflow access** set to selected workflows: `Justereseau/companion-remote-volume/.github/workflows/ci.yml@refs/heads/main`. This is the real protection: a pull request can edit the workflow, but a run from `refs/pull/N/merge` is not allowed to use the group. Leave "Allow public repositories" on only together with that restriction.
3. Set Settings > Actions > General > "Require approval for all outside collaborators".
4. Set the repository variable `SELF_HOSTED_MACOS` to `true` (Settings > Secrets and variables > Actions > Variables).

CI then adds an `Audio core (macos self-hosted)` job, **on pushes to main only**, running with `RV_INTEGRATION=1`. It briefly changes that Mac's volume and mute and restores them. With the variable unset (the default) CI is unchanged.
