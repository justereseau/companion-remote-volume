//! Configuration: defaults, validation of untrusted input, and the on-disk store.
//!
//! Port of `lib/config.js`. Everything that comes from the settings page or from disk is treated as
//! untrusted and normalised by [`sanitize`]: unknown keys are dropped, invalid values fall back to
//! the previous value or the default. The JSON shape is the same as the Electron app's `settings`
//! object, so an existing configuration can be imported (see [`legacy_candidates`]).

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const CONFIG_VERSION: &str = "1.1.0";

/// `0.0.0.0` keeps the original behaviour (reachable from the LAN, e.g. a Companion machine);
/// `127.0.0.1` restricts control to clients on this computer.
pub const BIND_ADDRESSES: [&str; 2] = ["0.0.0.0", "127.0.0.1"];

pub const MAX_TOKEN_LENGTH: usize = 256;
pub const MAX_ORIGINS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsConfig {
    pub port: u16,
    pub host: String,
    /// Optional shared secret. Empty = no token required (original behaviour).
    pub token: String,
    /// Browser origins that may connect. Native clients (Companion, scripts) send no `Origin` header
    /// and are unaffected. Empty = reject every browser page, which prevents drive-by websites from
    /// changing the volume (cross-site WebSocket hijacking).
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polling {
    pub enabled: bool,
    /// Milliseconds, 100-10000.
    pub interval: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppsConfig {
    /// Report per-app audio (`apps` state, app actions) to clients. Connected clients can see which
    /// apps are playing audio; turning this off hides that and refuses the app actions.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub websocket: WsConfig,
    pub polling: Polling,
    pub apps: AppsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            websocket: WsConfig {
                port: 2501,
                host: "0.0.0.0".into(),
                token: String::new(),
                allowed_origins: Vec::new(),
            },
            polling: Polling {
                enabled: false,
                interval: 1000,
            },
            apps: AppsConfig { enabled: true },
        }
    }
}

impl Config {
    /// The shape the settings page and the config file use.
    pub fn to_json(&self) -> Value {
        json!({
            "websocket": {
                "port": self.websocket.port,
                "host": self.websocket.host,
                "token": self.websocket.token,
                "allowedOrigins": self.websocket.allowed_origins,
            },
            "polling": { "enabled": self.polling.enabled, "interval": self.polling.interval },
            "apps": { "enabled": self.apps.enabled },
            "version": CONFIG_VERSION,
        })
    }
}

/// An integer in `min..=max`, from a JSON integer or a numeric string; anything else is `None`.
/// Out-of-range integers are clamped (as the JS version does), not rejected.
fn clamp_int(value: Option<&Value>, min: i64, max: i64) -> Option<i64> {
    let number = match value? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().and_then(integral))?,
        Value::String(s) if !s.trim().is_empty() => integral(s.trim().parse::<f64>().ok()?)?,
        _ => return None,
    };
    Some(number.clamp(min, max))
}

fn integral(f: f64) -> Option<i64> {
    // Beyond 2^53 floats are not exact integers anymore; those clamp to the limits anyway.
    (f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15).then_some(f as i64)
}

/// Printable ASCII only, bounded length: the token travels in a URL query string.
fn sanitize_token(value: Option<&Value>) -> String {
    let Some(Value::String(s)) = value else {
        return String::new();
    };
    let trimmed = s.trim();
    if trimmed.len() <= MAX_TOKEN_LENGTH && trimmed.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        trimmed.to_string()
    } else {
        String::new()
    }
}

/// `http(s)://host[:port]`, nothing else: an origin has no path, query or whitespace.
fn is_origin(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("https://") {
        r
    } else if let Some(r) = lower.strip_prefix("http://") {
        r
    } else {
        return false;
    };
    !rest.is_empty() && !rest.contains('/') && !rest.chars().any(char::is_whitespace)
}

fn sanitize_origins(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|o| is_origin(o))
        .take(MAX_ORIGINS)
        .map(String::from)
        .collect()
}

