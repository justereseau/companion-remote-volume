//! Request parsing, validation and dispatch. Free of OS code: it only talks to `dyn Backend`,
//! so it is unit-tested with a mock. The wire format is described in CONTRACT.md.

use serde_json::{json, Map, Value};

use crate::backend::{AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode};

/// Bumped on breaking wire changes. Adding actions, fields or events is not breaking.
pub const PROTOCOL_VERSION: u32 = 1;
/// Longest accepted request line, without the newline. Valid requests are under 100 bytes.
pub const MAX_LINE_BYTES: usize = 4096;

fn capabilities_json(caps: &Capabilities) -> Value {
    json!({
        "protocol": PROTOCOL_VERSION,
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "backend": caps.backend,
        "available": caps.available,
        "devices": caps.devices,
        "volume": caps.volume,
        "mute": caps.mute,
        "sessions": false,
    })
}

fn device_json(device: &Device) -> Value {
    json!({ "id": device.id, "name": device.name, "default": device.default })
}

pub fn error_json(id: Value, code: ErrorCode, message: &str) -> Value {
    json!({ "id": id, "ok": false, "error": { "code": code.as_str(), "message": message } })
}

/// The first line the core writes, before any request is read.
pub fn ready_event(backend: &mut dyn Backend) -> Value {
    json!({ "event": "ready", "data": capabilities_json(&backend.capabilities()) })
}

/// Reply for a request line that was discarded for being longer than `MAX_LINE_BYTES`.
pub fn line_too_long() -> Value {
    error_json(
        Value::Null,
        ErrorCode::LineTooLong,
        &format!("request is longer than {MAX_LINE_BYTES} bytes"),
    )
}

/// Integer 0-100. Integral floats such as `50.0` pass, like `Number.isInteger` in the JS server.
fn parse_percent(value: Option<&Value>) -> Result<u8, AudioError> {
    let invalid = || {
        AudioError::new(
            ErrorCode::InvalidValue,
            "value must be an integer between 0 and 100",
        )
    };
    let number = value.and_then(Value::as_f64).ok_or_else(invalid)?;
    if number.fract() == 0.0 && (0.0..=100.0).contains(&number) {
        Ok(number as u8)
    } else {
        Err(invalid())
    }
}

fn parse_bool(value: Option<&Value>) -> Result<bool, AudioError> {
    value
        .and_then(Value::as_bool)
        .ok_or_else(|| AudioError::new(ErrorCode::InvalidValue, "value must be true or false"))
}

fn dispatch(
    backend: &mut dyn Backend,
    action: &str,
    request: &Map<String, Value>,
) -> AudioResult<Value> {
    // Reserved for device selection (#9); refuse rather than silently act on the default device.
    if request.get("device").is_some_and(|d| !d.is_null()) {
        return Err(AudioError::new(
            ErrorCode::Unsupported,
            "per-device commands are not implemented; omit \"device\" to use the default output",
        ));
    }
    let value = request.get("value");
    match action {
        "getCapabilities" => Ok(capabilities_json(&backend.capabilities())),
        "listOutputDevices" => {
            let devices = backend.list_output_devices()?;
            Ok(json!({ "devices": devices.iter().map(device_json).collect::<Vec<_>>() }))
        }
        "getVolume" => Ok(json!({ "volume": backend.get_volume()? })),
        "setVolume" => {
            let percent = parse_percent(value)?;
            Ok(json!({ "volume": backend.set_volume(percent)? }))
        }
        "getMuted" => Ok(json!({ "muted": backend.get_muted()? })),
        "setMuted" => {
            let muted = parse_bool(value)?;
            Ok(json!({ "muted": backend.set_muted(muted)? }))
        }
        "getState" => {
            let volume = backend.get_volume()?;
            let muted = backend.get_muted()?;
            let name = backend
                .list_output_devices()?
                .into_iter()
                .find(|d| d.default)
                .map(|d| d.name);
            Ok(json!({ "volume": volume, "muted": muted, "outputDevice": name }))
        }
        _ => Err(AudioError::new(
            ErrorCode::UnknownAction,
            format!("unknown action \"{}\"", action.chars().take(64).collect::<String>()),
        )),
    }
}

