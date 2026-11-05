//! The WebSocket protocol Bitfocus Companion speaks: message validation, client admission, rate
//! limiting, capabilities. Port of `lib/protocol.js`; free of I/O so it is unit-tested directly.
//!
//! Protocol v1 (setVolume, increaseVolume, decreaseVolume, getState, isMuted, mute, unmute,
//! toggleMute) is unchanged. v2 adds `getCapabilities`, the per-app actions, string targets
//! (`app`, `device`), capability gating and the "not found" errors; see the README.

use std::time::Instant;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::backend::Features;
use crate::config::WsConfig;

/// Hard limit for a single incoming frame. Valid messages are < 100 bytes.
pub const MAX_PAYLOAD_BYTES: usize = 1024;
/// Maximum number of simultaneously connected clients.
pub const MAX_CLIENTS: usize = 32;
/// Announced in `capabilities.protocolVersion`. v2 only adds to v1.
pub const PROTOCOL_VERSION: u32 = 2;
/// Longest allowed target (`app` / `device`), in characters.
pub const MAX_TARGET_LENGTH: usize = 256;

/// Error texts that are part of the protocol (clients may match on them).
pub mod errors {
    pub const NOT_SUPPORTED: &str = "Not supported on this OS";
    pub const DEVICE_NOT_FOUND: &str = "Device not found";
    pub const APP_NOT_FOUND: &str = "App not found";
    pub const EXECUTION_FAILED: &str = "Failed to execute action";
    pub const BAD_MESSAGE: &str = "Failed to process message";
    pub const INVALID_ACTION: &str = "Invalid action";
    pub const RATE_LIMITED: &str = "Rate limit exceeded";
}

/// Which message key carries an action's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    App,
    Device,
}

impl TargetKind {
    pub fn key(self) -> &'static str {
        match self {
            TargetKind::App => "app",
            TargetKind::Device => "device",
        }
    }

    /// The text for "this target does not exist (any more)".
    pub fn not_found(self) -> &'static str {
        match self {
            TargetKind::App => errors::APP_NOT_FOUND,
            TargetKind::Device => errors::DEVICE_NOT_FOUND,
        }
    }
}

/// An integer `value` in `min..=max`; `error` is the text sent when it is not.
#[derive(Debug, Clone, Copy)]
pub struct ValueSpec {
    pub min: i64,
    pub max: i64,
    pub error: &'static str,
}

/// What one action needs and how the server treats it.
#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    pub name: &'static str,
    pub value: Option<ValueSpec>,
    /// The string target this action needs; unknown keys in a message are ignored.
    pub target: Option<TargetKind>,
    /// Dotted capability, e.g. `"apps.volume"`: the action answers "Not supported" while it is false.
    pub requires: Option<&'static str>,
    /// The command's result is sent to the caller.
    pub reply: bool,
    /// After the command, read the state again and push what changed to every client.
    pub mutates: bool,
}

impl ActionSpec {
    pub const fn new(name: &'static str) -> ActionSpec {
        ActionSpec {
            name,
            value: None,
            target: None,
            requires: None,
            reply: false,
            mutates: false,
        }
    }
}

const fn ranged(name: &'static str, min: i64, max: i64, error: &'static str) -> ActionSpec {
    ActionSpec {
        value: Some(ValueSpec { min, max, error }),
        mutates: true,
        ..ActionSpec::new(name)
    }
}

const fn replying(name: &'static str) -> ActionSpec {
    ActionSpec {
        reply: true,
        ..ActionSpec::new(name)
    }
}

const fn mutating(name: &'static str) -> ActionSpec {
    ActionSpec {
        mutates: true,
        ..ActionSpec::new(name)
    }
}

