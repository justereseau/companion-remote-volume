# Remote Volume

This small Electron application enables you to control the OS audio volume over a WebSocket connection.

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

- Windows Vista or later
- OSX on Arm & x64
- Linux\*

\*No autostart possible by the software setting - needs to be solved manually!

## Installation

Navigate to the [latest release](https://github.com/leonreucher/remote-volume/releases/latest) and download the corresponding file for your OS. Mac OS users will find a .dmg volume, containing the application. Windows users will find a .exe file which contains an installer.
Linux users will find a .deb file for installation.

## Setup

After installing the application, it will run as a menu bar application. By clicking on the menu bar item, you can either close the application or open the configuration screen.

Inside the configuration screen, you can

- select the port, on which clients can connect to the WebSocket server.
- enable or disable polling for changes made outside the app, and set the polling interval.
- select, wether the application should auto start together with the system.

- choose whether clients may connect from the network or only from this computer.
- set an optional access token.

![Setting Window](/docs/images/setting_window.png)

## Security

Anyone who can reach the WebSocket port can change your volume. To limit that:

- **Accept connections from: This computer only.** Binds the server to `127.0.0.1`. Use it when the controller (e.g. Companion) runs on the same machine.
- **Access token.** When set, clients must connect with `ws://<host>:<port>/?token=<token>` or send an `Authorization: Bearer <token>` header. Otherwise the connection is refused with HTTP 401.
- **Browser pages are rejected.** Connections carrying an `Origin` header (that is, from a web page) are refused with HTTP 403. This stops any website you visit from controlling your volume through `ws://localhost:2501`. Native clients such as Companion send no `Origin` and are unaffected. To allow a specific web UI, add its origin to `settings.websocket.allowedOrigins` in the config file (`config.json` in the app's user-data folder).
- Messages are limited to 1 KB, 20 commands/second per client and 32 simultaneous clients.

## Building from source

Requirements: Node.js 24+, and on Windows the "Desktop development with C++" workload of the Visual Studio Build Tools (for the native `naudiodon` module).

```sh
npm ci
npm test
npm start          # run in development
npm run build:win  # create dist/Remote Volume Setup <version>.exe
```

## Releasing

Releases are managed by [release-please](https://github.com/googleapis/release-please) from [Conventional Commits](https://www.conventionalcommits.org/):

- Write PR titles and commit messages as `fix: …` (patch), `feat: …` (minor) or `feat!: …` / a `BREAKING CHANGE:` footer (major). `chore:`, `ci:`, `docs:`, `test:` and `refactor:` don't trigger a release. The **Conventional commits** check enforces this on PRs. Renovate already writes `fix(deps): …` for anything that ships in the app, including Electron.
- On every push to `main`, release-please updates a **release PR** (`chore(main): release x.y.z`) that bumps `package.json`/`package-lock.json` and updates `CHANGELOG.md`.
- **To release:**
  1. Open the release PR. It is created with the built-in token, so CI does not start by itself. Go to **Actions → CI → Run workflow**, pick the `release-please--branches--main--components--remote-volume` branch and run it. This produces the required checks.
  2. Merge the release PR. The CI run on `main` creates the `vX.Y.Z` tag and GitHub release, then attaches the Windows/macOS/Linux installers and `SHA256SUMS.txt` built in that same run.
- If attaching the installers ever fails, run **Actions → CI → Run workflow** from the tag (e.g. `v1.2.0`) with `release_tag` set to it.

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

### 9. Get Capabilities

- **Action**: `getCapabilities`
- **Description**: Replies with what this machine supports right now. See [Protocol v2](#protocol-v2).
- **Payload**:
  ```json
  {
  	"action": "getCapabilities"
  }
  ```

## Protocol v2

Protocol v2 adds the building blocks for output-device selection and per-app volume. It **only adds** things; see [Compatibility](#compatibility).

### Capabilities

`getCapabilities` replies with `{ "capabilities": { … } }`. The same `capabilities` object is also the last key of the state message sent when a client connects.

```json
{
	"capabilities": {
		"protocolVersion": 2,
		"outputDevices": { "list": false, "select": false },
		"apps": { "volume": false, "mute": false }
	}
}
```

- `protocolVersion` is `2`.
- The flags depend on the OS **and** on the environment at the time of the question (for example, Linux without a Pulse/PipeWire server reports `false`), so ask again rather than caching forever. Only a literal `true` means supported. This version reports `false` for everything except `protocolVersion`, because no backend implements the new features yet.
- An action whose capability is `false` is answered with `{ "action": "<action>", "response": { "error": "Not supported on this OS" } }` and does nothing.
- Capabilities are not part of the state broadcast, and `getState` does not include them.

### Targets

An action that acts on something names it with a string key: `app` for per-app actions, `device` for device actions, for example `{ "action": "setAppVolume", "app": "spotify", "value": 40 }`. Each action takes only the key it declares; other keys are ignored.

A target must be a string of 1–256 characters (counted as Unicode characters) with no control characters (such as NUL, newline, tab, DEL), and valid Unicode text. A missing or invalid target is rejected with `Invalid <key> for <action>. Must be a string of 1-256 characters without control characters.` Targets are never put into a shell command or script; they reach the native audio code as program arguments or JSON. The 1 KB payload limit applies unchanged.

### Errors

Errors use the existing `{ "action": "…", "response": { "error": "…" } }` shape. Protocol v2 adds three texts, which clients may match on:

| Error                      | Meaning                                                                              |
| -------------------------- | ------------------------------------------------------------------------------------ |
| `Not supported on this OS` | The capability the action needs is `false` right now.                                |
| `Device not found`         | The `device` target does not exist (any more).                                       |
| `App not found`            | The `app` target does not exist (any more).                                          |

These are distinct from `Failed to execute action`, which still means an unexpected failure.

### Structured state

State fields keep being sent as one message per field (`{ "volume": 40 }`). Fields that hold lists, such as the upcoming `outputDevices` and `apps`, are sent in full, but only when their content changed since clients were last told. A scalar field is sent exactly as before.

### Compatibility

- Every v1 action, its payload, its validation and its replies are unchanged, byte for byte. Today's Bitfocus Companion module keeps working without an update.
- The only change a v1 client can observe is one additional key, `capabilities`, in the state message sent on connect. Clients must ignore keys they do not know, as they already have to for the other state messages.
- New versions add actions, keys and error texts but do not change or remove existing ones. `capabilities.protocolVersion` tells a client which additions to expect.
- The security model is unchanged: bind address, access token, `Origin` rejection, the 1 KB payload limit, 20 messages per second and 32 clients.

## Credits

The main part for controlling the system volume is based on the following project:
https://github.com/LinusU/node-loudness.

This application can be controlled by [Bitfocus Companion](https://bitfocus.io/companion)

![Companion Badge](./docs/images/companion_badge.png)