/// A fresh, fully populated, validated config. Invalid or missing values keep `previous`.
pub fn sanitize(input: &Value, previous: &Config) -> Config {
    let empty = serde_json::Map::new();
    let src = input.as_object().unwrap_or(&empty);
    let ws = src
        .get("websocket")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let polling = src
        .get("polling")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let apps = src.get("apps").and_then(Value::as_object).unwrap_or(&empty);

    let host = match ws.get("host").and_then(Value::as_str) {
        Some(h) if BIND_ADDRESSES.contains(&h) => h.to_string(),
        _ => previous.websocket.host.clone(),
    };
    Config {
        websocket: WsConfig {
            port: clamp_int(ws.get("port"), 1, 65535).map_or(previous.websocket.port, |p| p as u16),
            host,
            token: if ws.contains_key("token") {
                sanitize_token(ws.get("token"))
            } else {
                previous.websocket.token.clone()
            },
            allowed_origins: if ws.contains_key("allowedOrigins") {
                sanitize_origins(ws.get("allowedOrigins"))
            } else {
                previous.websocket.allowed_origins.clone()
            },
        },
        polling: Polling {
            enabled: polling
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(previous.polling.enabled),
            interval: clamp_int(polling.get("interval"), 100, 10000)
                .map_or(previous.polling.interval, |i| i as u32),
        },
        apps: AppsConfig {
            enabled: apps
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(previous.apps.enabled),
        },
    }
}

/// Where `config.json` lives: `RV_CONFIG_DIR`, else the OS convention.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = env::var_os("RV_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    app_data_dir().join("RemoteVolume")
}

/// The per-user application data folder (`%APPDATA%`, `~/Library/Application Support`, `~/.config`).
fn app_data_dir() -> PathBuf {
    let home = || env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(windows) {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                env::var_os("USERPROFILE")
                    .map(|p| PathBuf::from(p).join("AppData").join("Roaming"))
                    .unwrap_or_default()
            })
    } else if cfg!(target_os = "macos") {
        home().join("Library").join("Application Support")
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
    }
}

/// Where the Electron app kept its settings (`electron-store`: `config.json` holding `{ settings }`
/// in the app's user-data folder). The folder name depends on how the app was packaged, so both
/// likely names are tried.
pub fn legacy_candidates() -> Vec<PathBuf> {
    let base = app_data_dir();
    ["Remote Volume", "remote-volume"]
        .iter()
        .map(|name| base.join(name).join("config.json"))
        .collect()
}

pub struct Store {
    path: PathBuf,
}

/// How the configuration was obtained, for the startup log.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    File,
    /// Imported from the Electron app's settings at this path.
    Imported(PathBuf),
    /// No usable file: defaults.
    Defaults,
}

