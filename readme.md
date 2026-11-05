# Remote Volume

A small native app that lets you control the OS audio volume over a WebSocket connection. It is a single Rust binary for Windows, macOS and Linux, with a settings page served on your own computer. It is a fork of [ultimategmbh/remote-volume](https://github.com/ultimategmbh/remote-volume).

## Features

- Get, set, increase or decrease the audio volume
- Get, mute, unmute or toggle the mute state of the audio volume
- Get the current audio output device

Connected clients are kept in sync automatically:

- **After every command** (`setVolume`, `increaseVolume`, `decreaseVolume`, `mute`, `unmute`, `toggleMute`) the app reads the resulting state and sends what changed to **all** connected clients straight away. This happens whether or not polling is enabled.
- **Polling** (optional, configurable interval) also picks up changes made outside the app, such as media keys, the Windows mixer or other apps.
- When a client connects, it receives the full state. If anything changed since the last update, the other clients get those changes too.

Updates contain only the fields that changed, one message per field, for example `{ "volume": 42 }` or `{ "muted": true }`.

## Supported OS

- Windows (WASAPI)
- macOS on Arm (CoreAudio)
- Linux with PulseAudio or PipeWire (`pipewire-pulse`); the `libpulse0` package must be installed

## Installation

Download the archive for your OS from the [latest release](https://github.com/justereseau/companion-remote-volume/releases/latest), unpack it and run `remote-volume`. There is no installer and no tray icon: the app runs in a terminal window until you stop it with Ctrl+C. To start it with your computer, use your OS's own mechanism (Task Scheduler on Windows, a `launchd` agent on macOS, a `systemd` user service on Linux).

macOS: the binary is not signed. If Gatekeeper blocks it, run `xattr -d com.apple.quarantine remote-volume` once.

## Setup

Open the settings page at `http://127.0.0.1:2502/` (start the app with `--open` to open it in your browser). There you can

- select the port on which clients can connect to the WebSocket server (default `2501`).
- choose whether clients may connect from the network or only from this computer.
- set an optional access token and the allowed browser origins.
- enable or disable polling for changes made outside the app, and set the polling interval.

The settings are saved in `config.json` in `%APPDATA%\RemoteVolume`, `~/Library/Application Support/RemoteVolume` or `~/.config/RemoteVolume` (`--config-dir` or `RV_CONFIG_DIR` change it). On the first start the settings of the old Electron app are imported if they are found. Run `remote-volume --help` for all options.

## Security

Anyone who can reach the WebSocket port can change your volume. To limit that:

- **Accept connections from: This computer only.** Binds the server to `127.0.0.1`. Use it when the controller (e.g. Companion) runs on the same machine.
- **Access token.** When set, clients must connect with `ws://<host>:<port>/?token=<token>` or send an `Authorization: Bearer <token>` header. Otherwise the connection is refused with HTTP 401.
- **Browser pages are rejected.** Connections carrying an `Origin` header (that is, from a web page) are refused with HTTP 403. This stops any website you visit from controlling your volume through `ws://localhost:2501`. Native clients such as Companion send no `Origin` and are unaffected. To allow a specific web UI, add its origin in the settings page.
- Messages are limited to 1 KB, 20 commands/second per client and 32 simultaneous clients.
- The settings page can change the access token, so it listens on `127.0.0.1` only and accepts changes only from its own page. Anyone who can use your computer can still open it.

## Building from source

Requirements: [rustup](https://rustup.rs). On Windows the "Desktop development with C++" workload of the Visual Studio Build Tools, on macOS the Xcode command line tools, on Linux `libpulse-dev` and `pkg-config`.

```sh
cd core
cargo run --release --bin remote-volume -- --open
cargo test
```

More about the crate, its tests and the optional self-hosted Mac runner is in [core/README.md](core/README.md).

## Releasing

Releases are managed by [release-please](https://github.com/googleapis/release-please) from [Conventional Commits](https://www.conventionalcommits.org/):

- Write PR titles and commit messages as `fix: …` (patch), `feat: …` (minor) or `feat!: …` / a `BREAKING CHANGE:` footer (major). `chore:`, `ci:`, `docs:`, `test:` and `refactor:` don't trigger a release. The **Conventional commits** check enforces this on PRs. Renovate writes `fix(deps): …` for dependency updates.
- On every push to `main`, release-please updates a **release PR** (`chore(main): release x.y.z`) that bumps `version.txt`, `core/Cargo.toml` and `core/Cargo.lock` and updates `CHANGELOG.md`.
- **To release:**
  1. Open the release PR. It is created with the built-in token, so CI does not start by itself. Go to **Actions → CI → Run workflow**, pick the `release-please--branches--main--components--remote-volume` branch and run it. This produces the required checks.
  2. Merge the release PR. The CI run on `main` creates the `vX.Y.Z` tag and GitHub release, then attaches the Windows/macOS/Linux archives and `SHA256SUMS.txt` built in that same run.
- If attaching the archives ever fails, run **Actions → CI → Run workflow** from the tag (e.g. `v1.4.0`) with `release_tag` set to it.

## Usage

When using [Bitfocus Companion](https://bitfocus.io/companion) you don't need the following information - this is only for people, who would like to integrate the application in another project.

### 1. Set Volume

- **Action**: `setVolume`
- **Description**: Sets the system volume to a specified level.
- **Value Range**: 0 - 100
- **Payload**:

  ```json
  {
    "action": "setVolume",
    "value": <number>
  }
  ```

### 2. Get State

- **Action**: `getState`
- **Description**: Returns all current audio information
- **Payload**:
  ```json
  {
  	"action": "getState"
  }
  ```

### 3. Increase Volume

- **Action**: `increaseVolume`
- **Description**: Increases the system volume by a specified increment.
- **Value Range**: 1 - 99
- **Payload**:
  ```json
  {
  	"action": "increaseVolume",
    "value": <number>
  }
  ```

### 4. Decrease Volume

- **Action**: `decreaseVolume`
- **Description**: Decreases the system volume by a specified decrement.
- **Value Range**: 1 - 99
- **Payload**:
  ```json
  {
  	"action": "decreaseVolume",
    "value": <number>
  }
  ```

### 5. Mute

- **Action**: `mute`
- **Description**: Mutes the system audio.
- **Payload**:
  ```json
  {
  	"action": "mute"
  }
  ```

### 6. Unmute

- **Action**: `unmute`
- **Description**: Unmutes the system audio.
- **Payload**:
  ```json
  {
  	"action": "unmute"
  }
  ```

### 7. Toggle Mute

- **Action**: `toggleMute`
- **Description**: Toggles the mute state of the system audio.
- **Payload**:
  ```json
  {
  	"action": "toggleMute"
  }
  ```

### 8. Is Muted

- **Action**: `isMuted`
- **Description**: Checks if the system audio is currently muted. Replies with `{ "muted": <boolean> }`.
- **Payload**:
  ```json
  {
  	"action": "isMuted"
  }
  ```

## Credits

Based on [remote-volume](https://github.com/ultimategmbh/remote-volume) by Ultimate Technology Solutions GmbH, whose first versions used [node-loudness](https://github.com/LinusU/node-loudness).

This application can be controlled by [Bitfocus Companion](https://bitfocus.io/companion)

![Companion Badge](./docs/images/companion_badge.png)
