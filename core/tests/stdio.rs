//! Drives the real binary over stdio.
//!
//! Protocol tests run everywhere without audio hardware. The audio tests are opt-in, like
//! `test/integration/os-backend.test.js`, because they briefly change the system volume (and
//! restore it):
//!
//!   RV_INTEGRATION=1          real default output device: list, volume, exact 0, mute
//!   RV_INTEGRATION=no-device  machine without an output device (hosted Windows runners)
//!   RV_EXPECT_SINK=<id>       with =1: this device must be listed and be the default
//!
//! With neither set, the audio tests print a note and pass.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Core {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Core {
    fn spawn() -> Core {
        let mut child = Command::new(env!("CARGO_BIN_EXE_remote-volume-core"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the core binary");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Core {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    /// Spawn and consume the `ready` event.
    fn start() -> (Core, Value) {
        let mut core = Core::spawn();
        let ready = core.read();
        assert_eq!(
            ready["event"], "ready",
            "first line must be the ready event: {ready}"
        );
        (core, ready)
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read a line from the core");
        assert!(!line.is_empty(), "the core closed stdout");
        assert!(
            line.ends_with('\n'),
            "replies are newline-terminated: {line:?}"
        );
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply is not JSON ({e}): {line:?}"))
    }

    fn write_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn raw(&mut self, line: &str) -> Value {
        self.write_raw(format!("{line}\n").as_bytes());
        self.read()
    }

    fn call(&mut self, action: &str, value: Option<Value>) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({ "id": id, "action": action });
        if let Some(v) = value {
            request["value"] = v;
        }
        let reply = self.raw(&request.to_string());
        assert_eq!(reply["id"], id, "reply must echo the id: {reply}");
        reply
    }

    fn ok(&mut self, action: &str, value: Option<Value>) -> Value {
        let reply = self.call(action, value);
        assert_eq!(reply["ok"], true, "{action} failed: {reply}");
        reply["result"].clone()
    }

    fn error_code(&mut self, action: &str, value: Option<Value>) -> String {
        let reply = self.call(action, value);
        assert_eq!(reply["ok"], false, "{action} should fail: {reply}");
        reply["error"]["code"].as_str().unwrap().to_string()
    }

    fn volume(&mut self) -> u64 {
        self.ok("getVolume", None)["volume"].as_u64().unwrap()
    }

    fn set_volume(&mut self, percent: u64) -> u64 {
        self.ok("setVolume", Some(json!(percent)))["volume"]
            .as_u64()
            .unwrap()
    }

    fn muted(&mut self) -> bool {
        self.ok("getMuted", None)["muted"].as_bool().unwrap()
    }

    fn set_muted(&mut self, muted: bool) -> bool {
        self.ok("setMuted", Some(json!(muted)))["muted"]
            .as_bool()
            .unwrap()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.wait();
    }
}

fn mode() -> String {
    env::var("RV_INTEGRATION").unwrap_or_default()
}

#[test]
fn announces_itself_first() {
    let (_core, ready) = Core::start();
    let data = &ready["data"];
    assert_eq!(data["protocol"], 1);
    assert_eq!(data["os"], env::consts::OS);
    assert_eq!(data["sessions"], false);
    for key in ["backend", "version"] {
        assert!(data[key].is_string(), "{key}: {data}");
    }
    for key in ["available", "devices", "volume", "mute"] {
        assert!(data[key].is_boolean(), "{key}: {data}");
    }
}

#[test]
fn get_capabilities_matches_the_ready_event() {
    let (mut core, ready) = Core::start();
    assert_eq!(core.ok("getCapabilities", None), ready["data"]);
}

#[test]
fn rejects_malformed_requests_and_keeps_serving() {
    let (mut core, _) = Core::start();
    for (line, code) in [
        ("this is not json", "parse_error"),
        ("[]", "invalid_request"),
        (r#"{"action":"getVolume"}"#, "invalid_request"),
        (r#"{"id":1}"#, "invalid_request"),
        (r#"{"id":1,"action":"explode"}"#, "unknown_action"),
        (
            r#"{"id":1,"action":"setVolume","value":101}"#,
            "invalid_value",
        ),
        (
            r#"{"id":1,"action":"setVolume","value":-1}"#,
            "invalid_value",
        ),
        (
            r#"{"id":1,"action":"setVolume","value":"7"}"#,
            "invalid_value",
        ),
        (
            r#"{"id":1,"action":"setMuted","value":"yes"}"#,
            "invalid_value",
        ),
        (
            r#"{"id":1,"action":"getVolume","device":"x"}"#,
            "unsupported",
        ),
    ] {
        let reply = core.raw(line);
        assert_eq!(reply["ok"], false, "{line}: {reply}");
        assert_eq!(reply["error"]["code"], code, "{line}: {reply}");
        assert!(reply["error"]["message"].is_string(), "{line}: {reply}");
    }
    // Blank lines produce no reply, so the next answer belongs to the next request.
    core.write_raw(b"\n   \r\n");
    assert_eq!(core.call("getCapabilities", None)["ok"], true);
}

#[test]
fn survives_an_oversized_line() {
    let (mut core, _) = Core::start();
    let mut huge = vec![b'a'; 100_000];
    huge.push(b'\n');
    core.write_raw(&huge);
    let reply = core.read();
    assert_eq!(reply["error"]["code"], "line_too_long", "{reply}");
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(core.call("getCapabilities", None)["ok"], true);
}

#[test]
fn handles_crlf_and_a_final_line_without_newline() {
    let (mut core, _) = Core::start();
    core.write_raw(b"{\"id\":1,\"action\":\"getCapabilities\"}\r\n");
    assert_eq!(core.read()["id"], 1);
    core.write_raw(b"{\"id\":2,\"action\":\"getCapabilities\"}");
    core.stdin = None; // EOF terminates the last request
    assert_eq!(core.read()["id"], 2);
}

#[test]
fn exits_when_stdin_closes() {
    let (mut core, _) = Core::start();
    core.stdin = None;
    let status = core.child.wait().expect("wait for the core");
    assert!(status.success(), "{status}");
}

#[test]
fn audio_with_a_real_device() {
    if mode() != "1" {
        eprintln!("skipped: set RV_INTEGRATION=1 to run against the real default output device");
        return;
    }
    let (mut core, ready) = Core::start();
    let caps = &ready["data"];
    assert_eq!(caps["available"], true, "no audio backend answered: {caps}");
    assert_eq!(caps["devices"], true);
    assert_eq!(caps["volume"], true);

    let devices = core.ok("listOutputDevices", None)["devices"]
        .as_array()
        .unwrap()
        .clone();
    assert!(!devices.is_empty(), "no output devices listed");
    for d in &devices {
        assert!(d["id"].as_str().is_some_and(|s| !s.is_empty()), "{d}");
        assert!(d["name"].as_str().is_some_and(|s| !s.is_empty()), "{d}");
        assert!(d["default"].is_boolean(), "{d}");
    }
    assert_eq!(
        devices.iter().filter(|d| d["default"] == true).count(),
        1,
        "{devices:?}"
    );
    if let Some(expected) = env::var("RV_EXPECT_SINK").ok().filter(|v| !v.is_empty()) {
        let sink = devices
            .iter()
            .find(|d| d["id"] == expected.as_str())
            .unwrap_or_else(|| panic!("{expected} not listed in {devices:?}"));
        assert_eq!(
            sink["default"], true,
            "{expected} should be the default device"
        );
    }
    let state = core.ok("getState", None);
    let default_name = devices.iter().find(|d| d["default"] == true).unwrap()["name"].clone();
    assert_eq!(state["outputDevice"], default_name);

    // Run every step, restore the system state, then assert, so a failure leaves nothing behind.
    let original_volume = core.volume();
    let original_muted = core.muted_if(caps["mute"] == true);
    let target = if original_volume >= 50 {
        original_volume - 2
    } else {
        original_volume + 2
    };

    let set_reply = core.set_volume(target);
    let read_back = core.volume();
    let at_one = core.set_volume(1);
    let at_zero = core.set_volume(0);
    let zero_read_back = core.volume();
    let state_at_zero = core.ok("getState", None)["volume"].clone();

    let mute_steps = if caps["mute"] == true {
        let flipped = !original_muted.unwrap();
        let set = core.set_muted(flipped);
        let read = core.muted();
        let back = core.set_muted(!flipped);
        Some((flipped, set, read, back))
    } else {
        None
    };

    core.set_volume(original_volume);
    if let Some(m) = original_muted {
        core.set_muted(m);
    }
    let restored = core.volume();

    // Drivers may quantise volume steps; allow 1% of rounding like the JS integration test.
    assert!(
        set_reply.abs_diff(target) <= 1,
        "set {target}, reply {set_reply}"
    );
    assert_eq!(
        read_back, set_reply,
        "getVolume must agree with the setVolume reply"
    );
    assert!(at_one <= 2, "set 1, got {at_one}");
    assert_eq!(at_zero, 0, "volume 0 must really reach 0");
    assert_eq!(zero_read_back, 0);
    assert_eq!(state_at_zero, 0);
    assert!(
        restored.abs_diff(original_volume) <= 1,
        "restore {original_volume}, got {restored}"
    );
    if let Some((flipped, set, read, back)) = mute_steps {
        assert_eq!(set, flipped);
        assert_eq!(read, flipped);
        assert_eq!(back, !flipped);
    } else {
        // Not every macOS device has a mute control; it must say so clearly.
        let code = core.error_code("getMuted", None);
        assert_eq!(code, "unsupported");
    }
}

#[test]
fn audio_without_a_device() {
    if mode() != "no-device" {
        eprintln!("skipped: set RV_INTEGRATION=no-device on a machine without an output device");
        return;
    }
    let (mut core, ready) = Core::start();
    assert_eq!(
        ready["data"]["available"], true,
        "the audio API itself should answer: {ready}"
    );

    // Enumeration succeeds with an empty list...
    let devices = core.ok("listOutputDevices", None);
    assert_eq!(devices["devices"], json!([]));
    // ...and everything that needs a device fails cleanly (Windows: HRESULT 0x80070490).
    for (action, value) in [
        ("getVolume", None),
        ("setVolume", Some(json!(10))),
        ("getMuted", None),
        ("setMuted", Some(json!(true))),
        ("getState", None),
    ] {
        assert_eq!(core.error_code(action, value), "no_device", "{action}");
    }
    // The core is still alive and answering.
    assert_eq!(core.call("getCapabilities", None)["ok"], true);
}

impl Core {
    /// Mute state when the backend supports mute, else None.
    fn muted_if(&mut self, supported: bool) -> Option<bool> {
        supported.then(|| self.muted())
    }
}