impl Store {
    pub fn new(dir: &Path) -> Store {
        Store {
            path: dir.join("config.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load and sanitize the config. A missing or corrupt file gives the defaults; if there is no
    /// file yet, `legacy` paths (Electron settings) are tried first.
    pub fn load(&self, legacy: &[PathBuf]) -> (Config, Loaded) {
        let defaults = Config::default();
        if let Some(value) = read_json(&self.path) {
            return (sanitize(&value, &defaults), Loaded::File);
        }
        if !self.path.exists() {
            for candidate in legacy {
                if let Some(settings) =
                    read_json(candidate).and_then(|v| v.get("settings").cloned())
                {
                    return (
                        sanitize(&settings, &defaults),
                        Loaded::Imported(candidate.clone()),
                    );
                }
            }
        }
        (defaults, Loaded::Defaults)
    }

    /// Write atomically (temp file + rename). The file holds the access token, so on Unix it is
    /// readable by the owner only.
    pub fn save(&self, config: &Config) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(&config.to_json()).map_err(io::Error::other)?;
        fs::write(&tmp, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, &self.path)
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Config {
        Config::default()
    }

    #[test]
    fn defaults_match_the_electron_app() {
        let c = defaults();
        assert_eq!(c.websocket.port, 2501);
        assert_eq!(c.websocket.host, "0.0.0.0");
        assert_eq!(c.websocket.token, "");
        assert!(c.websocket.allowed_origins.is_empty());
        assert_eq!(
            c.polling,
            Polling {
                enabled: false,
                interval: 1000
            }
        );
    }

    #[test]
    fn garbage_input_gives_the_previous_config() {
        for input in [json!(null), json!(7), json!("x"), json!([]), json!({})] {
            assert_eq!(sanitize(&input, &defaults()), defaults(), "{input}");
        }
        let mut prev = defaults();
        prev.websocket.token = "abc".into();
        prev.websocket.port = 3000;
        assert_eq!(sanitize(&json!({}), &prev), prev);
    }

    #[test]
    fn port_is_clamped_and_accepts_numeric_strings() {
        let port = |v: Value| {
            sanitize(&json!({ "websocket": { "port": v } }), &defaults())
                .websocket
                .port
        };
        assert_eq!(port(json!(8080)), 8080);
        assert_eq!(port(json!("8080")), 8080);
        assert_eq!(port(json!(" 8080 ")), 8080);
        assert_eq!(port(json!(0)), 1);
        assert_eq!(port(json!(70000)), 65535);
        assert_eq!(port(json!(-5)), 1);
        assert_eq!(port(json!(8080.0)), 8080);
        for bad in [
            json!(80.5),
            json!("abc"),
            json!(""),
            json!(null),
            json!(true),
            json!([1]),
        ] {
            assert_eq!(port(bad), 2501);
        }
    }

    #[test]
    fn host_must_be_one_of_the_bind_addresses() {
        let host = |v: Value| {
            sanitize(&json!({ "websocket": { "host": v } }), &defaults())
                .websocket
                .host
        };
        assert_eq!(host(json!("127.0.0.1")), "127.0.0.1");
        assert_eq!(host(json!("0.0.0.0")), "0.0.0.0");
        assert_eq!(host(json!("192.168.1.5")), "0.0.0.0");
        assert_eq!(host(json!(5)), "0.0.0.0");
    }

    #[test]
    fn token_is_printable_ascii_and_bounded() {
        let token = |v: Value| {
            sanitize(&json!({ "websocket": { "token": v } }), &defaults())
                .websocket
                .token
        };
        assert_eq!(token(json!("  s3cret!  ")), "s3cret!");
        assert_eq!(token(json!("has space")), "");
        assert_eq!(token(json!("tab\there")), "");
        assert_eq!(token(json!("café")), "");
        assert_eq!(token(json!("a".repeat(256))), "a".repeat(256));
        assert_eq!(token(json!("a".repeat(257))), "");
        assert_eq!(token(json!(42)), "");
        assert_eq!(token(json!("")), "");
    }

    #[test]
    fn absent_token_and_origins_keep_the_previous_values_present_ones_replace_them() {
        let mut prev = defaults();
        prev.websocket.token = "keep".into();
        prev.websocket.allowed_origins = vec!["http://localhost:8080".into()];
        let kept = sanitize(&json!({ "websocket": { "port": 2600 } }), &prev);
        assert_eq!(kept.websocket.token, "keep");
        assert_eq!(
            kept.websocket.allowed_origins,
            prev.websocket.allowed_origins
        );
        let cleared = sanitize(
            &json!({ "websocket": { "token": "", "allowedOrigins": [] } }),
            &prev,
        );
        assert_eq!(cleared.websocket.token, "");
        assert!(cleared.websocket.allowed_origins.is_empty());
    }

    #[test]
    fn origins_are_filtered_trimmed_and_capped() {
        let origins = |v: Value| {
            sanitize(
                &json!({ "websocket": { "allowedOrigins": v } }),
                &defaults(),
            )
            .websocket
            .allowed_origins
        };
        assert_eq!(
            origins(json!([
                " http://localhost:8080 ",
                "HTTPS://Example.com",
                "ftp://x",
                "http://a/b",
                "http://",
                "no",
                5,
                "http://a b",
                "http://ok.test"
            ])),
            vec![
                "http://localhost:8080",
                "HTTPS://Example.com",
                "http://ok.test"
            ]
        );
        let many: Vec<String> = (0..50).map(|i| format!("http://h{i}.test")).collect();
        assert_eq!(origins(json!(many)).len(), MAX_ORIGINS);
        assert!(origins(json!("http://a.test")).is_empty());
    }

    #[test]
    fn polling_interval_is_clamped() {
        let interval = |v: Value| {
            sanitize(&json!({ "polling": { "interval": v } }), &defaults())
                .polling
                .interval
        };
        assert_eq!(interval(json!(50)), 100);
        assert_eq!(interval(json!(99999)), 10000);
        assert_eq!(interval(json!("250")), 250);
        assert_eq!(interval(json!("fast")), 1000);
        let on = sanitize(&json!({ "polling": { "enabled": true } }), &defaults());
        assert!(on.polling.enabled);
        assert!(
            !sanitize(&json!({ "polling": { "enabled": "yes" } }), &defaults())
                .polling
                .enabled
        );
    }

    #[test]
    fn app_tracking_is_on_by_default_and_only_a_boolean_changes_it() {
        assert!(defaults().apps.enabled);
        let off = sanitize(&json!({ "apps": { "enabled": false } }), &defaults());
        assert!(!off.apps.enabled);
        for keep in [
            json!({}),
            json!({ "apps": 1 }),
            json!({ "apps": { "enabled": "no" } }),
        ] {
            assert!(!sanitize(&keep, &off).apps.enabled, "{keep}");
        }
        assert_eq!(off.to_json()["apps"], json!({ "enabled": false }));
    }

    #[test]
    fn unknown_keys_are_dropped_and_json_round_trips() {
        let c = sanitize(
            &json!({ "evil": 1, "runOnStartup": true, "websocket": { "port": 3001, "x": 1 } }),
            &defaults(),
        );
        assert_eq!(c.websocket.port, 3001);
        let out = c.to_json();
        assert_eq!(out["version"], CONFIG_VERSION);
        assert!(out.get("evil").is_none());
        assert_eq!(sanitize(&out, &defaults()), c);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("rv-config-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn store_saves_and_loads() {
        let dir = temp_dir("roundtrip");
        let store = Store::new(&dir);
        assert_eq!(store.load(&[]), (defaults(), Loaded::Defaults));
        let mut c = defaults();
        c.websocket.port = 4000;
        c.websocket.token = "tok".into();
        c.polling = Polling {
            enabled: true,
            interval: 500,
        };
        store.save(&c).unwrap();
        assert_eq!(store.load(&[]), (c, Loaded::File));
        assert!(!dir.join("config.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_gives_defaults_and_is_not_overwritten_by_an_import() {
        let dir = temp_dir("corrupt");
        let store = Store::new(&dir);
        fs::write(store.path(), "{ not json").unwrap();
        let legacy = dir.join("legacy.json");
        fs::write(&legacy, r#"{"settings":{"websocket":{"port":4444}}}"#).unwrap();
        // The file exists (just unreadable), so a legacy import must not silently replace it.
        assert_eq!(store.load(&[legacy]), (defaults(), Loaded::Defaults));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn electron_settings_are_imported_once() {
        let dir = temp_dir("legacy");
        let store = Store::new(&dir.join("new"));
        let legacy = dir.join("old-config.json");
        fs::write(
            &legacy,
            r#"{"settings":{"websocket":{"port":2600,"host":"127.0.0.1","token":"abc","allowedOrigins":["http://localhost:1"]},"runOnStartup":true,"polling":{"enabled":true,"interval":250},"version":"1.1.0"}}"#,
        )
        .unwrap();
        let missing = dir.join("nope.json");
        let (c, how) = store.load(&[missing, legacy.clone()]);
        assert_eq!(how, Loaded::Imported(legacy.clone()));
        assert_eq!(c.websocket.port, 2600);
        assert_eq!(c.websocket.host, "127.0.0.1");
        assert_eq!(c.websocket.token, "abc");
        assert_eq!(
            c.polling,
            Polling {
                enabled: true,
                interval: 250
            }
        );
        store.save(&c).unwrap();
        // Once we have our own file the old one is not consulted again.
        fs::write(&legacy, r#"{"settings":{"websocket":{"port":9999}}}"#).unwrap();
        assert_eq!(store.load(&[legacy]).0.websocket.port, 2600);
        let _ = fs::remove_dir_all(&dir);
    }
}
