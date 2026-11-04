//! The settings page: a small HTTP server on `127.0.0.1` that serves one page and a JSON API to
//! read and save the configuration. It replaces the Electron settings window.
//!
//! Anyone who can open this page can change the access token, so:
//! - it listens on the loopback interface only, on its own port (never the WebSocket port);
//! - the `Host` header must name the loopback address and this port (defeats DNS rebinding);
//! - saving needs `Content-Type: application/json` and an `Origin` that is this very page (defeats
//!   cross-site requests: a page on another origin cannot send a JSON PUT without a CORS preflight,
//!   which this server never approves);
//! - the page may load nothing but its own script and may not be framed.

use std::io::{self, Read};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::{self, JoinHandle};

use serde_json::{json, Value};
use tokio::runtime::Handle;
use tokio::sync::Mutex;

use crate::config::{sanitize, Config, Store};
use crate::log::Log;
use crate::server::Server;

const PAGE: &str = include_str!("../assets/settings.html");
const SCRIPT: &str = include_str!("../assets/settings.js");
/// A config is a few hundred bytes; anything bigger is not from our page.
const MAX_BODY_BYTES: usize = 8 * 1024;

const CSP: &str =
    "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; \
                   base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The configuration and what saving it does: persist, then apply to the running server.
pub struct SettingsApp {
    server: Server,
    store: Store,
    config: Mutex<Config>,
    log: Arc<dyn Log>,
}

impl SettingsApp {
    pub fn new(server: Server, store: Store, config: Config, log: Arc<dyn Log>) -> SettingsApp {
        SettingsApp {
            server,
            store,
            config: Mutex::new(config),
            log,
        }
    }

    pub async fn config(&self) -> Config {
        self.config.lock().await.clone()
    }

    /// Validate `input` against the current config, save it and apply it. Returns the config in
    /// effect and a message for the user. Saves are serialised.
    pub async fn save(&self, input: &Value) -> Result<(Config, String), String> {
        let mut current = self.config.lock().await;
        let next = sanitize(input, &current);
        self.store
            .save(&next)
            .map_err(|e| format!("Could not write {}: {e}", self.store.path().display()))?;
        *current = next.clone();
        match self.server.apply_config(next.clone()).await {
            Ok(_) => Ok((next, "Configuration saved successfully!".into())),
            Err(e) => {
                self.log
                    .error(&format!("Failed to apply the new configuration: {e}"));
                Err(format!(
                    "Saved, but the WebSocket server could not listen on {}:{}: {e}",
                    next.websocket.host, next.websocket.port
                ))
            }
        }
    }
}