/// The actions of protocol v1 plus `getCapabilities`.
pub const BUILTIN_ACTIONS: [ActionSpec; 9] = [
    ranged(
        "setVolume",
        0,
        100,
        "Invalid value for setVolume. Must be an integer between 0 and 100.",
    ),
    ranged(
        "increaseVolume",
        1,
        99,
        "Invalid value for increaseVolume. Must be an integer between 1 and 99.",
    ),
    ranged(
        "decreaseVolume",
        1,
        99,
        "Invalid value for decreaseVolume. Must be an integer between 1 and 99.",
    ),
    replying("getState"),
    replying("getCapabilities"),
    replying("isMuted"),
    mutating("mute"),
    mutating("unmute"),
    mutating("toggleMute"),
];

const fn app_ranged(name: &'static str, min: i64, max: i64, error: &'static str) -> ActionSpec {
    ActionSpec {
        target: Some(TargetKind::App),
        requires: Some("apps.volume"),
        ..ranged(name, min, max, error)
    }
}

const fn app_mute(name: &'static str) -> ActionSpec {
    ActionSpec {
        target: Some(TargetKind::App),
        requires: Some("apps.mute"),
        ..mutating(name)
    }
}

/// Per-app volume and mute (protocol v2). Each action except `getApps` names its `app`.
pub const APP_ACTIONS: [ActionSpec; 7] = [
    ActionSpec {
        requires: Some("apps"),
        ..replying("getApps")
    },
    app_ranged(
        "setAppVolume",
        0,
        100,
        "Invalid value for setAppVolume. Must be an integer between 0 and 100.",
    ),
    app_ranged(
        "increaseAppVolume",
        1,
        99,
        "Invalid value for increaseAppVolume. Must be an integer between 1 and 99.",
    ),
    app_ranged(
        "decreaseAppVolume",
        1,
        99,
        "Invalid value for decreaseAppVolume. Must be an integer between 1 and 99.",
    ),
    app_mute("muteApp"),
    app_mute("unmuteApp"),
    app_mute("toggleAppMute"),
];

/// One validated client message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub action: &'static str,
    pub value: Option<i64>,
    pub app: Option<String>,
    pub device: Option<String>,
}

/// A message that was refused; `action` is set when the action name was readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub action: Option<String>,
    pub error: String,
}

fn reject(action: Option<&str>, error: impl Into<String>) -> Rejection {
    Rejection {
        action: action.map(String::from),
        error: error.into(),
    }
}

/// The reply for a refused message, in the existing shapes.
pub fn error_reply(action: Option<&str>, error: &str) -> Value {
    match action {
        Some(a) => json!({ "action": a, "response": { "error": error } }),
        None => json!({ "error": error }),
    }
}

/// `Number.isInteger` for a JSON number, as an `i64` (out-of-range values fail the range check).
fn as_integer(value: &Value) -> Option<i64> {
    let n = value.as_number()?;
    if let Some(i) = n.as_i64() {
        return Some(i);
    }
    let f = n.as_f64()?;
    (f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15).then_some(f as i64)
}

/// A target is a name or id passed on to native code: 1-256 characters, none of them a control
/// character. (Well-formed Unicode is guaranteed by the JSON parser, which refuses lone surrogates.)
pub fn is_valid_target(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(char::is_control) && s.chars().count() <= MAX_TARGET_LENGTH
}

fn target_error(action: &str, key: &str) -> String {
    format!(
        "Invalid {key} for {action}. Must be a string of 1-{MAX_TARGET_LENGTH} characters without control characters."
    )
}