/// Handle one request line (without its newline) and produce exactly one reply.
pub fn handle_line(backend: &mut dyn Backend, line: &[u8]) -> Value {
    let text = match std::str::from_utf8(line) {
        Ok(t) => t,
        Err(_) => {
            return error_json(Value::Null, ErrorCode::ParseError, "request is not valid UTF-8")
        }
    };
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return error_json(Value::Null, ErrorCode::ParseError, &format!("invalid JSON: {e}")),
    };
    let Some(request) = parsed.as_object() else {
        return error_json(Value::Null, ErrorCode::InvalidRequest, "request must be a JSON object");
    };
    let id = match request.get("id") {
        Some(v) if v.is_string() || v.is_i64() || v.is_u64() => v.clone(),
        _ => {
            return error_json(
                Value::Null,
                ErrorCode::InvalidRequest,
                "id is required and must be a string or an integer",
            )
        }
    };
    let Some(action) = request.get("action").and_then(Value::as_str) else {
        return error_json(id, ErrorCode::InvalidRequest, "action is required and must be a string");
    };
    match dispatch(backend, action, request) {
        Ok(result) => json!({ "id": id, "ok": true, "result": result }),
        Err(e) => error_json(id, e.code, &e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory backend with one device, optionally "unplugged".
    struct Mock {
        volume: u8,
        muted: bool,
        plugged: bool,
    }

    impl Mock {
        fn new() -> Self {
            Mock {
                volume: 30,
                muted: false,
                plugged: true,
            }
        }
        fn check(&self) -> AudioResult<()> {
            if self.plugged {
                Ok(())
            } else {
                Err(AudioError::new(ErrorCode::NoDevice, "unplugged"))
            }
        }
    }

    impl Backend for Mock {
        fn capabilities(&mut self) -> Capabilities {
            Capabilities {
                backend: "mock",
                available: true,
                devices: true,
                volume: true,
                mute: true,
            }
        }
        fn list_output_devices(&mut self) -> AudioResult<Vec<Device>> {
            Ok(if self.plugged {
                vec![
                    Device { id: "a".into(), name: "Speakers".into(), default: true },
                    Device { id: "b".into(), name: "Headset".into(), default: false },
                ]
            } else {
                vec![]
            })
        }
        fn get_volume(&mut self) -> AudioResult<u8> {
            self.check().map(|_| self.volume)
        }
        fn set_volume(&mut self, percent: u8) -> AudioResult<u8> {
            self.check()?;
            self.volume = percent;
            Ok(self.volume)
        }
        fn get_muted(&mut self) -> AudioResult<bool> {
            self.check().map(|_| self.muted)
        }
        fn set_muted(&mut self, muted: bool) -> AudioResult<bool> {
            self.check()?;
            self.muted = muted;
            Ok(self.muted)
        }
    }

    fn call(backend: &mut Mock, line: &str) -> Value {
        handle_line(backend, line.as_bytes())
    }

    fn code(reply: &Value) -> &str {
        assert_eq!(reply["ok"], false, "{reply}");
        reply["error"]["code"].as_str().unwrap()
    }

    #[test]
    fn ready_event_carries_capabilities() {
        let ready = ready_event(&mut Mock::new());
        assert_eq!(ready["event"], "ready");
        assert_eq!(ready["data"]["protocol"], PROTOCOL_VERSION);
        assert_eq!(ready["data"]["backend"], "mock");
        assert_eq!(ready["data"]["sessions"], false);
    }

    #[test]
    fn set_volume_replies_with_the_value_read_back() {
        let mut m = Mock::new();
        let reply = call(&mut m, r#"{"id":7,"action":"setVolume","value":42}"#);
        assert_eq!(reply, json!({ "id": 7, "ok": true, "result": { "volume": 42 } }));
        assert_eq!(m.volume, 42);
    }

    #[test]
    fn volume_zero_reaches_the_backend() {
        let mut m = Mock::new();
        let reply = call(&mut m, r#"{"id":1,"action":"setVolume","value":0}"#);
        assert_eq!(reply["result"]["volume"], 0);
        assert_eq!(m.volume, 0);
    }

    #[test]
    fn string_ids_are_echoed() {
        let reply = call(&mut Mock::new(), r#"{"id":"abc","action":"getVolume"}"#);
        assert_eq!(reply["id"], "abc");
        assert_eq!(reply["result"]["volume"], 30);
    }

    #[test]
    fn integral_floats_are_accepted_as_percent() {
        let reply = call(&mut Mock::new(), r#"{"id":1,"action":"setVolume","value":50.0}"#);
        assert_eq!(reply["result"]["volume"], 50);
    }

    #[test]
    fn rejects_bad_volumes_without_touching_the_backend() {
        for bad in ["101", "-1", "1.5", "\"50\"", "null", "true", "1e3"] {
            let mut m = Mock::new();
            let reply = call(&mut m, &format!(r#"{{"id":1,"action":"setVolume","value":{bad}}}"#));
            assert_eq!(code(&reply), "invalid_value", "value {bad}");
            assert_eq!(m.volume, 30, "value {bad}");
        }
        let missing = call(&mut Mock::new(), r#"{"id":1,"action":"setVolume"}"#);
        assert_eq!(code(&missing), "invalid_value");
    }

    #[test]
    fn set_muted_requires_a_boolean() {
        let mut m = Mock::new();
        assert_eq!(call(&mut m, r#"{"id":1,"action":"setMuted","value":true}"#)["result"]["muted"], true);
        assert_eq!(code(&call(&mut m, r#"{"id":2,"action":"setMuted","value":1}"#)), "invalid_value");
        assert_eq!(code(&call(&mut m, r#"{"id":3,"action":"setMuted"}"#)), "invalid_value");
    }

    #[test]
    fn lists_devices() {
        let reply = call(&mut Mock::new(), r#"{"id":1,"action":"listOutputDevices"}"#);
        assert_eq!(
            reply["result"]["devices"],
            json!([
                { "id": "a", "name": "Speakers", "default": true },
                { "id": "b", "name": "Headset", "default": false },
            ])
        );
    }

    #[test]
    fn get_state_mirrors_the_websocket_state() {
        let reply = call(&mut Mock::new(), r#"{"id":1,"action":"getState"}"#);
        assert_eq!(
            reply["result"],
            json!({ "volume": 30, "muted": false, "outputDevice": "Speakers" })
        );
    }

    #[test]
    fn no_device_is_an_error_not_a_crash() {
        let mut m = Mock { plugged: false, ..Mock::new() };
        assert_eq!(code(&call(&mut m, r#"{"id":1,"action":"getVolume"}"#)), "no_device");
        assert_eq!(code(&call(&mut m, r#"{"id":2,"action":"setMuted","value":true}"#)), "no_device");
        let list = call(&mut m, r#"{"id":3,"action":"listOutputDevices"}"#);
        assert_eq!(list["result"]["devices"], json!([]));
    }

    #[test]
    fn malformed_requests_get_an_error_with_null_id() {
        let mut m = Mock::new();
        for (line, want) in [
            ("not json", "parse_error"),
            ("[1,2]", "invalid_request"),
            ("42", "invalid_request"),
            (r#"{"action":"getVolume"}"#, "invalid_request"),
            (r#"{"id":1.5,"action":"getVolume"}"#, "invalid_request"),
            (r#"{"id":null,"action":"getVolume"}"#, "invalid_request"),
        ] {
            let reply = call(&mut m, line);
            assert_eq!(code(&reply), want, "{line}");
            assert_eq!(reply["id"], Value::Null, "{line}");
        }
        assert_eq!(handle_line(&mut m, &[0xff, 0xfe])["error"]["code"], "parse_error");
    }

    #[test]
    fn missing_or_unknown_action_keeps_the_id() {
        let mut m = Mock::new();
        let missing = call(&mut m, r#"{"id":5}"#);
        assert_eq!((code(&missing), &missing["id"]), ("invalid_request", &json!(5)));
        let unknown = call(&mut m, r#"{"id":6,"action":"launchMissiles"}"#);
        assert_eq!((code(&unknown), &unknown["id"]), ("unknown_action", &json!(6)));
    }

    #[test]
    fn device_argument_is_refused_until_device_selection_exists() {
        let reply = call(&mut Mock::new(), r#"{"id":1,"action":"getVolume","device":"b"}"#);
        assert_eq!(code(&reply), "unsupported");
        let null = call(&mut Mock::new(), r#"{"id":2,"action":"getVolume","device":null}"#);
        assert_eq!(null["ok"], true);
    }
}
