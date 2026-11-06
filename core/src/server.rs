//! WebSocket server: admission control, command dispatch, state polling and broadcast.
//! Port of `lib/server.js`; the protocol rules live in [`crate::wsproto`].
//!
//! Every client is one task. State changes are pushed to all clients through one broadcast channel,
//! so a slow client delays only itself.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch, Notify};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout, MissedTickBehavior};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{http, Error as WsError, Message as WsMessage};
use tokio_tungstenite::{accept_hdr_async_with_config, WebSocketStream};

use crate::apps;
use crate::audio::AudioHandle;
use crate::backend::{AudioError, ErrorCode, Features, MediaKey};
use crate::config::Config;
use crate::log::Log;
use crate::wsproto::{
    admit_client, capabilities_json, error_reply, errors, has_capability, parse_message,
    same_value, ActionSpec, Handshake, Message, RateLimiter, Refusal, TargetKind, APP_ACTIONS,
    BUILTIN_ACTIONS, KEYBOARD_ACTIONS, MAX_PAYLOAD_BYTES, MEDIA_ACTIONS,
};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Why a command did not succeed.
#[derive(Debug)]
pub enum CommandError {
    /// The named device or app does not exist (any more): `Device not found` / `App not found`.
    NotFound(TargetKind),
    /// Anything else; logged, the client sees `Failed to execute action`.
    Failed(String),
}

pub type Command = Arc<dyn Fn(Message) -> BoxFuture<Result<Value, CommandError>> + Send + Sync>;
pub type StateProvider = Arc<dyn Fn() -> BoxFuture<Result<Value, String>> + Send + Sync>;

