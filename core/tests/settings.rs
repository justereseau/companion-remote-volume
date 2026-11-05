//! The settings page over a real loopback socket: routes, the browser-facing security checks, and
//! that saving persists the config and applies it to the running WebSocket server.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;

use remote_volume_core::audio::AudioHandle;
use remote_volume_core::backend::{AudioResult, Backend, Capabilities, Device};
use remote_volume_core::config::{Config, Store};
use remote_volume_core::log::NoLog;
use remote_volume_core::server::Server;
use remote_volume_core::settings::{SettingsApp, SettingsHttp};
use serde_json::{json, Value};

struct Silent;

impl Backend for Silent {
    fn capabilities(&mut self) -> Capabilities {
        Capabilities::none("fake")
    }
    fn list_output_devices(&mut self) -> AudioResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn get_volume(&mut self) -> AudioResult<u8> {
        Ok(10)
    }
    fn set_volume(&mut self, percent: u8) -> AudioResult<u8> {
        Ok(percent)
    }
    fn get_muted(&mut self) -> AudioResult<bool> {
        Ok(false)
    }
    fn set_muted(&mut self, muted: bool) -> AudioResult<bool> {
        Ok(muted)
    }
}

struct Page {
    http: SettingsHttp,
    server: Server,
    dir: PathBuf,
    port: u16,
}