/// An HTTP request reduced to what the routes look at.
#[derive(Debug, Default, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub host: Option<String>,
    pub origin: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// The body was larger than [`MAX_BODY_BYTES`] and was not read.
    pub too_large: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resp {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

fn text(status: u16, message: &str) -> Resp {
    Resp {
        status,
        content_type: "text/plain; charset=utf-8",
        body: message.to_string(),
    }
}

fn json_resp(status: u16, value: &Value) -> Resp {
    Resp {
        status,
        content_type: "application/json",
        body: value.to_string(),
    }
}

/// The names this page may be reached under, for `Host` and `Origin` checks.
fn allowed_hosts(port: u16) -> [String; 3] {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

pub async fn handle_request(app: &SettingsApp, req: &Req, port: u16) -> Resp {
    let hosts = allowed_hosts(port);
    if !req
        .host
        .as_deref()
        .is_some_and(|h| hosts.iter().any(|a| a.eq_ignore_ascii_case(h)))
    {
        return text(403, "Forbidden: unexpected Host header");
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET" | "HEAD", "/" | "/index.html") => Resp {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: PAGE.to_string(),
        },
        ("GET" | "HEAD", "/settings.js") => Resp {
            status: 200,
            content_type: "text/javascript; charset=utf-8",
            body: SCRIPT.to_string(),
        },
        ("GET" | "HEAD", "/api/config") => json_resp(200, &app.config().await.to_json()),
        ("PUT", "/api/config") => {
            // Browsers send Origin on every PUT; a missing or foreign one is not our page.
            let origin_ok = req
                .origin
                .as_deref()
                .and_then(|o| o.strip_prefix("http://"))
                .is_some_and(|o| hosts.iter().any(|a| a.eq_ignore_ascii_case(o)));
            if !origin_ok {
                return text(403, "Forbidden: unexpected Origin header");
            }
            let json_type = req.content_type.as_deref().is_some_and(|c| {
                c.trim()
                    .to_ascii_lowercase()
                    .starts_with("application/json")
            });
            if !json_type {
                return text(415, "Content-Type must be application/json");
            }
            if req.too_large {
                return text(413, "Request body too large");
            }
            let Ok(input) = serde_json::from_slice::<Value>(&req.body) else {
                return json_resp(400, &json!({ "error": "The body is not valid JSON" }));
            };
            match app.save(&input).await {
                Ok((config, message)) => json_resp(
                    200,
                    &json!({ "config": config.to_json(), "message": message }),
                ),
                Err(error) => json_resp(500, &json!({ "error": error })),
            }
        }
        (_, "/" | "/index.html" | "/settings.js" | "/api/config") => {
            text(405, "Method not allowed")
        }
        _ => text(404, "Not found"),
    }
}

/// The running HTTP listener.
pub struct SettingsHttp {
    pub port: u16,
    http: Arc<tiny_http::Server>,
    thread: StdMutex<Option<JoinHandle<()>>>,
}

impl SettingsHttp {
    /// Listen on `127.0.0.1:port` (0 = any free port). Requests are handled on a dedicated thread
    /// that runs the async handlers on `runtime`.
    pub fn start(app: Arc<SettingsApp>, port: u16, runtime: Handle) -> io::Result<SettingsHttp> {
        let http = tiny_http::Server::http(("127.0.0.1", port))
            .map_err(|e| io::Error::other(e.to_string()))?;
        let port = http
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .ok_or_else(|| io::Error::other("settings server has no IP address"))?;
        let http = Arc::new(http);
        let worker = http.clone();
        let thread = thread::Builder::new()
            .name("settings-http".into())
            .spawn(move || {
                for mut request in worker.incoming_requests() {
                    let req = read_request(&mut request);
                    let resp = runtime.block_on(handle_request(&app, &req, port));
                    let _ = request.respond(to_http(resp, req.method == "HEAD"));
                }
            })?;
        Ok(SettingsHttp {
            port,
            http,
            thread: StdMutex::new(Some(thread)),
        })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    pub fn stop(&self) {
        self.http.unblock();
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

fn header_value(request: &tiny_http::Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn read_request(request: &mut tiny_http::Request) -> Req {
    let path = request.url().split('?').next().unwrap_or("/").to_string();
    let too_large = request.body_length().is_some_and(|n| n > MAX_BODY_BYTES);
    let mut body = Vec::new();
    if !too_large {
        // Bounded even when the client lies about (or omits) Content-Length.
        let _ = request
            .as_reader()
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut body);
    }
    let too_large = too_large || body.len() > MAX_BODY_BYTES;
    Req {
        method: request.method().as_str().to_string(),
        path,
        host: header_value(request, "Host"),
        origin: header_value(request, "Origin"),
        content_type: header_value(request, "Content-Type"),
        body,
        too_large,
    }
}

fn to_http(resp: Resp, head_only: bool) -> tiny_http::Response<io::Cursor<Vec<u8>>> {
    let header = |name: &str, value: &str| {
        tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    };
    let body = if head_only {
        Vec::new()
    } else {
        resp.body.into_bytes()
    };
    tiny_http::Response::from_data(body)
        .with_status_code(resp.status)
        .with_header(header("Content-Type", resp.content_type))
        .with_header(header("Content-Security-Policy", CSP))
        .with_header(header("X-Content-Type-Options", "nosniff"))
        .with_header(header("X-Frame-Options", "DENY"))
        .with_header(header("Referrer-Policy", "no-referrer"))
        .with_header(header("Cache-Control", "no-store"))
}