/// Extension points for more protocol v2 features (device selection): extra actions with their
/// handlers, and extra state fields. Empty by default, which is protocol v1, `getCapabilities` and
/// the per-app actions.
#[derive(Default, Clone)]
pub struct Extensions {
    pub actions: Vec<ActionSpec>,
    pub commands: HashMap<&'static str, Command>,
    /// Extra fields of the state (e.g. list-valued `outputDevices`, `apps`): read on every refresh,
    /// in `getState` and in the initial state, and broadcast when their content changes.
    pub state: Vec<(&'static str, StateProvider)>,
    /// Per-connection message limit as (burst, refill per second); default 20 and 20.
    pub rate_limit: Option<(u32, f64)>,
}

/// What [`Server::apply_config`] had to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyResult {
    pub restarted: bool,
    pub disconnected: usize,
    pub polling_restarted: bool,
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait after an OS change notification before reading the state: one change (an app
/// starting) often comes as a burst of notifications, and they are read in one pass.
const CHANGE_SETTLE: Duration = Duration::from_millis(50);

#[derive(Debug)]
enum Kick {
    /// Close handshake with this code and reason.
    Close(CloseCode, &'static str),
    /// Drop the connection.
    Terminate,
}

struct ClientEntry {
    kick: mpsc::UnboundedSender<Kick>,
    handshake: Handshake,
}

struct Outgoing {
    /// Position in the order broadcasts were made; a newcomer skips what its snapshot already covers.
    seq: u64,
    text: String,
    /// The client that already knows (the one that just connected).
    exclude: Option<u64>,
}

struct Running {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
    accept: JoinHandle<()>,
}

struct Inner {
    audio: AudioHandle,
    log: Arc<dyn Log>,
    config: RwLock<Config>,
    actions: Vec<ActionSpec>,
    ext: Extensions,
    state_keys: Vec<&'static str>,
    /// What clients were last told, per state field.
    last: Mutex<HashMap<&'static str, Value>>,
    clients: Mutex<HashMap<u64, ClientEntry>>,
    next_client: AtomicU64,
    /// Admitted connections, counted from the handshake on, for the client limit.
    slots: Arc<AtomicUsize>,
    events: broadcast::Sender<Arc<Outgoing>>,
    /// Number of broadcasts made so far.
    seq: AtomicU64,
    refresh_lock: tokio::sync::Mutex<()>,
    refresh_again: AtomicBool,
    running: Mutex<Option<Running>>,
    polling: Mutex<Option<JoinHandle<()>>>,
    /// Signalled by the OS backend's change watcher (see [`Server::change_notifier`]).
    changes: Arc<Notify>,
    change_task: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

impl Server {
    pub fn new(audio: AudioHandle, config: Config, log: Arc<dyn Log>) -> Server {
        Self::with_extensions(audio, config, log, Extensions::default())
    }

    pub fn with_extensions(
        audio: AudioHandle,
        config: Config,
        log: Arc<dyn Log>,
        ext: Extensions,
    ) -> Server {
        let mut actions = BUILTIN_ACTIONS.to_vec();
        actions.extend(APP_ACTIONS);
        actions.extend(MEDIA_ACTIONS);
        actions.extend(KEYBOARD_ACTIONS);
        actions.extend(ext.actions.iter().copied());
        let mut state_keys = vec!["volume", "muted", "outputDevice", "apps"];
        state_keys.extend(ext.state.iter().map(|(key, _)| *key));
        let (events, _) = broadcast::channel(256);
        Server {
            inner: Arc::new(Inner {
                audio,
                log,
                config: RwLock::new(config),
                actions,
                ext,
                state_keys,
                last: Mutex::new(HashMap::new()),
                clients: Mutex::new(HashMap::new()),
                next_client: AtomicU64::new(1),
                slots: Arc::new(AtomicUsize::new(0)),
                events,
                seq: AtomicU64::new(0),
                refresh_lock: tokio::sync::Mutex::new(()),
                refresh_again: AtomicBool::new(false),
                running: Mutex::new(None),
                polling: Mutex::new(None),
                changes: Arc::new(Notify::new()),
                change_task: Mutex::new(None),
            }),
        }
    }

    pub fn config(&self) -> Config {
        self.inner.config.read().unwrap().clone()
    }

    /// The bound address while listening.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.inner.running.lock().unwrap().as_ref().map(|r| r.addr)
    }

    pub fn client_count(&self) -> usize {
        self.inner.clients.lock().unwrap().len()
    }

    /// Start listening with the current config. Resolves with the bound address.
    pub async fn start(&self) -> io::Result<SocketAddr> {
        let inner = &self.inner;
        let ws = inner.config.read().unwrap().websocket.clone();
        let listener = TcpListener::bind((ws.host.as_str(), ws.port)).await?;
        let addr = listener.local_addr()?;
        let (stop, stop_rx) = watch::channel(false);
        let accept = tokio::spawn(accept_loop(inner.clone(), listener, stop_rx));
        *inner.running.lock().unwrap() = Some(Running { addr, stop, accept });
        inner.log.info(&format!(
            "WebSocket server listening on ws://{addr}{}",
            if ws.token.is_empty() {
                ""
            } else {
                " (token required)"
            }
        ));
        self.start_polling();
        self.start_change_task();
        Ok(addr)
    }

    /// Stop listening and disconnect every client.
    pub async fn stop(&self) {
        self.stop_polling();
        if let Some(task) = self.inner.change_task.lock().unwrap().take() {
            task.abort();
        }
        let running = self.inner.running.lock().unwrap().take();
        if let Some(running) = running {
            let _ = running.stop.send(true);
            // Wait until the listener is closed so the port can be bound again right away.
            let _ = running.accept.await;
        }
        let clients: Vec<_> = self.inner.clients.lock().unwrap().drain().collect();
        for (_, client) in clients {
            let _ = client.kick.send(Kick::Terminate);
        }
    }

    /// Stop and start again (new port / bind address). Disconnects every client.
    pub async fn restart(&self) -> io::Result<SocketAddr> {
        self.inner
            .log
            .info("Restarting WebSocket server to apply new configuration.");
        self.stop().await;
        self.inner.last.lock().unwrap().clear();
        self.start().await
    }

    /// Apply a new config after a settings save, touching only what changed:
    /// - port / bind address: restart the listener (unavoidable)
    /// - token / allowed origins: keep listening; disconnect only clients that fail the new rules
    /// - polling: restart the polling task only
    /// - anything else: nothing
    pub async fn apply_config(&self, next: Config) -> io::Result<ApplyResult> {
        let inner = &self.inner;
        let previous = std::mem::replace(&mut *inner.config.write().unwrap(), next.clone());
        let listening = inner.running.lock().unwrap().is_some();
        let (pw, nw) = (&previous.websocket, &next.websocket);
        if !listening || pw.port != nw.port || pw.host != nw.host {
            self.restart().await?;
            return Ok(ApplyResult {
                restarted: true,
                disconnected: 0,
                polling_restarted: true,
            });
        }

        let mut disconnected = 0;
        if pw.token != nw.token || pw.allowed_origins != nw.allowed_origins {
            for client in inner.clients.lock().unwrap().values() {
                if admit_client(&client.handshake, nw, 0).is_ok() {
                    continue;
                }
                inner.log.info(&format!(
                    "Disconnecting {}: no longer allowed by the access settings",
                    client.handshake.remote
                ));
                let _ = client
                    .kick
                    .send(Kick::Close(CloseCode::Policy, "Access settings changed"));
                disconnected += 1;
            }
        }

        let polling_restarted = previous.polling != next.polling;
        if polling_restarted {
            self.start_polling();
        }
        if previous.apps != next.apps {
            // Clients see the `apps` list appear, or get `"apps": null` when tracking is turned off.
            self.refresh().await;
        }
        inner.log.info(&format!(
            "Settings applied without restart (disconnected {disconnected}, polling {}).",
            if polling_restarted {
                "updated"
            } else {
                "unchanged"
            }
        ));
        Ok(ApplyResult {
            restarted: false,
            disconnected,
            polling_restarted,
        })
    }

    /// Read the OS state and broadcast what changed. Calls made while a refresh is running are
    /// coalesced into exactly one extra pass, so the latest change is never missed and a burst of
    /// commands does not become a burst of OS calls. Returns when the state is up to date.
    pub async fn refresh(&self) {
        self.inner.refresh().await;
    }

    /// A function for the OS backend's change watcher (see [`crate::backend::watch`]) to call,
    /// from any thread, whenever something may have changed outside this app. While the server
    /// runs, each call (or burst of calls) leads to one refresh, so external changes reach clients
    /// without polling.
    pub fn change_notifier(&self) -> impl Fn() + Send + Sync + 'static {
        let changes = self.inner.changes.clone();
        move || changes.notify_one()
    }

    fn start_change_task(&self) {
        let inner = self.inner.clone();
        let task = tokio::spawn(async move {
            loop {
                inner.changes.notified().await;
                sleep(CHANGE_SETTLE).await;
                inner.refresh().await;
            }
        });
        if let Some(previous) = self.inner.change_task.lock().unwrap().replace(task) {
            previous.abort();
        }
    }

    fn start_polling(&self) {
        self.stop_polling();
        let polling = self.inner.config.read().unwrap().polling;
        if !polling.enabled {
            return;
        }
        self.inner.log.info(&format!(
            "Starting volume/mute state monitoring every {} ms.",
            polling.interval
        ));
        let inner = self.inner.clone();
        let task = tokio::spawn(async move {
            let mut tick = interval(Duration::from_millis(u64::from(polling.interval)));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            tick.tick().await; // the first tick is immediate; wait a full interval like setInterval
            loop {
                tick.tick().await;
                // Skip a tick instead of piling up work when the OS is slow.
                if inner.refresh_lock.try_lock().is_err() {
                    continue;
                }
                inner.refresh().await;
            }
        });
        *self.inner.polling.lock().unwrap() = Some(task);
    }

    fn stop_polling(&self) {
        if let Some(task) = self.inner.polling.lock().unwrap().take() {
            task.abort();
        }
    }
}

impl Inner {
    async fn read_state(&self) -> Result<Map<String, Value>, String> {
        let audio = self.audio.read_state().await.map_err(|e| e.to_string())?;
        let mut state = Map::new();
        state.insert("volume".into(), json!(audio.volume));
        state.insert("muted".into(), json!(audio.muted));
        state.insert("outputDevice".into(), json!(audio.output_device));
        // `apps` is left out where per-app audio is unsupported or turned off, and when listing
        // fails: volume and mute must still get through. Clients that saw a list then get
        // `"apps": null`.
        if self.config.read().unwrap().apps.enabled {
            if let Ok(list) = self.audio.list_apps().await {
                state.insert("apps".into(), apps::to_json(&list));
            }
        }
        for (key, provider) in &self.ext.state {
            state.insert((*key).into(), provider().await?);
        }
        Ok(state)
    }

    /// What the backend supports right now, minus what the configuration turns off.
    async fn features(&self) -> Result<Features, String> {
        let mut features = self.audio.features().await.map_err(|e| e.to_string())?;
        if !self.config.read().unwrap().apps.enabled {
            features.apps_volume = false;
            features.apps_mute = false;
        }
        Ok(features)
    }

    async fn capabilities(&self) -> Result<Value, String> {
        Ok(capabilities_json(&self.features().await?))
    }

    fn broadcast(&self, message: &Value, exclude: Option<u64>) {
        let text = message.to_string();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        // No receivers just means no clients.
        let _ = self.events.send(Arc::new(Outgoing { seq, text, exclude }));
    }

    /// Broadcast each field whose content differs from what clients were last told, one message
    /// per field. Scalars compare strictly; lists and objects structurally.
    fn broadcast_changes(&self, state: &Map<String, Value>, exclude: Option<u64>) {
        let mut last = self.last.lock().unwrap();
        for key in &self.state_keys {
            let value = state.get(*key).unwrap_or(&Value::Null);
            let before = last.get(key).unwrap_or(&Value::Null);
            if !same_value(value, before) {
                last.insert(key, value.clone());
                self.broadcast(&json!({ *key: value }), exclude);
            }
        }
    }

    async fn refresh(&self) {
        self.refresh_again.store(true, Ordering::SeqCst);
        let _running = self.refresh_lock.lock().await;
        while self.refresh_again.swap(false, Ordering::SeqCst) {
            match self.read_state().await {
                Ok(state) => self.broadcast_changes(&state, None),
                Err(error) => self
                    .log
                    .error(&format!("Error reading volume/mute state: {error}")),
            }
        }
    }

    /// Full state for a newcomer, plus `capabilities`, and the number of broadcasts it already
    /// covers (the newcomer must skip those). Whatever changed since the last broadcast also goes to
    /// the other clients, so everyone stays in sync even with polling disabled.
    ///
    /// Reading and broadcasting happen under the refresh lock, like in `refresh`: two overlapping
    /// read-then-broadcast cycles could otherwise publish an older reading after a newer one.
    async fn initial_state(&self, client: u64) -> Option<(Value, u64)> {
        let guard = self.refresh_lock.lock().await;
        let mut state = match self.read_state().await {
            Ok(state) => state,
            Err(error) => {
                self.log
                    .error(&format!("Failed to send initial state: {error}"));
                return None;
            }
        };
        self.broadcast_changes(&state, Some(client));
        let covered = self.seq.load(Ordering::SeqCst);
        drop(guard);
        // If the capability probe fails the client can still ask with getCapabilities, so it
        // never costs the client its state.
        match self.capabilities().await {
            Ok(caps) => {
                state.insert("capabilities".into(), caps);
            }
            Err(error) => self
                .log
                .warn(&format!("Failed to read capabilities: {error}")),
        }
        Some((Value::Object(state), covered))
    }

    /// Run one validated message. `Err` is an unexpected failure (logged; the client sees
    /// `Failed to execute action`).
    async fn execute(&self, msg: &Message, spec: &ActionSpec) -> Result<Outcome, String> {
        // Ask before acting: an action the machine cannot do never reaches the controller.
        if let Some(path) = spec.requires {
            if !has_capability(&self.features().await?, path) {
                return Ok(Outcome::Refused(errors::NOT_SUPPORTED));
            }
        }
        let failed = |e: crate::backend::AudioError| e.to_string();
        let value = msg.value.map(|v| v as u8).unwrap_or(0);
        if APP_ACTIONS.iter().any(|a| a.name == msg.action) {
            return self.execute_app(msg, value).await;
        }
        let media = match msg.action {
            "mediaPlayPause" => Some(MediaKey::PlayPause),
            "mediaNextTrack" => Some(MediaKey::Next),
            "mediaPreviousTrack" => Some(MediaKey::Previous),
            _ => None,
        };
        if let Some(key) = media {
            return match self.audio.press_media_key(key).await {
                Ok(()) => Ok(Outcome::Done(Value::Null)),
                Err(error) if error.code == ErrorCode::NotFound => {
                    self.log
                        .warn(&format!("{}: {}", msg.action, errors::NO_MEDIA_PLAYER));
                    Ok(Outcome::Refused(errors::NO_MEDIA_PLAYER))
                }
                Err(error) => Err(error.to_string()),
            };
        }
        if msg.action == "pressFunctionKey" {
            return self
                .audio
                .press_function_key(value)
                .await
                .map(|()| Outcome::Done(Value::Null))
                .map_err(failed);
        }
        let result = match msg.action {
            "setVolume" => self
                .audio
                .set_volume(value)
                .await
                .map(|_| Value::Null)
                .map_err(failed),
            "increaseVolume" => self
                .audio
                .increase_volume(value)
                .await
                .map(|_| Value::Null)
                .map_err(failed),
            "decreaseVolume" => self
                .audio
                .decrease_volume(value)
                .await
                .map(|_| Value::Null)
                .map_err(failed),
            "mute" => self
                .audio
                .set_muted(true)
                .await
                .map(|_| Value::Null)
                .map_err(failed),
            "unmute" => self
                .audio
                .set_muted(false)
                .await
                .map(|_| Value::Null)
                .map_err(failed),
            "toggleMute" => self
                .audio
                .toggle_mute()
                .await
                .map(|_| Value::Null)
                .map_err(failed),
            "isMuted" => self
                .audio
                .is_muted()
                .await
                .map(|m| json!({ "muted": m }))
                .map_err(failed),
            "getState" => self.read_state().await.map(Value::Object),
            "getCapabilities" => self
                .capabilities()
                .await
                .map(|c| json!({ "capabilities": c })),
            other => {
                let Some(command) = self.ext.commands.get(other) else {
                    return Err(format!("no handler for {other}"));
                };
                return match command(msg.clone()).await {
                    Ok(value) => Ok(Outcome::Done(value)),
                    Err(CommandError::NotFound(kind)) => {
                        self.log.warn(&format!("{other}: {}", kind.not_found()));
                        Ok(Outcome::Refused(kind.not_found()))
                    }
                    Err(CommandError::Failed(e)) => Err(e),
                };
            }
        };
        result.map(Outcome::Done)
    }

    /// The per-app actions. The backend resolves the target and acts in one job, so the app cannot
    /// vanish in between; a target that matches nothing is `App not found`.
    async fn execute_app(&self, msg: &Message, value: u8) -> Result<Outcome, String> {
        let app = || msg.app.clone().unwrap_or_default();
        let audio = &self.audio;
        let result = match msg.action {
            "getApps" => {
                return match audio.list_apps().await {
                    Ok(list) => Ok(Outcome::Done(json!({ "apps": apps::to_json(&list) }))),
                    Err(error) => self.app_failure(msg.action, error),
                }
            }
            "setAppVolume" => audio.set_app_volume(app(), value).await,
            "increaseAppVolume" => audio.change_app_volume(app(), i16::from(value)).await,
            "decreaseAppVolume" => audio.change_app_volume(app(), -i16::from(value)).await,
            "muteApp" => audio.set_app_muted(app(), true).await,
            "unmuteApp" => audio.set_app_muted(app(), false).await,
            "toggleAppMute" => audio.toggle_app_mute(app()).await,
            other => return Err(format!("no handler for {other}")),
        };
        match result {
            Ok(()) => Ok(Outcome::Done(Value::Null)),
            Err(error) => self.app_failure(msg.action, error),
        }
    }

    fn app_failure(&self, action: &str, error: AudioError) -> Result<Outcome, String> {
        match error.code {
            ErrorCode::NotFound => {
                self.log
                    .warn(&format!("{action}: {}", errors::APP_NOT_FOUND));
                Ok(Outcome::Refused(errors::APP_NOT_FOUND))
            }
            // The capability said yes but the OS said no (e.g. the Pulse server just went away).
            ErrorCode::Unsupported => Ok(Outcome::Refused(errors::NOT_SUPPORTED)),
            _ => Err(error.to_string()),
        }
    }
}

enum Outcome {
    /// The command ran; the value is its reply (sent only for actions with \`reply\`).
    Done(Value),
    /// The command was refused with a protocol error text: "Not supported", "App not found", ...
    Refused(&'static str),
}

async fn accept_loop(inner: Arc<Inner>, listener: TcpListener, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => { tokio::spawn(handle_connection(inner.clone(), stream, peer)); }
                Err(error) => {
                    inner.log.warn(&format!("Accepting a connection failed: {error}"));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
}

/// Counts one admitted connection against `MAX_CLIENTS` from the handshake until it ends.
#[derive(Clone)]
struct Slot {
    count: Arc<AtomicUsize>,
    held: Arc<AtomicBool>,
}

impl Slot {
    /// Take a slot; returns how many were taken before.
    fn reserve(&self) -> usize {
        self.held.store(true, Ordering::SeqCst);
        self.count.fetch_add(1, Ordering::SeqCst)
    }

    fn release(&self) {
        if self.held.swap(false, Ordering::SeqCst) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

struct SlotGuard(Slot);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn header(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .map(|v| v.to_str().unwrap_or("\u{fffd}not-utf8").to_string())
}

fn refusal_response(refusal: Refusal) -> ErrorResponse {
    http::Response::builder()
        .status(refusal.code)
        .body(Some(refusal.reason.to_string()))
        .expect("static status and body")
}

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_PAYLOAD_BYTES))
        .max_frame_size(Some(MAX_PAYLOAD_BYTES))
}

#[allow(clippy::result_large_err)] // the handshake callback must return tungstenite's own error type
async fn handle_connection(inner: Arc<Inner>, stream: TcpStream, peer: SocketAddr) {
    let slot = Slot {
        count: inner.slots.clone(),
        held: Arc::new(AtomicBool::new(false)),
    };
    let _guard = SlotGuard(slot.clone());
    let presented: Arc<Mutex<Option<Handshake>>> = Arc::new(Mutex::new(None));

    let callback = {
        let (inner, slot, presented) = (inner.clone(), slot.clone(), presented.clone());
        move |req: &Request, response: Response| -> Result<Response, ErrorResponse> {
            let handshake = Handshake {
                url: req.uri().to_string(),
                origin: header(req, "origin"),
                authorization: header(req, "authorization"),
                remote: peer.to_string(),
            };
            let ws = inner.config.read().unwrap().websocket.clone();
            match admit_client(&handshake, &ws, slot.reserve()) {
                Ok(()) => {
                    *presented.lock().unwrap() = Some(handshake);
                    Ok(response)
                }
                Err(refusal) => {
                    slot.release();
                    inner.log.warn(&format!(
                        "Rejected WebSocket client {peer}: {}",
                        refusal.reason
                    ));
                    Err(refusal_response(refusal))
                }
            }
        }
    };

    let ws = match timeout(
        HANDSHAKE_TIMEOUT,
        accept_hdr_async_with_config(stream, callback, Some(ws_config())),
    )
    .await
    {
        Ok(Ok(ws)) => ws,
        Ok(Err(_)) | Err(_) => return, // refused, malformed or too slow
    };
    let Some(handshake) = presented.lock().unwrap().take() else {
        return;
    };

    let id = inner.next_client.fetch_add(1, Ordering::SeqCst);
    let (kick, kicks) = mpsc::unbounded_channel();
    inner
        .clients
        .lock()
        .unwrap()
        .insert(id, ClientEntry { kick, handshake });
    inner.log.info(&format!("Client connected from {peer}"));
    client_loop(&inner, id, ws, kicks).await;
    inner.clients.lock().unwrap().remove(&id);
}

async fn send_json(ws: &mut WebSocketStream<TcpStream>, value: &Value) -> Result<(), WsError> {
    ws.send(WsMessage::text(value.to_string())).await
}

async fn client_loop(
    inner: &Arc<Inner>,
    id: u64,
    mut ws: WebSocketStream<TcpStream>,
    mut kicks: mpsc::UnboundedReceiver<Kick>,
) {
    let mut events = inner.events.subscribe();
    let mut limiter = match inner.ext.rate_limit {
        Some((burst, per_second)) => RateLimiter::new(burst, per_second),
        None => RateLimiter::default(),
    };

    let mut covered = 0;
    if let Some((state, seq)) = inner.initial_state(id).await {
        covered = seq;
        if send_json(&mut ws, &state).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            incoming = ws.next() => match incoming {
                None | Some(Ok(WsMessage::Close(_))) => break,
                Some(Err(WsError::Capacity(_))) => {
                    // Frames over the payload limit close the connection with 1009.
                    let frame = CloseFrame { code: CloseCode::Size, reason: "".into() };
                    let _ = timeout(Duration::from_secs(1), ws.close(Some(frame))).await;
                    break;
                }
                Some(Err(error)) => {
                    inner.log.warn(&format!("WebSocket client error: {error}"));
                    break;
                }
                // Binary frames are ignored; ping/pong are answered by the library.
                Some(Ok(WsMessage::Text(text))) => {
                    if handle_text(inner, &mut limiter, text.as_bytes(), &mut ws).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {}
            },
            outgoing = events.recv() => match outgoing {
                Ok(out) => {
                    if out.seq > covered && out.exclude != Some(id) && ws.send(WsMessage::text(out.text.clone())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            kick = kicks.recv() => match kick {
                Some(Kick::Close(code, reason)) => {
                    let frame = CloseFrame { code, reason: reason.into() };
                    let _ = timeout(Duration::from_secs(1), ws.close(Some(frame))).await;
                    break;
                }
                Some(Kick::Terminate) | None => break,
            },
        }
    }
}

/// Handle one text message. `Err` means the connection is gone.
async fn handle_text(
    inner: &Arc<Inner>,
    limiter: &mut RateLimiter,
    raw: &[u8],
    ws: &mut WebSocketStream<TcpStream>,
) -> Result<(), WsError> {
    if !limiter.try_remove() {
        return send_json(ws, &json!({ "error": errors::RATE_LIMITED })).await;
    }
    let msg = match parse_message(raw, &inner.actions) {
        Ok(msg) => msg,
        Err(rejection) => {
            inner
                .log
                .warn(&format!("Rejected client message: {}", rejection.error));
            return send_json(
                ws,
                &error_reply(rejection.action.as_deref(), &rejection.error),
            )
            .await;
        }
    };
    let spec = *inner
        .actions
        .iter()
        .find(|a| a.name == msg.action)
        .expect("parse_message only returns known actions");

    match inner.execute(&msg, &spec).await {
        Ok(Outcome::Refused(error)) => send_json(ws, &error_reply(Some(msg.action), error)).await,
        Ok(Outcome::Done(response)) => {
            if spec.reply {
                send_json(ws, &response).await?;
            }
            // Push the resulting state to every client right away, whether or not polling is enabled.
            if spec.mutates {
                let inner = inner.clone();
                tokio::spawn(async move { inner.refresh().await });
            }
            Ok(())
        }
        Err(error) => {
            inner
                .log
                .error(&format!("Error executing {}: {error}", msg.action));
            send_json(ws, &error_reply(Some(msg.action), errors::EXECUTION_FAILED)).await
        }
    }
}
