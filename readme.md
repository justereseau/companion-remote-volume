# Remote Volume

This small Electron application enables you to control the OS audio volume over a WebSocket connection.

## Features

- Get, set, increase or decrease the audio volume
- Get, mute, unmute or toggle the mute state of the audio volume
- Get the current audio output device

The application has an built in feature to poll the system audio values (volume and mute state) - this can be enabled or disabled in the configuration window (you can also define a polling interval).
As soon as something changes, the updated values will be sent out to all connected WebSocket clients.

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
- enable or disable the polling feature and set a polling interval.
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

The main part for controlling the system volume is based on the following project:
https://github.com/LinusU/node-loudness.

This application can be controlled by [Bitfocus Companion](https://bitfocus.io/companion)

![Companion Badge](./docs/images/companion_badge.png)