/// Parse and validate one client message against an action table.
pub fn parse_message(raw: &[u8], actions: &[ActionSpec]) -> Result<Message, Rejection> {
    let text = String::from_utf8_lossy(raw);
    let Ok(Value::Object(msg)) = serde_json::from_str::<Value>(&text) else {
        return Err(reject(None, errors::BAD_MESSAGE));
    };
    let name = msg.get("action").and_then(Value::as_str);
    let Some(spec) = name.and_then(|n| actions.iter().find(|a| a.name == n)) else {
        let shown: Option<String> = name.map(|n| n.chars().take(64).collect());
        return Err(reject(shown.as_deref(), errors::INVALID_ACTION));
    };

    let mut message = Message {
        action: spec.name,
        value: None,
        app: None,
        device: None,
    };
    if let Some(range) = spec.value {
        match msg.get("value").and_then(as_integer) {
            Some(v) if (range.min..=range.max).contains(&v) => message.value = Some(v),
            _ => return Err(reject(Some(spec.name), range.error)),
        }
    }
    if let Some(kind) = spec.target {
        match msg.get(kind.key()).and_then(Value::as_str) {
            Some(t) if is_valid_target(t) => {
                let slot = match kind {
                    TargetKind::App => &mut message.app,
                    TargetKind::Device => &mut message.device,
                };
                *slot = Some(t.to_string());
            }
            _ => return Err(reject(Some(spec.name), target_error(spec.name, kind.key()))),
        }
    }
    Ok(message)
}

/// `capabilities` as sent on the wire. Key order is part of the documented shape.
pub fn capabilities_json(features: &Features) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "outputDevices": { "list": features.devices_list, "select": features.devices_select },
        "apps": { "volume": features.apps_volume, "mute": features.apps_mute },
    })
}

/// Whether the dotted capability (`"apps.volume"`) is currently supported. `"apps"` means volume or
/// mute. Unknown paths are not.
pub fn has_capability(features: &Features, path: &str) -> bool {
    match path {
        "apps" => features.apps_volume || features.apps_mute,
        "outputDevices.list" => features.devices_list,
        "outputDevices.select" => features.devices_select,
        "apps.volume" => features.apps_volume,
        "apps.mute" => features.apps_mute,
        _ => false,
    }
}

/// Structural equality for state fields: scalars strictly, lists by position, objects by key.
/// Used to decide whether a list-valued field needs to be sent again.
pub fn same_value(a: &Value, b: &Value) -> bool {
    a == b
}

/// Constant-time string comparison (hashing first equalises lengths).
pub fn safe_equal(a: &str, b: &str) -> bool {
    let (x, y) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    x.iter()
        .zip(y.iter())
        .fold(0u8, |acc, (p, q)| acc | (p ^ q))
        == 0
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract a token from an `Authorization: Bearer` header or the `?token=` query parameter.
pub fn extract_token(authorization: Option<&str>, request_target: &str) -> String {
    if let Some(rest) = authorization.and_then(|a| a.strip_prefix("Bearer ")) {
        return rest.trim().to_string();
    }
    let query = request_target.split_once('?').map_or("", |(_, q)| q);
    let query = query.split('#').next().unwrap_or("");
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k) == "token").then(|| percent_decode(v))
        })
        .next()
        .unwrap_or_default()
}

/// What a client presented when it connected; kept so access-rule changes can be re-checked.
#[derive(Debug, Clone, Default)]
pub struct Handshake {
    /// The request target, e.g. `/?token=abc`.
    pub url: String,
    pub origin: Option<String>,
    pub authorization: Option<String>,
    pub remote: String,
}

/// An HTTP status and reason for refusing a WebSocket upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    pub code: u16,
    pub reason: &'static str,
}

/// Decide whether an incoming upgrade request may become a WebSocket client.
pub fn admit_client(
    handshake: &Handshake,
    ws: &WsConfig,
    client_count: usize,
) -> Result<(), Refusal> {
    if client_count >= MAX_CLIENTS {
        return Err(Refusal {
            code: 503,
            reason: "Too many clients",
        });
    }
    // Browsers always send Origin on WebSocket handshakes; native clients normally don't.
    // Blocking unknown origins stops arbitrary web pages from controlling the volume.
    if let Some(origin) = &handshake.origin {
        if !ws.allowed_origins.iter().any(|o| o == origin) {
            return Err(Refusal {
                code: 403,
                reason: "Origin not allowed",
            });
        }
    }
    if !ws.token.is_empty() {
        let presented = extract_token(handshake.authorization.as_deref(), &handshake.url);
        if presented.is_empty() || !safe_equal(&presented, &ws.token) {
            return Err(Refusal {
                code: 401,
                reason: "Invalid or missing token",
            });
        }
    }
    Ok(())
}