impl Drop for Page {
    fn drop(&mut self) {
        self.http.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rv-settings-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

async fn page(name: &str) -> Page {
    let mut config = Config::default();
    config.websocket.port = 0;
    config.websocket.host = "127.0.0.1".into();
    let log = Arc::new(NoLog);
    let server = Server::new(
        AudioHandle::spawn(|| Box::new(Silent)),
        config.clone(),
        log.clone(),
    );
    server.start().await.unwrap();
    let dir = temp_dir(name);
    let app = Arc::new(SettingsApp::new(
        server.clone(),
        Store::new(&dir),
        config,
        log,
    ));
    let http = SettingsHttp::start(app, 0, tokio::runtime::Handle::current()).unwrap();
    let port = http.port;
    Page {
        http,
        server,
        dir,
        port,
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// A raw HTTP/1.1 request; `host` and `headers` are sent as given so the checks can be provoked.
fn request(
    port: u16,
    method: &str,
    path: &str,
    host: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Response {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut text = format!("{method} {path} HTTP/1.1\r\n");
    if let Some(host) = host {
        text += &format!("Host: {host}\r\n");
    }
    for (k, v) in headers {
        text += &format!("{k}: {v}\r\n");
    }
    text += &format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(text.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Response {
        status,
        headers,
        body: body.to_string(),
    }
}

fn get(p: &Page, path: &str) -> Response {
    request(
        p.port,
        "GET",
        path,
        Some(&format!("127.0.0.1:{}", p.port)),
        &[],
        b"",
    )
}

fn put(p: &Page, body: &Value) -> Response {
    let origin = format!("http://127.0.0.1:{}", p.port);
    request(
        p.port,
        "PUT",
        "/api/config",
        Some(&format!("127.0.0.1:{}", p.port)),
        &[("Origin", &origin), ("Content-Type", "application/json")],
        body.to_string().as_bytes(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_the_page_and_its_script_with_hardening_headers() {
    let p = page("serve").await;
    let index = get(&p, "/");
    assert_eq!(index.status, 200);
    assert!(index
        .header("content-type")
        .unwrap()
        .starts_with("text/html"));
    assert!(index.body.contains("<title>Remote Volume</title>"));
    assert!(index
        .body
        .contains(&format!("Version {}", remote_volume_core::VERSION)));
    assert!(!index.body.contains("{{VERSION}}"));
    assert!(index.body.contains("Developed by JusteReseau"));
    assert!(index
        .body
        .contains(r#"<script src="settings.js"></script>"#));
    let csp = index.header("content-security-policy").unwrap();
    assert!(
        csp.contains("default-src 'none'")
            && csp.contains("script-src 'self'")
            && csp.contains("frame-ancestors 'none'")
    );
    assert_eq!(index.header("x-frame-options"), Some("DENY"));
    assert_eq!(index.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(index.header("cache-control"), Some("no-store"));

    let script = get(&p, "/settings.js");
    assert_eq!(script.status, 200);
    assert!(script
        .header("content-type")
        .unwrap()
        .starts_with("text/javascript"));
    assert!(script.body.contains("/api/config"));
    assert_eq!(get(&p, "/nope").status, 404);
    assert_eq!(
        request(
            p.port,
            "DELETE",
            "/api/config",
            Some(&format!("127.0.0.1:{}", p.port)),
            &[],
            b""
        )
        .status,
        405
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_loopback_host_names_are_answered_to_defeat_dns_rebinding() {
    let p = page("host").await;
    for host in [
        Some("evil.example"),
        Some("evil.example:80"),
        Some("127.0.0.1"),
        Some("127.0.0.1:1"),
        None,
    ] {
        let r = request(p.port, "GET", "/api/config", host, &[], b"");
        assert!(
            r.status == 403 || r.status == 400,
            "{host:?} -> {}",
            r.status
        );
        assert!(!r.body.contains("websocket"), "{host:?} leaked the config");
    }
    for host in [
        format!("localhost:{}", p.port),
        format!("LOCALHOST:{}", p.port),
        format!("[::1]:{}", p.port),
    ] {
        assert_eq!(
            request(p.port, "GET", "/api/config", Some(&host), &[], b"").status,
            200,
            "{host}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn get_config_returns_the_current_settings() {
    let p = page("get").await;
    let r = get(&p, "/api/config");
    assert_eq!(r.status, 200);
    let c = r.json();
    assert_eq!(c["websocket"]["host"], "127.0.0.1");
    assert_eq!(c["websocket"]["token"], "");
    assert_eq!(c["polling"], json!({"enabled": false, "interval": 1000}));
    assert!(c.get("runOnStartup").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_persists_validates_and_applies_the_config() {
    let p = page("save").await;
    let port = p.server.local_addr().unwrap().port();
    let r = put(
        &p,
        &json!({
            "websocket": { "port": port, "host": "127.0.0.1", "token": "  abc123  ", "allowedOrigins": ["http://localhost:8080", "junk"] },
            "polling": { "enabled": true, "interval": 5 },
            "evil": true
        }),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let body = r.json();
    assert_eq!(body["message"], "Configuration saved successfully!");
    assert_eq!(body["config"]["websocket"]["token"], "abc123");
    assert_eq!(
        body["config"]["websocket"]["allowedOrigins"],
        json!(["http://localhost:8080"])
    );
    assert_eq!(
        body["config"]["polling"],
        json!({"enabled": true, "interval": 100})
    );
    assert!(body["config"].get("evil").is_none());

    let on_disk: Value =
        serde_json::from_slice(&fs::read(p.dir.join("config.json")).unwrap()).unwrap();
    assert_eq!(on_disk, body["config"]);
    assert_eq!(get(&p, "/api/config").json(), body["config"]);
    let live = p.server.config();
    assert_eq!(live.websocket.token, "abc123");
    assert!(live.polling.enabled);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_save_that_changes_the_token_applies_it_to_the_running_server() {
    let p = page("token").await;
    let before = p.server.local_addr().unwrap();
    let r = put(
        &p,
        &json!({ "websocket": { "port": before.port(), "token": "newtoken" } }),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // same port: no restart, and the token is now required
    assert_eq!(p.server.local_addr().unwrap(), before);
    let (status, _) = ws_status(before.port(), "/").await;
    assert_eq!(status, 401);
    let (status, _) = ws_status(before.port(), "/?token=newtoken").await;
    assert_eq!(status, 101);
}

async fn ws_status(port: u16, path: &str) -> (u16, ()) {
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Error;
    match connect_async(format!("ws://127.0.0.1:{port}{path}")).await {
        Ok((_, response)) => (response.status().as_u16(), ()),
        Err(Error::Http(response)) => (response.status().as_u16(), ()),
        Err(other) => panic!("{other}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_save_cannot_bind_reports_the_error_but_keeps_the_config() {
    let p = page("busy").await;
    // Occupy a port, then ask the server to move there.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let r = put(&p, &json!({ "websocket": { "port": port } }));
    assert_eq!(r.status, 500);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("could not listen"),
        "{}",
        r.body
    );
    let on_disk: Value =
        serde_json::from_slice(&fs::read(p.dir.join("config.json")).unwrap()).unwrap();
    assert_eq!(on_disk["websocket"]["port"], port);
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_is_refused_without_the_pages_own_origin_and_a_json_content_type() {
    let p = page("csrf").await;
    let host = format!("127.0.0.1:{}", p.port);
    let own = format!("http://{host}");
    let body = br#"{"websocket":{"token":"hijacked"}}"#;
    let attempt = |headers: &[(&str, &str)]| {
        request(p.port, "PUT", "/api/config", Some(&host), headers, body)
    };

    // a page on another site
    assert_eq!(
        attempt(&[
            ("Origin", "https://evil.example"),
            ("Content-Type", "application/json")
        ])
        .status,
        403
    );
    // no Origin at all
    assert_eq!(attempt(&[("Content-Type", "application/json")]).status, 403);
    // the right host over https, or another port, is not this page
    assert_eq!(
        attempt(&[
            ("Origin", &format!("https://{host}")),
            ("Content-Type", "application/json")
        ])
        .status,
        403
    );
    assert_eq!(
        attempt(&[
            ("Origin", "http://127.0.0.1:1"),
            ("Content-Type", "application/json")
        ])
        .status,
        403
    );
    // a form post (no preflight needed) must not be accepted as JSON
    assert_eq!(
        attempt(&[
            ("Origin", &own),
            ("Content-Type", "application/x-www-form-urlencoded")
        ])
        .status,
        415
    );
    assert_eq!(
        attempt(&[("Origin", &own), ("Content-Type", "text/plain")]).status,
        415
    );
    assert_eq!(attempt(&[("Origin", &own)]).status, 415);
    assert_eq!(p.server.config().websocket.token, "", "nothing was applied");
    assert!(!p.dir.join("config.json").exists(), "nothing was written");
    // and the genuine request works
    assert_eq!(
        attempt(&[
            ("Origin", &own),
            ("Content-Type", "application/json; charset=utf-8")
        ])
        .status,
        200
    );
    assert_eq!(p.server.config().websocket.token, "hijacked");
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_bodies_are_rejected() {
    let p = page("body").await;
    let host = format!("127.0.0.1:{}", p.port);
    let origin = format!("http://{host}");
    let headers = [
        ("Origin", origin.as_str()),
        ("Content-Type", "application/json"),
    ];
    assert_eq!(
        request(
            p.port,
            "PUT",
            "/api/config",
            Some(&host),
            &headers,
            b"{ nope"
        )
        .status,
        400
    );
    let huge = vec![b' '; 9000];
    assert_eq!(
        request(p.port, "PUT", "/api/config", Some(&host), &headers, &huge).status,
        413
    );
    // valid JSON that is not an object changes nothing
    let before = get(&p, "/api/config").json();
    assert_eq!(
        request(
            p.port,
            "PUT",
            "/api/config",
            Some(&host),
            &headers,
            b"[1,2]"
        )
        .status,
        200
    );
    assert_eq!(get(&p, "/api/config").json(), before);
}
