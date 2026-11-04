# Audio core

Rust binary that owns the OS audio calls and talks line-delimited JSON over stdio. The wire contract is in [CONTRACT.md](CONTRACT.md).

## Local setup

| OS | You need |
|---|---|
| all | [rustup](https://rustup.rs) (`winget install Rustlang.Rustup` on Windows). `rust-toolchain.toml` selects the stable channel and clippy. |
| Windows | Visual Studio Build Tools with the "Desktop development with C++" workload (MSVC + Windows SDK). |
| macOS | Xcode command line tools (`xcode-select --install`). |
| Linux | `sudo apt install libpulse-dev pkg-config`, plus a running PulseAudio or PipeWire server to test against. |

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
