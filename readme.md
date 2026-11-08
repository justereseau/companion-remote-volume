# Remote Volume

A small native app that lets you control the OS audio volume over a WebSocket connection. It is a single Rust binary for Windows, macOS and Linux, with a settings page served on your own computer. It is a fork of [ultimategmbh/remote-volume](https://github.com/ultimategmbh/remote-volume).

## Features

- Get, set, increase or decrease the audio volume
- Get, mute, unmute or toggle the mute state of the audio volume
- Get the current audio output device
- Per-app volume and mute ([protocol v2](#protocol-v2)) on Windows and Linux (PulseAudio / PipeWire)
- Media keys: play/pause, next and previous track, like a keyboard's ([protocol v2](#media-keys)), on Windows and Linux
- Function keys F1 to F24, e.g. as hotkeys for other apps ([protocol v2](#function-keys)), on Windows and Linux (X11)

Connected clients are kept in sync automatically:

- **After every command** (`setVolume`, `increaseVolume`, `decreaseVolume`, `mute`, `unmute`, `toggleMute`) the app reads the resulting state and sends what changed to **all** connected clients straight away. This happens whether or not polling is enabled.
- **On Linux**, changes made outside the app (media keys, pavucontrol, an app starting or stopping, the default output changing) are pushed straight away: the app subscribes to PulseAudio / PipeWire change events. It reconnects by itself when the sound server restarts.
- **Polling** (optional, configurable interval) picks up such changes on Windows and macOS, such as media keys, the Windows mixer or other apps. On Linux it is not needed.
- When a client connects, it receives the full state. If anything changed since the last update, the other clients get those changes too.

Updates contain only the fields that changed, one message per field, for example `{ "volume": 42 }` or `{ "muted": true }`.

## Supported OS

- Windows (WASAPI)
- macOS on Arm (CoreAudio)
- Linux with PulseAudio or PipeWire (`pipewire-pulse`); the `libpulse0` package must be installed

## Installation

The installers set the app up for your user, with no admin rights: it runs in the background, starts at every login and [keeps itself up to date](#updates). Your settings stay when you upgrade or uninstall.

**Windows:** download and run `remote-volume-setup-windows-x64.exe` from the [latest release](https://github.com/justereseau/companion-remote-volume/releases/latest). The setup is not signed, so SmartScreen may warn about it (**More info**, **Run anyway**). When the firewall asks, allow Remote Volume on private networks, or Companion cannot connect from another computer. Uninstall it from **Settings**, **Apps**.

**Linux and macOS (Apple silicon):**

```sh
curl -fsSL https://github.com/justereseau/companion-remote-volume/releases/latest/download/install.sh | sh
```

On Linux it needs a systemd user session and PulseAudio's client library (`sudo apt install libpulse0`). To uninstall, run the installed program with `--uninstall`: `~/.local/share/remote-volume/remote-volume` on Linux, `~/Library/Application Support/RemoteVolume/remote-volume` on macOS.

The app has no tray icon. Its settings are at <http://127.0.0.1:2502/>, and it logs to `remote-volume.log` next to its `config.json`.

**Without installing:** the archives in the release hold just the program. Unpack one and run `remote-volume`: it runs in a terminal window until you stop it with Ctrl+C, and does not update itself. macOS: the binary is not signed. If Gatekeeper blocks it, run `xattr -d com.apple.quarantine remote-volume` once. `remote-volume --install` turns any copy into an installed one.

### Updates

An installed app checks the GitHub releases once a day, a few minutes after it starts, and installs a newer version by itself. Installing restarts the app, so connected clients drop for a moment and reconnect. Turn **Install updates automatically** off on the settings page to stop this; **Update now** there still works.

An update is installed only if the release's manifest carries a valid signature from the release key built into the app, its version is newer than the running one, the download matches the checksum in the manifest, and the new program runs on this computer and reports that version. Otherwise nothing changes, and the reason is on the settings page and in the log. Updates cannot be triggered over the WebSocket, only from the settings page on the computer itself.

## Setup

Open the settings page at `http://127.0.0.1:2502/` (start the app with `--open` to open it in your browser). There you can

- select the port on which clients can connect to the WebSocket server (default `2501`).
- choose whether clients may connect from the network or only from this computer.
- set an optional access token and the allowed browser origins.
- enable or disable polling for changes made outside the app, and set the polling interval.
- turn per-app volume and mute on or off (on by default).

The settings are saved in `config.json` in `%APPDATA%\RemoteVolume`, `~/Library/Application Support/RemoteVolume` or `~/.config/RemoteVolume` (`--config-dir` or `RV_CONFIG_DIR` change it). On the first start the settings of the old Electron app are imported if they are found. Run `remote-volume --help` for all options.

## Security

Anyone who can reach the WebSocket port can change your volume. To limit that:

- **Accept connections from: This computer only.** Binds the server to `127.0.0.1`. Use it when the controller (e.g. Companion) runs on the same machine.
- **Access token.** When set, clients must connect with `ws://<host>:<port>/?token=<token>` or send an `Authorization: Bearer <token>` header. Otherwise the connection is refused with HTTP 401.
- **Browser pages are rejected.** Connections carrying an `Origin` header (that is, from a web page) are refused with HTTP 403. This stops any website you visit from controlling your volume through `ws://localhost:2501`. Native clients such as Companion send no `Origin` and are unaffected. To allow a specific web UI, add its origin in the settings page.
- Messages are limited to 1 KB, 20 commands/second per client and 32 simultaneous clients.
- The settings page can change the access token, so it listens on `127.0.0.1` only and accepts changes only from its own page. Anyone who can use your computer can still open it.
- **Privacy:** with per-app volume and mute on, connected clients can see which apps are playing audio (e.g. `spotify`, `discord`). Turn it off in the settings page if that is a concern; the `apps` list then disappears and the app actions are refused.

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
  2. Merge the release PR. The CI run on `main` creates the `vX.Y.Z` tag and a draft GitHub release, attaches the Windows/macOS/Linux programs, archives, installers, `SHA256SUMS.txt` and the signed update manifest built in that same run, then publishes it.
- If the run fails before publishing, the release stays a draft that nobody can download. Run **Actions → CI → Run workflow** from the tag (e.g. `v1.4.0`) with `release_tag` set to it.
- A published release cannot change: with the repository setting **Immutable releases**, its files and tag are locked, and the workflow refuses to touch it. Only its title and notes can still be edited. To fix a bad release, make a new one. Check a release, or a file you downloaded from it, with:
  ```bash
  gh release verify vX.Y.Z --repo justereseau/companion-remote-volume
  gh release verify-asset vX.Y.Z remote-volume-setup-windows-x64.exe --repo justereseau/companion-remote-volume
  ```

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

### Errors

A refused or failed action is answered with `{ "action": "<action>", "response": { "error": "<text>" } }`. When the message has no readable `action`, or the rate limit is hit, the answer is `{ "error": "<text>" }`. Clients may match on these texts:

| Error | When |
|---|---|
| `Invalid action` | Unknown `action` |
| `Invalid value for <action>. …` | `value` missing, not an integer or out of range |
| `Invalid app for <action>. …` | `app` missing, not a string, empty, over 256 characters or with control characters |
| `Failed to process message` | Not a JSON object |
| `Failed to execute action` | The OS call failed |
| `Rate limit exceeded` | More than 20 messages/second |
| `Not supported on this OS` | Protocol v2: the feature is unavailable here (see `capabilities`) |
| `App not found` | Protocol v2: no running app matches `app` |
| `No media player found` | Protocol v2: a media key was sent but no media player is running (Linux) |

## Protocol v2

Version 2 only **adds** to version 1: every v1 message, reply and state update keeps its shape, so v1 clients keep working unchanged. Clients should ignore keys they don't know; future versions follow the same rule.

### Capabilities

What this computer supports right now. It depends on the OS and on the moment (for example, Linux without a running PulseAudio/PipeWire server reports `false`), and per-app audio can be turned off in the settings. The state sent on connect carries it as a last `capabilities` key, and `getCapabilities` asks again:

```json
{ "action": "getCapabilities" }
```

```json
{
	"capabilities": {
		"protocolVersion": 2,
		"version": "1.7.1",
		"outputDevices": { "list": false, "select": false },
		"apps": { "volume": true, "mute": true },
		"media": { "keys": true },
		"keyboard": { "functionKeys": true }
	}
}
```

`version` is the version of the running app; a build from source adds `-dev`, and older apps do not send it.

An action whose capability is `false` is answered with `Not supported on this OS` and does nothing. `outputDevices` is reserved for device selection, which is not implemented yet. Capabilities are not pushed when they change; ask again with `getCapabilities`.

### Per-app volume and mute

An **app** is every audio session of one program, grouped. Its `id` is the program's file name, lowercased and without `.exe` (`spotify`, `chrome`, `discord`), so it stays the same when the program restarts. `name` is its display name. On Windows the system sounds (notifications, alerts) are the app `system`, named as in the Windows volume mixer.

- `volume` is relative to the master volume (Windows session volume; a PulseAudio/PipeWire stream volume). With PulseAudio's `flat-volumes` on, raising an app can also raise the master volume.
- When a program has several sessions (browser tabs, helper processes), the reported `volume` is the highest of them, `muted` is `true` only when every session is muted, and every action applies to all of them.
- The `app` in an action is matched against the `id`, ignoring case and a trailing `.exe` (`Spotify`, `spotify.exe` and `spotify` are the same), then against the display name.

**State:** `apps` is part of the state sent on connect and of `getState`, and the whole list is pushed again whenever anything in it changes (an app starts or stops, a volume or mute changes):

```json
{ "apps": [{ "id": "spotify", "name": "Spotify", "volume": 80, "muted": false }] }
```

`apps` is absent where per-app audio is unavailable or turned off. A client that had a list gets `{ "apps": null }` when it is turned off.

**Actions:**

| Action | Payload | Capability |
|---|---|---|
| `getApps` | `{ "action": "getApps" }`, replies `{ "apps": [...] }` | `apps.volume` or `apps.mute` |
| `setAppVolume` | `{ "action": "setAppVolume", "app": "spotify", "value": 40 }` (0 - 100) | `apps.volume` |
| `increaseAppVolume` | `{ "action": "increaseAppVolume", "app": "spotify", "value": 10 }` (1 - 99) | `apps.volume` |
| `decreaseAppVolume` | `{ "action": "decreaseAppVolume", "app": "spotify", "value": 10 }` (1 - 99) | `apps.volume` |
| `muteApp` | `{ "action": "muteApp", "app": "discord" }` | `apps.mute` |
| `unmuteApp` | `{ "action": "unmuteApp", "app": "discord" }` | `apps.mute` |
| `toggleAppMute` | `{ "action": "toggleAppMute", "app": "discord" }` | `apps.mute` |

Like the v1 commands, the changing actions send no reply: the new `apps` list is pushed to every client. If the app is not running the reply is `App not found`, so a client can grey out its button.

**Support:**

| OS | Volume | Mute | Status |
|---|---|---|---|
| Linux, PulseAudio / PipeWire | yes | yes | supported. An app is a program's playback streams; changes made in other mixers (pavucontrol) are pushed without polling |
| Windows | yes | yes | supported. Changes made in the Windows mixer are picked up by polling |
| macOS | no | maybe on 14.2+ | being investigated ([#19](https://github.com/Justereseau/companion-remote-volume/issues/19)) |
| Linux, ALSA only | no | no | not possible |

### Media keys

The same as pressing the media keys of a keyboard on that computer: the OS sends them to the media player it picks.

| Action | Payload | Capability |
|---|---|---|
| `mediaPlayPause` | `{ "action": "mediaPlayPause" }` | `media.keys` |
| `mediaNextTrack` | `{ "action": "mediaNextTrack" }` | `media.keys` |
| `mediaPreviousTrack` | `{ "action": "mediaPreviousTrack" }` | `media.keys` |

There is no reply on success, and no state changes. On Linux the answer is `No media player found` when no player is running.

| OS | Status |
|---|---|
| Windows | supported: real key presses (`SendInput`). Windows ignores them while an app running as administrator has the focus. |
| Linux | supported: desktops route media keys to players over MPRIS (D-Bus); the app does the same with `dbus-send`, picking the player that is playing, else a paused one. Needs a D-Bus session (any desktop session). |
| macOS | not yet: sending media keys needs the Accessibility permission |

### Function keys

Presses and releases F1 to F24 on that computer, like a keyboard: the focused window gets the key, or the app that registered it as a global hotkey. F13 to F24 are not on most keyboards, so they make good hotkeys for other apps (OBS, Discord, ...).

| Action | Payload | Capability |
|---|---|---|
| `pressFunctionKey` | `{ "action": "pressFunctionKey", "value": 13 }` (1 - 24 for F1 - F24) | `keyboard.functionKeys` |

There is no reply on success, and no state changes.

| OS | Status |
|---|---|
| Windows | supported: real key presses (`SendInput`). Windows ignores them while an app running as administrator has the focus. |
| Linux | X11 only, with `xdotool` installed (`sudo apt install xdotool`): Wayland does not accept key presses from other programs, except for apps running through XWayland while they have the focus. Checked when the app starts; without `xdotool` or a display the capability is `false`. |
| macOS | not yet: sending keys needs the Accessibility permission |

## Credits

Based on [remote-volume](https://github.com/ultimategmbh/remote-volume) by Ultimate Technology Solutions GmbH, whose first versions used [node-loudness](https://github.com/LinusU/node-loudness).

This application can be controlled by [Bitfocus Companion](https://bitfocus.io/companion)

![Companion Badge](./docs/images/companion_badge.png)
