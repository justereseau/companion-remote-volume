# Audio core contract

`remote-volume-core` is a standalone binary that owns all OS audio calls. The Electron app (and later the all-Rust app, see #8) runs it as a **sidecar** and talks to it over **stdin/stdout**. This file is the contract between the two. It is protocol version **1**.

Status: spike. What the binary implements today is marked **implemented**; everything else is reserved so the shape does not have to change when #9 (device selection) and #10 (per-app volume) land.

## Transport

- Spawn the binary with piped stdin and stdout. No arguments.
- **One JSON object per line**, UTF-8, terminated by `\n` (`\r\n` is accepted on input). The core writes `\n` only.
- stdout carries protocol messages only. stderr is free-form diagnostics; never parse it.
- A request line is at most **4096 bytes** (without the newline). Longer lines are discarded and answered with `line_too_long`. Blank lines are ignored.
- Requests are handled **one at a time, in order**; every request gets exactly one reply, in the same order. Events (below) can be interleaved between replies.
- The core writes a `ready` event before it reads anything. Clients should wait for it.
- The core exits with status 0 when **stdin closes**. A parent that dies therefore never leaves it behind. To stop it, close stdin.
- If the core cannot write to stdout (parent gone) it exits quietly. Any other I/O failure exits with status 1 and a line on stderr.

## Requests

```json
{ "id": 1, "action": "setVolume", "value": 40 }
```

| Field | Type | Notes |
|---|---|---|
| `id` | integer or string | **Required.** Echoed in the reply. The client picks it; the core never looks at it otherwise. |
| `action` | string | **Required.** Names follow `lib/protocol.js` where an equivalent exists. |
| `value` | any | Only for actions that take one. |
| `device` | string | Reserved for #9: id of an output device. Today any non-null `device` is refused with `unsupported`. Omit it. |

Unknown extra fields are ignored.

## Actions

All actions work on the **default output device** (the one the OS plays system sound through), re-resolved on every call, so a device change is picked up immediately.

| Action | `value` | Result | Status |
|---|---|---|---|
| `getCapabilities` | – | capabilities object (below) | implemented |
| `listOutputDevices` | – | `{ "devices": [{ "id", "name", "default" }] }` | implemented |
| `getVolume` | – | `{ "volume": 0-100 }` | implemented |
| `setVolume` | integer 0-100 | `{ "volume": 0-100 }`, read back from the OS after setting | implemented |
| `getMuted` | – | `{ "muted": bool }` | implemented |
| `setMuted` | boolean | `{ "muted": bool }`, read back from the OS after setting | implemented |
| `getState` | – | `{ "volume", "muted", "outputDevice" }`, same fields as the WebSocket `getState` reply; `outputDevice` is the default device's `name` or `null` | implemented |
| `setDefaultOutputDevice` | – (uses `device`) | – | reserved, #12-#15 |
| `listSessions`, `getSessionVolume`, `setSessionVolume`, `getSessionMuted`, `setSessionMuted` | – | per-app audio | reserved, #16-#18 |

The rest of the WebSocket protocol (`increaseVolume`, `decreaseVolume`, `mute`, `unmute`, `toggleMute`, `isMuted`) is **not** duplicated in the core. The JS server composes them from `getVolume`/`setVolume`/`getMuted`/`setMuted`, as `lib/volume.js` does today, so the WebSocket protocol stays backward compatible without the core knowing about it.

### Values

- **Volume** is an integer percent, 0-100. Integral JSON numbers such as `50.0` are accepted, as in the JS server (`Number.isInteger`). Anything else is `invalid_value`.
- **Volume 0 means 0.** The core sets the endpoint to exactly zero and the reply shows what the OS reports. This is the fix for the old Windows helper, which ignored `0` (see `HELPER_ZERO_ARG` in `impl/windows/index.js`). Replies show the value **read back after the change**, so drivers that quantise steps are reported honestly; clients should allow ±1.
- Volume above 100% (PulseAudio boost) is reported as 100.
- **Muted** is a JSON boolean. Mute is independent of volume: changing one never changes the other.
- **Device `id`** is the OS identifier and is stable across calls: the MMDevice endpoint id on Windows, the CoreAudio device UID on macOS, the sink name on Linux (PulseAudio and PipeWire). **`name`** is for humans: the friendly name on Windows, the CoreAudio name on macOS, the sink description on Linux. Do not match on `name`.
- Exactly one listed device has `"default": true`, or none when the OS has no default output.

## Replies

Success:

```json
{ "id": 1, "ok": true, "result": { "volume": 40 } }
```

Failure:

```json
{ "id": 1, "ok": false, "error": { "code": "no_device", "message": "..." } }
```

`code` is for programs and is part of the contract. `message` is for logs and may change.

If the request was so broken that its `id` could not be read (invalid JSON, not an object, missing or malformed `id`, oversized line), `id` is `null`.

## Error model

| `code` | When | Client should |
|---|---|---|
| `parse_error` | The line is not UTF-8 or not JSON. `id` is `null`. | Fix the client. |
| `invalid_request` | JSON, but not an object; `id` missing or not an integer/string; `action` missing or not a string. | Fix the client. |
| `line_too_long` | Request longer than 4096 bytes. `id` is `null`. | Fix the client. |
| `unknown_action` | Unrecognised `action`. A newer client talking to an older core gets this. | Check `getCapabilities`/`protocol`. |
| `invalid_value` | `value` missing, wrong type or out of range. Checked **before** the OS is touched. | Fix the client. |
| `no_device` | There is no default output device (headset unplugged, hosted CI runner, all sinks gone). | Report "no device"; retry later or wait for a `devicesChanged` event once events exist. |
| `unsupported` | This device or OS cannot do it: macOS device without a mute control, `device` argument today, any call on an OS without a backend. | Hide or disable the control. |
| `backend_unavailable` | The OS audio service is not reachable: no PulseAudio/PipeWire server on Linux, Windows audio service stopped. | Report "no audio service"; retry later. |
| `internal` | An OS call failed unexpectedly. The message carries the OS error (HRESULT, `OSStatus`). | Log it, surface a generic error. |

The core never crashes on a bad request or an OS error: it replies and keeps serving. A core that exits is a bug (or stdin closed).

On top of this, the Electron server keeps its own rules (1 KB WebSocket payload, 20 msg/s, token, `Origin`). The core trusts its parent and does not rate-limit.

## Capabilities

Returned by `getCapabilities` and as the data of the `ready` event:

```json
{
  "protocol": 1,
  "version": "0.1.0",
  "os": "linux",
  "backend": "pulse",
  "available": true,
  "devices": true,
  "volume": true,
  "mute": true,
  "sessions": false
}
```

| Field | Meaning |
|---|---|
| `protocol` | Contract version. Bumped only for breaking changes; new actions, fields and events are not breaking. |
| `version` | Core build version. |
| `os` | `windows`, `macos`, `linux` (Rust `std::env::consts::OS`). |
| `backend` | `wasapi`, `coreaudio`, `pulse`, or `none` (OS without a backend). |
| `available` | The OS audio API or server answered. `false` means every other capability is `false` and commands fail with `backend_unavailable`. |
| `devices` | `listOutputDevices` works. |
| `volume`, `mute` | The **default output device** has the control. With no default device they describe the API instead (a hosted Windows runner reports `true`; Linux without a server reports `false`). |
| `sessions` | Per-app volume/mute. Always `false` in the spike. |

Capabilities are probed on each call, so a client may re-ask after `backend_unavailable` or `no_device` to see whether things changed.

## Events

Messages with an `event` field and no `id`:

```json
{ "event": "ready", "data": { ...capabilities } }
```

| Event | `data` | Status |
|---|---|---|
| `ready` | capabilities | implemented |
| `volumeChanged` | `{ "volume", "muted" }` of the default device | reserved: removes the need to poll |
| `devicesChanged` | `{ "devices": [...] }` | reserved: hot-plug and default-device changes |
| `sessionsChanged` | session list | reserved, #16-#18 |

A client must ignore events it does not know. Because replies and events share a stream, tell them apart by the presence of `id` vs `event`.

## Per OS

| | Windows | macOS | Linux |
|---|---|---|---|
| API | WASAPI: `IMMDeviceEnumerator`, `IAudioEndpointVolume` (`windows` crate) | CoreAudio `AudioObject*` property API | libpulse (`libpulse-binding`); PipeWire via its pulse server |
| Volume | `MasterVolumeLevelScalar` | virtual main volume (`vmvc`), else per-channel `volm` | sink volume, average of channels; 100% = `PA_VOLUME_NORM` |
| Mute | `IAudioEndpointVolume::SetMute` | `mute` property; **absent on many devices** (#19) | sink mute |
| No device | HRESULT `0x80070490` → `no_device` | default device is `kAudioObjectUnknown` → `no_device` | no default sink → `no_device` |
| No audio service | `0x88890010`, `0x800706BA` → `backend_unavailable` | – | cannot connect → `backend_unavailable`, all capabilities `false` |

The Linux backend never starts a sound server (`NOAUTOSPAWN`) and does not fall back to ALSA.

## Running and testing

```sh
cd core
cargo build --release          # target/release/remote-volume-core
cargo test                     # unit tests + protocol tests over stdio, no audio needed

# Against the real default output device (changes the volume briefly and restores it):
RV_INTEGRATION=1 cargo test
# On a machine without an output device (hosted Windows CI):
RV_INTEGRATION=no-device cargo test
```

Try it by hand:

```sh
echo '{"id":1,"action":"listOutputDevices"}' | ./target/release/remote-volume-core
```

On Linux CI, `RV_EXPECT_SINK=ci_sink` additionally requires that sink to be listed and be the default.