/// Token-bucket limiter, one per connection. Every accepted command touches the OS audio API, so
/// unbounded message rates are a DoS vector.
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_second: f64) -> RateLimiter {
        Self::starting_at(capacity, refill_per_second, Instant::now())
    }

    pub fn starting_at(capacity: u32, refill_per_second: f64, now: Instant) -> RateLimiter {
        RateLimiter {
            capacity: f64::from(capacity),
            refill_per_sec: refill_per_second,
            tokens: f64::from(capacity),
            last: now,
        }
    }

    pub fn try_remove(&mut self) -> bool {
        self.try_remove_at(Instant::now())
    }

    pub fn try_remove_at(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

impl Default for RateLimiter {
    /// 20 messages per second, bursts up to 20.
    fn default() -> Self {
        RateLimiter::new(20, 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn parse(raw: &str) -> Result<Message, Rejection> {
        parse_message(raw.as_bytes(), &BUILTIN_ACTIONS)
    }

    fn ok(action: &'static str, value: Option<i64>) -> Result<Message, Rejection> {
        Ok(Message {
            action,
            value,
            app: None,
            device: None,
        })
    }

    #[test]
    fn valid_v1_actions() {
        assert_eq!(
            parse(r#"{"action":"setVolume","value":42}"#),
            ok("setVolume", Some(42))
        );
        assert_eq!(parse(r#"{"action":"mute"}"#), ok("mute", None));
        assert_eq!(parse(r#"{"action":"isMuted"}"#), ok("isMuted", None));
        assert_eq!(
            parse(r#"{"action":"getCapabilities"}"#),
            ok("getCapabilities", None)
        );
        assert_eq!(
            parse_message(br#"{"action":"mute"}"#, &BUILTIN_ACTIONS),
            ok("mute", None)
        );
        // integral floats are integers, as with Number.isInteger
        assert_eq!(
            parse(r#"{"action":"setVolume","value":50.0}"#),
            ok("setVolume", Some(50))
        );
        assert_eq!(
            parse(r#"{"action":"setVolume","value":5e1}"#),
            ok("setVolume", Some(50))
        );
    }

    #[test]
    fn keys_that_are_not_part_of_an_action_are_ignored() {
        assert_eq!(
            parse(r#"{"action":"mute","app":"spotify","extra":1}"#),
            ok("mute", None)
        );
        assert_eq!(
            parse(r#"{"action":"setVolume","value":5,"device":"x"}"#),
            ok("setVolume", Some(5))
        );
    }

    #[test]
    fn bad_values() {
        for value in [
            "-1", "101", "50.5", "\"50\"", "null", "1e300", "true", "[]", "{}",
        ] {
            let r = parse(&format!(r#"{{"action":"setVolume","value":{value}}}"#));
            assert_eq!(
                r,
                Err(reject(
                    Some("setVolume"),
                    "Invalid value for setVolume. Must be an integer between 0 and 100."
                )),
                "{value}"
            );
        }
        assert!(parse(r#"{"action":"setVolume"}"#).is_err());
        assert_eq!(
            parse(r#"{"action":"increaseVolume","value":0}"#),
            Err(reject(
                Some("increaseVolume"),
                "Invalid value for increaseVolume. Must be an integer between 1 and 99."
            ))
        );
        assert!(parse(r#"{"action":"decreaseVolume","value":100}"#).is_err());
        assert!(parse(r#"{"action":"decreaseVolume","value":99}"#).is_ok());
    }

    #[test]
    fn unknown_actions_and_garbage() {
        assert_eq!(
            parse(r#"{"action":"constructor"}"#),
            Err(reject(Some("constructor"), "Invalid action"))
        );
        assert_eq!(
            parse(r#"{"action":"__proto__"}"#),
            Err(reject(Some("__proto__"), "Invalid action"))
        );
        assert_eq!(
            parse(r#"{"action":5}"#),
            Err(reject(None, "Invalid action"))
        );
        assert_eq!(parse(r#"{}"#), Err(reject(None, "Invalid action")));
        for raw in ["null", "[]", "\"x\"", "{", "", "5"] {
            assert_eq!(
                parse(raw),
                Err(reject(None, "Failed to process message")),
                "{raw}"
            );
        }
        let long = format!(r#"{{"action":"{}"}}"#, "x".repeat(200));
        assert_eq!(parse(&long).unwrap_err().action.unwrap().len(), 64);
        assert_eq!(
            parse_message(&[0xff, 0xfe], &BUILTIN_ACTIONS),
            Err(reject(None, "Failed to process message"))
        );
    }

    #[test]
    fn app_actions_need_an_app_and_their_own_ranges() {
        let parse_apps = |v: Value| parse_message(v.to_string().as_bytes(), &APP_ACTIONS);
        let m = parse_apps(json!({"action":"setAppVolume","app":"spotify","value":0})).unwrap();
        assert_eq!(
            (m.action, m.value, m.app.as_deref()),
            ("setAppVolume", Some(0), Some("spotify"))
        );
        assert_eq!(
            parse_apps(json!({"action":"increaseAppVolume","app":"spotify","value":0}))
                .unwrap_err()
                .error,
            "Invalid value for increaseAppVolume. Must be an integer between 1 and 99."
        );
        assert!(parse_apps(json!({"action":"decreaseAppVolume","app":"x","value":99})).is_ok());
        assert!(parse_apps(json!({"action":"toggleAppMute","app":"x"})).is_ok());
        assert!(parse_apps(json!({"action":"muteApp"})).is_err());
        let all = parse_apps(json!({"action":"getApps","app":"ignored"})).unwrap();
        assert_eq!((all.action, all.app), ("getApps", None));
        for spec in APP_ACTIONS {
            let volume = spec.name.contains("Volume");
            assert_eq!(spec.mutates, spec.name != "getApps", "{}", spec.name);
            assert_eq!(spec.reply, spec.name == "getApps", "{}", spec.name);
            let needs = match spec.name {
                "getApps" => "apps",
                _ if volume => "apps.volume",
                _ => "apps.mute",
            };
            assert_eq!(spec.requires, Some(needs), "{}", spec.name);
        }
        let only_mute = Features {
            apps_mute: true,
            ..Features::default()
        };
        assert!(has_capability(&only_mute, "apps"));
        assert!(!has_capability(&Features::default(), "apps"));
    }

    // Device actions arrive with #12, so the target rules are exercised with a table that declares
    // both kinds.
    fn targeted() -> Vec<ActionSpec> {
        let mut t = BUILTIN_ACTIONS.to_vec();
        t.push(ActionSpec {
            value: Some(ValueSpec {
                min: 0,
                max: 100,
                error: "bad value",
            }),
            target: Some(TargetKind::App),
            ..ActionSpec::new("setAppVolume")
        });
        t.push(ActionSpec {
            target: Some(TargetKind::App),
            ..ActionSpec::new("muteApp")
        });
        t.push(ActionSpec {
            target: Some(TargetKind::Device),
            ..ActionSpec::new("setOutputDevice")
        });
        t
    }

    fn parse_t(v: Value) -> Result<Message, Rejection> {
        parse_message(v.to_string().as_bytes(), &targeted())
    }

    #[test]
    fn target_valid() {
        let m = parse_t(json!({"action":"setAppVolume","app":"spotify","value":40})).unwrap();
        assert_eq!(
            (m.action, m.value, m.app.as_deref(), m.device),
            ("setAppVolume", Some(40), Some("spotify"), None)
        );
        let m = parse_t(json!({"action":"setOutputDevice","device":"{0.0.0.00000000}.{a1b2}"}))
            .unwrap();
        assert_eq!(m.device.as_deref(), Some("{0.0.0.00000000}.{a1b2}"));
        // printable non-ASCII, spaces and shell metacharacters are plain data
        for app in ["Café Müller", "日本語", "a b", "$(rm -rf /)", "x'; y", "😀"] {
            assert_eq!(
                parse_t(json!({"action":"muteApp","app":app}))
                    .unwrap()
                    .app
                    .as_deref(),
                Some(app)
            );
        }
        assert!(parse_t(json!({"action":"muteApp","app":"a".repeat(256)})).is_ok());
    }

    #[test]
    fn target_uses_only_the_declared_key() {
        assert!(parse_t(json!({"action":"muteApp","device":"speakers"})).is_err());
        assert!(parse_t(json!({"action":"setOutputDevice","app":"spotify"})).is_err());
        let m = parse_t(json!({"action":"muteApp","app":"spotify","device":"speakers","junk":[1]}))
            .unwrap();
        assert_eq!((m.app.as_deref(), m.device), (Some("spotify"), None));
    }

    #[test]
    fn target_missing_empty_or_wrong_type() {
        for v in [
            json!({"action":"muteApp"}),
            json!({"action":"muteApp","app":null}),
            json!({"action":"muteApp","app":""}),
            json!({"action":"muteApp","app":42}),
            json!({"action":"muteApp","app":true}),
            json!({"action":"muteApp","app":["spotify"]}),
            json!({"action":"muteApp","app":{"name":"spotify"}}),
            json!({"action":"setOutputDevice"}),
        ] {
            let r = parse_t(v.clone()).unwrap_err();
            assert!(
                r.action.is_some() && r.error.starts_with("Invalid "),
                "{v}: {r:?}"
            );
        }
        assert_eq!(
            parse_t(json!({"action":"muteApp"})).unwrap_err().error,
            "Invalid app for muteApp. Must be a string of 1-256 characters without control characters."
        );
        assert!(parse_t(json!({"action":"setOutputDevice"}))
            .unwrap_err()
            .error
            .starts_with("Invalid device for setOutputDevice."));
    }

    #[test]
    fn target_too_long_counts_characters() {
        assert!(parse_t(json!({"action":"muteApp","app":"a".repeat(257)})).is_err());
        assert!(is_valid_target(&"😀".repeat(256)));
        assert!(!is_valid_target(&"😀".repeat(257)));
    }

    #[test]
    fn target_control_characters() {
        for bad in [
            "a\0b", "a\nb", "a\rb", "a\tb", "a\u{1b}b", "a\u{7f}b", "a\u{85}b", "\0",
        ] {
            assert!(
                parse_t(json!({"action":"muteApp","app":bad})).is_err(),
                "{bad:?}"
            );
            assert!(
                parse_t(json!({"action":"setOutputDevice","device":bad})).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn lone_surrogates_never_get_through() {
        let raw = br#"{"action":"muteApp","app":"a\ud800"}"#;
        assert_eq!(
            parse_message(raw, &targeted()),
            Err(reject(None, "Failed to process message"))
        );
        let ok = r#"{"action":"muteApp","app":"\ud83d\ude00"}"#;
        assert_eq!(
            parse_message(ok.as_bytes(), &targeted())
                .unwrap()
                .app
                .as_deref(),
            Some("😀")
        );
    }

    #[test]
    fn value_is_checked_before_the_target_and_still_on_targeted_actions() {
        let r = parse_t(json!({"action":"setAppVolume","app":"spotify","value":101})).unwrap_err();
        assert_eq!(r, reject(Some("setAppVolume"), "bad value"));
        assert!(parse_t(json!({"action":"setAppVolume","app":"spotify"})).is_err());
    }

    #[test]
    fn a_maximal_targeted_message_fits_the_payload_limit() {
        let worst =
            json!({"action":"increaseAppVolume","app":"€".repeat(MAX_TARGET_LENGTH),"value":99})
                .to_string();
        assert!(worst.len() < MAX_PAYLOAD_BYTES, "{} bytes", worst.len());
    }

    #[test]
    fn capabilities_shape_and_order() {
        let none = capabilities_json(&Features::default());
        assert_eq!(
            none,
            json!({"protocolVersion":2,"outputDevices":{"list":false,"select":false},"apps":{"volume":false,"mute":false}})
        );
        assert_eq!(
            none.to_string(),
            r#"{"protocolVersion":2,"outputDevices":{"list":false,"select":false},"apps":{"volume":false,"mute":false}}"#
        );
        let some = Features {
            apps_volume: true,
            devices_select: true,
            ..Features::default()
        };
        assert_eq!(capabilities_json(&some)["apps"]["volume"], true);
        assert!(has_capability(&some, "apps.volume"));
        assert!(has_capability(&some, "outputDevices.select"));
        assert!(!has_capability(&some, "apps.mute"));
        assert!(!has_capability(&some, "nope.nothing"));
    }

    #[test]
    fn error_replies_keep_the_v1_shapes() {
        assert_eq!(
            error_reply(Some("mute"), "x").to_string(),
            r#"{"action":"mute","response":{"error":"x"}}"#
        );
        assert_eq!(error_reply(None, "x").to_string(), r#"{"error":"x"}"#);
    }

    #[test]
    fn same_value_is_structural() {
        let apps = json!([{"id":"spotify","volume":40,"muted":false}]);
        assert!(same_value(&apps, &apps.clone()));
        assert!(
            same_value(&apps, &json!([{"muted":false,"volume":40,"id":"spotify"}])),
            "key order is irrelevant"
        );
        assert!(!same_value(
            &apps,
            &json!([{"id":"spotify","volume":41,"muted":false}])
        ));
        assert!(!same_value(&apps, &json!([{"id":"spotify","volume":40}])));
        assert!(!same_value(&json!([1, 2]), &json!([2, 1])));
        assert!(!same_value(&json!(null), &json!([])));
        assert!(!same_value(&json!(1), &json!("1")));
        assert!(same_value(&json!(null), &json!(null)));
    }

    fn cfg(token: &str, origins: &[&str]) -> WsConfig {
        WsConfig {
            port: 0,
            host: "127.0.0.1".into(),
            token: token.into(),
            allowed_origins: origins.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn hs(url: &str, origin: Option<&str>, auth: Option<&str>) -> Handshake {
        Handshake {
            url: url.into(),
            origin: origin.map(String::from),
            authorization: auth.map(String::from),
            remote: "127.0.0.1:1".into(),
        }
    }

    #[test]
    fn browser_origins_are_blocked_unless_allowed_native_clients_pass() {
        assert!(admit_client(&hs("/", None, None), &cfg("", &[]), 0).is_ok());
        assert_eq!(
            admit_client(
                &hs("/", Some("https://evil.example"), None),
                &cfg("", &[]),
                0
            ),
            Err(Refusal {
                code: 403,
                reason: "Origin not allowed"
            })
        );
        assert!(admit_client(
            &hs("/", Some("http://localhost:8000"), None),
            &cfg("", &["http://localhost:8000"]),
            0
        )
        .is_ok());
        // exact match only
        assert!(admit_client(
            &hs("/", Some("http://localhost:8000"), None),
            &cfg("", &["http://localhost:80"]),
            0
        )
        .is_err());
    }

    #[test]
    fn token_is_enforced_when_configured() {
        let c = cfg("s3cret", &[]);
        assert_eq!(
            admit_client(&hs("/", None, None), &c, 0).unwrap_err().code,
            401
        );
        assert_eq!(
            admit_client(&hs("/?token=wrong", None, None), &c, 0)
                .unwrap_err()
                .code,
            401
        );
        assert_eq!(
            admit_client(&hs("/?token=", None, None), &c, 0)
                .unwrap_err()
                .code,
            401
        );
        assert!(admit_client(&hs("/?token=s3cret", None, None), &c, 0).is_ok());
        assert!(admit_client(&hs("/?a=1&token=s3cret", None, None), &c, 0).is_ok());
        assert!(admit_client(&hs("/", None, Some("Bearer s3cret")), &c, 0).is_ok());
        assert!(admit_client(&hs("/", None, Some("Bearer  s3cret ")), &c, 0).is_ok());
        assert_eq!(
            admit_client(&hs("/", None, Some("Basic s3cret")), &c, 0)
                .unwrap_err()
                .code,
            401
        );
        // a Bearer header wins over the query string, even when empty
        assert_eq!(
            admit_client(&hs("/?token=s3cret", None, Some("Bearer ")), &c, 0)
                .unwrap_err()
                .code,
            401
        );
        // the origin is checked before the token
        assert_eq!(
            admit_client(&hs("/?token=s3cret", Some("http://x.test"), None), &c, 0)
                .unwrap_err()
                .code,
            403
        );
    }

    #[test]
    fn query_tokens_are_percent_decoded() {
        let c = cfg("a b&c%d", &[]);
        assert!(admit_client(&hs("/?token=a%20b%26c%25d", None, None), &c, 0).is_ok());
        assert!(admit_client(&hs("/?token=a+b%26c%25d", None, None), &c, 0).is_ok());
        assert_eq!(
            extract_token(None, "/?token=%zz"),
            "%zz",
            "invalid escapes stay as they are"
        );
        assert_eq!(extract_token(None, "/?token=abc#frag"), "abc");
        assert_eq!(extract_token(None, "/?token"), "");
        assert_eq!(extract_token(None, "/"), "");
        assert_eq!(extract_token(None, "/?%74oken=x"), "x");
        assert_eq!(extract_token(None, "/?token=a%"), "a%");
    }

    #[test]
    fn the_client_count_is_limited() {
        assert_eq!(
            admit_client(&hs("/", None, None), &cfg("", &[]), MAX_CLIENTS),
            Err(Refusal {
                code: 503,
                reason: "Too many clients"
            })
        );
        assert!(admit_client(&hs("/", None, None), &cfg("", &[]), MAX_CLIENTS - 1).is_ok());
    }

    #[test]
    fn safe_equal_compares_content() {
        assert!(safe_equal("abc", "abc"));
        assert!(!safe_equal("abc", "abd"));
        assert!(!safe_equal("abc", "abcd"));
        assert!(safe_equal("", ""));
    }

    #[test]
    fn rate_limiter_refills_over_time() {
        let t0 = Instant::now();
        let mut rl = RateLimiter::starting_at(2, 1.0, t0);
        assert!(rl.try_remove_at(t0));
        assert!(rl.try_remove_at(t0));
        assert!(!rl.try_remove_at(t0));
        assert!(!rl.try_remove_at(t0 + Duration::from_millis(500)));
        assert!(rl.try_remove_at(t0 + Duration::from_millis(1000)));
        // never above capacity
        let later = t0 + Duration::from_secs(100);
        assert!(rl.try_remove_at(later) && rl.try_remove_at(later) && !rl.try_remove_at(later));
    }

    #[test]
    fn default_limiter_allows_20_per_second() {
        let t0 = Instant::now();
        let mut rl = RateLimiter::starting_at(20, 20.0, t0);
        assert_eq!((0..25).filter(|_| rl.try_remove_at(t0)).count(), 20);
    }
}
