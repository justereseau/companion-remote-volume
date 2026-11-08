//! End-to-end tests of the WebSocket server with a fake audio backend (no sound card needed): a real
//! listener on a free port, real WebSocket clients. Port of `test/server.test.js`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use remote_volume_core::audio::AudioHandle;
use remote_volume_core::backend::{
    AppSession, AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode, Features,
    MediaKey,
};
use remote_volume_core::config::{Config, Polling};
use remote_volume_core::log::{Log, MemoryLog, NoLog};
use remote_volume_core::server::{ApplyResult, CommandError, Extensions, Server, StateProvider};
use remote_volume_core::wsproto::{ActionSpec, TargetKind, ValueSpec};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

// ---- fake backend -------------------------------------------------------------------------------

struct FakeState {
    volume: u8,
    muted: bool,
    device: Option<String>,
    features: Features,
    reads: usize,
    calls: Vec<String>,
    fail_volume_reads: bool,
    fail_mute: bool,
    /// Makes volume reads slow, like a real OS call.
    read_delay_ms: u64,
    /// Per-app sessions; `None` = per-app audio unsupported (the default, like every OS today).
    sessions: Option<Vec<AppSession>>,
    fail_apps: bool,
    /// What `press_media_key` answers: `None` = a player got the key.
    media_error: Option<ErrorCode>,
}

type Shared = Arc<Mutex<FakeState>>;

struct FakeBackend(Shared);

fn failure() -> AudioError {
    AudioError::new(ErrorCode::Internal, "helper crashed")
}

impl Backend for FakeBackend {
    fn capabilities(&mut self) -> Capabilities {
        Capabilities::none("fake")
    }
    fn list_output_devices(&mut self) -> AudioResult<Vec<Device>> {
        let s = self.0.lock().unwrap();
        Ok(s.device
            .iter()
            .map(|name| Device {
                id: "dev".into(),
                name: name.clone(),
                default: true,
            })
            .collect())
    }
    fn get_volume(&mut self) -> AudioResult<u8> {
        let mut s = self.0.lock().unwrap();
        s.reads += 1;
        let delay = s.read_delay_ms;
        if delay > 0 {
            drop(s);
            std::thread::sleep(Duration::from_millis(delay));
            s = self.0.lock().unwrap();
        }
        if s.fail_volume_reads {
            return Err(failure());
        }
        Ok(s.volume)
    }
    fn set_volume(&mut self, percent: u8) -> AudioResult<u8> {
        let mut s = self.0.lock().unwrap();
        s.calls.push(format!("setVolume {percent}"));
        s.volume = percent;
        Ok(percent)
    }
    fn get_muted(&mut self) -> AudioResult<bool> {
        Ok(self.0.lock().unwrap().muted)
    }
    fn set_muted(&mut self, muted: bool) -> AudioResult<bool> {
        let mut s = self.0.lock().unwrap();
        if s.fail_mute {
            return Err(failure());
        }
        s.calls.push(format!("setMuted {muted}"));
        s.muted = muted;
        Ok(muted)
    }
    fn features(&mut self) -> Features {
        self.0.lock().unwrap().features
    }
    fn list_app_sessions(&mut self) -> AudioResult<Vec<AppSession>> {
        let s = self.0.lock().unwrap();
        if s.fail_apps {
            return Err(failure());
        }
        s.sessions
            .clone()
            .ok_or_else(|| AudioError::new(ErrorCode::Unsupported, "no per-app audio"))
    }
    fn set_app_volume(&mut self, id: &str, percent: u8) -> AudioResult<()> {
        self.each_session(id, format!("setAppVolume {id} {percent}"), |x| {
            x.volume = percent
        })
    }
    fn set_app_muted(&mut self, id: &str, muted: bool) -> AudioResult<()> {
        self.each_session(id, format!("setAppMuted {id} {muted}"), |x| x.muted = muted)
    }
    fn press_media_key(&mut self, key: MediaKey) -> AudioResult<()> {
        let mut s = self.0.lock().unwrap();
        if let Some(code) = s.media_error {
            return Err(AudioError::new(code, "no player"));
        }
        s.calls.push(format!("mediaKey {key:?}"));
        Ok(())
    }
    fn press_function_key(&mut self, number: u8) -> AudioResult<()> {
        let mut s = self.0.lock().unwrap();
        if let Some(code) = s.media_error {
            return Err(AudioError::new(code, "cannot press keys"));
        }
        s.calls.push(format!("functionKey F{number}"));
        Ok(())
    }
}

impl FakeBackend {
    fn each_session(
        &mut self,
        id: &str,
        call: String,
        f: impl Fn(&mut AppSession),
    ) -> AudioResult<()> {
        let mut s = self.0.lock().unwrap();
        if s.fail_apps {
            return Err(failure());
        }
        s.calls.push(call);
        let sessions = s.sessions.as_mut().expect("per-app audio enabled");
        let mut found = false;
        for x in sessions.iter_mut().filter(|x| x.id == id) {
            f(x);
            found = true;
        }
        if found {
            Ok(())
        } else {
            Err(AudioError::new(ErrorCode::NotFound, id))
        }
    }
}

// ---- harness ------------------------------------------------------------------------------------

struct Env {
    server: Server,
    state: Shared,
    log: Arc<MemoryLog>,
    url: String,
}

fn base_config() -> Config {
    let mut config = Config::default();
    config.websocket.port = 0;
    config.websocket.host = "127.0.0.1".into();
    config
}

async fn env_with(config: Config, ext: Extensions) -> Env {
    let state: Shared = Arc::new(Mutex::new(FakeState {
        volume: 40,
        muted: false,
        device: Some("Speakers".into()),
        features: Features::default(),
        reads: 0,
        calls: Vec::new(),
        fail_volume_reads: false,
        fail_mute: false,
        read_delay_ms: 0,
        sessions: None,
        fail_apps: false,
        media_error: None,
    }));
    let backend_state = state.clone();
    let audio = AudioHandle::spawn(move || Box::new(FakeBackend(backend_state)));
    let log = MemoryLog::shared();
    let server = Server::with_extensions(audio, config, log.clone(), ext);
    let addr = server.start().await.unwrap();
    Env {
        server,
        state,
        log,
        url: format!("ws://127.0.0.1:{}", addr.port()),
    }
}

async fn env() -> Env {
    env_with(base_config(), Extensions::default()).await
}

impl Env {
    fn set(&self, f: impl FnOnce(&mut FakeState)) {
        f(&mut self.state.lock().unwrap());
    }
    fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }
    async fn connect(&self) -> Client {
        self.try_connect("", &[]).await.expect("connect")
    }
    /// Connect, or return the HTTP status the server refused with.
    async fn try_connect(&self, path: &str, headers: &[(&str, &str)]) -> Result<Client, u16> {
        let mut request = format!("{}{path}", self.url).into_client_request().unwrap();
        for (name, value) in headers {
            request.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        match connect_async(request).await {
            Ok((ws, _)) => Ok(Client {
                ws,
                seen: Vec::new(),
            }),
            Err(WsError::Http(response)) => Err(response.status().as_u16()),
            Err(other) => panic!("unexpected connect error: {other}"),
        }
    }
}

struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    seen: Vec<Value>,
}

impl Client {
    async fn send(&mut self, value: Value) {
        self.send_text(&value.to_string()).await;
    }
    async fn send_text(&mut self, text: &str) {
        self.ws.send(Message::text(text)).await.unwrap();
    }
    /// The next JSON text message.
    async fn next(&mut self) -> Value {
        loop {
            match timeout(Duration::from_secs(3), self.ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let value: Value = serde_json::from_str(text.as_str()).unwrap();
                    self.seen.push(value.clone());
                    return value;
                }
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                other => panic!(
                    "expected a message, got {other:?}; seen so far: {:?}",
                    self.seen
                ),
            }
        }
    }
    /// The first message matching `pred`, skipping others.
    async fn until(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            let v = self.next().await;
            if pred(&v) {
                return v;
            }
        }
    }
    /// Assert that nothing arrives for a while.
    async fn quiet(&mut self) {
        if let Ok(Some(Ok(Message::Text(t)))) =
            timeout(Duration::from_millis(200), self.ws.next()).await
        {
            panic!("unexpected message: {t}");
        }
    }
    /// Wait for the connection to close; returns the close code and reason.
    async fn closed(&mut self) -> (u16, String) {
        loop {
            match timeout(Duration::from_secs(3), self.ws.next()).await {
                Ok(Some(Ok(Message::Close(Some(frame))))) => {
                    return (u16::from(frame.code), frame.reason.to_string())
                }
                Ok(Some(Ok(Message::Close(None)))) | Ok(None) | Ok(Some(Err(_))) => {
                    return (0, String::new())
                }
                Ok(Some(Ok(_))) => continue,
                Err(_) => panic!("the connection did not close"),
            }
        }
    }
}

fn no_caps() -> String {
    format!(
        r#"{{"protocolVersion":2,"version":"{}","outputDevices":{{"list":false,"select":false}},"apps":{{"volume":false,"mute":false}},"media":{{"keys":false}},"keyboard":{{"functionKeys":false}}}}"#,
        remote_volume_core::VERSION
    )
}

// ---- v1 behaviour -------------------------------------------------------------------------------

#[tokio::test]
async fn pushes_the_current_state_on_connect() {
    let env = env().await;
    let mut c = env.connect().await;
    let first = c.next().await;
    assert_eq!(
        first.to_string(),
        format!(
            r#"{{"volume":40,"muted":false,"outputDevice":"Speakers","capabilities":{}}}"#,
            no_caps()
        ),
        "key order and shape are part of the contract"
    );
}

#[tokio::test]
async fn a_missing_output_device_is_null() {
    let env = env().await;
    env.set(|s| s.device = None);
    let mut c = env.connect().await;
    assert_eq!(c.next().await["outputDevice"], Value::Null);
}

#[tokio::test]
async fn executes_every_documented_action() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    for msg in [
        json!({"action":"setVolume","value":70}),
        json!({"action":"increaseVolume","value":5}),
        json!({"action":"decreaseVolume","value":10}),
        json!({"action":"mute"}),
        json!({"action":"unmute"}),
        json!({"action":"toggleMute"}),
    ] {
        c.send(msg).await;
    }
    sleep(Duration::from_millis(150)).await;
    c.send(json!({"action":"getState"})).await;
    let state = c.until(|m| m.get("outputDevice").is_some()).await;
    assert_eq!(
        state.to_string(),
        r#"{"volume":65,"muted":true,"outputDevice":"Speakers"}"#
    );
    c.send(json!({"action":"isMuted"})).await;
    assert_eq!(
        c.until(|m| m == &json!({"muted":true})).await.to_string(),
        r#"{"muted":true}"#
    );
    assert_eq!(
        env.calls(),
        [
            "setVolume 70",
            "setVolume 75",
            "setVolume 65",
            "setMuted true",
            "setMuted false",
            "setMuted true"
        ]
    );
}

#[tokio::test]
async fn increase_and_decrease_stop_at_the_ends() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    env.set(|s| s.volume = 95);
    c.send(json!({"action":"increaseVolume","value":10})).await;
    assert_eq!(c.next().await, json!({"volume":100}));
    c.send(json!({"action":"decreaseVolume","value":99})).await;
    assert_eq!(c.next().await, json!({"volume":1}));
    c.send(json!({"action":"decreaseVolume","value":5})).await;
    assert_eq!(c.next().await, json!({"volume":0}));
}

#[tokio::test]
async fn commands_push_the_new_state_to_every_client_even_with_polling_disabled() {
    let env = env().await;
    let (mut a, mut b) = (env.connect().await, env.connect().await);
    a.next().await;
    b.next().await;

    a.send(json!({"action":"setVolume","value":70})).await;
    assert_eq!(a.next().await, json!({"volume":70}));
    assert_eq!(b.next().await, json!({"volume":70}));

    b.send(json!({"action":"toggleMute"})).await;
    assert_eq!(a.next().await, json!({"muted":true}));
    assert_eq!(b.next().await, json!({"muted":true}));

    a.send(json!({"action":"decreaseVolume","value":20})).await;
    assert_eq!(b.next().await, json!({"volume":50}));
}

#[tokio::test]
async fn only_changed_fields_are_pushed_after_a_command() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"setVolume","value":40})).await; // same as now: nothing to push
    c.send(json!({"action":"mute"})).await;
    assert_eq!(c.next().await, json!({"muted":true}));
    c.quiet().await;
}

#[tokio::test]
async fn a_burst_of_commands_ends_with_the_final_value_and_never_reads_more_than_once_per_command()
{
    let env = env_with(
        base_config(),
        Extensions {
            rate_limit: Some((100, 1000.0)),
            ..Extensions::default()
        },
    )
    .await;
    let mut c = env.connect().await;
    c.next().await;
    env.set(|s| s.read_delay_ms = 10);
    let before = env.state.lock().unwrap().reads;
    for v in 1..=15 {
        c.send(json!({"action":"setVolume","value":v})).await;
    }
    c.until(|m| m == &json!({"volume":15})).await;
    sleep(Duration::from_millis(100)).await;
    let reads = env.state.lock().unwrap().reads - before;
    assert!(reads <= 15, "{reads} state reads for 15 commands");
    assert_eq!(env.state.lock().unwrap().volume, 15);
}

#[tokio::test]
async fn simultaneous_refreshes_are_coalesced_into_one_extra_pass() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    env.set(|s| s.read_delay_ms = 50);
    let before = env.state.lock().unwrap().reads;
    env.set(|s| s.volume = 61);
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let server = env.server.clone();
        tasks.push(tokio::spawn(async move { server.refresh().await }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let reads = env.state.lock().unwrap().reads - before;
    assert!(
        reads <= 2,
        "expected at most 2 passes for 10 simultaneous refreshes, got {reads}"
    );
    assert_eq!(c.next().await, json!({"volume":61}));
    c.quiet().await;
}

#[tokio::test]
async fn failed_commands_do_not_push_state() {
    let env = env().await;
    env.set(|s| s.fail_mute = true);
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"mute"})).await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"mute","response":{"error":"Failed to execute action"}}"#
    );
    c.send(json!({"action":"isMuted"})).await;
    assert_eq!(c.next().await, json!({"muted":false}));
    c.quiet().await;
    assert!(env
        .log
        .lines()
        .iter()
        .any(|l| l.starts_with("ERROR Error executing mute")));
}

#[tokio::test]
async fn a_new_client_brings_others_up_to_date_with_external_changes() {
    let env = env().await;
    let mut a = env.connect().await;
    a.next().await;
    env.set(|s| s.volume = 5); // changed outside the app, polling disabled
    let mut b = env.connect().await;
    let first = b.next().await;
    assert_eq!(first["volume"], 5);
    assert!(first.get("capabilities").is_some());
    assert_eq!(a.next().await, json!({"volume":5}));
    b.quiet().await; // the newcomer gets the full state once, not the diff too
}

#[tokio::test]
async fn invalid_messages_get_an_error_reply_and_never_reach_the_backend() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    for text in [
        r#"{"action":"setVolume","value":101}"#,
        r#"{"action":"setVolume","value":"50"}"#,
        r#"{"action":"increaseVolume","value":0}"#,
        r#"{"action":"constructor"}"#,
        "not json",
        r#"{"value":1}"#,
    ] {
        c.send_text(text).await;
    }
    let replies = [
        c.next().await,
        c.next().await,
        c.next().await,
        c.next().await,
        c.next().await,
        c.next().await,
    ];
    assert!(replies[0]["response"]["error"]
        .as_str()
        .unwrap()
        .contains("between 0 and 100"));
    assert!(replies[1]["response"]["error"]
        .as_str()
        .unwrap()
        .contains("between 0 and 100"));
    assert!(replies[2]["response"]["error"]
        .as_str()
        .unwrap()
        .contains("between 1 and 99"));
    assert_eq!(
        replies[3].to_string(),
        r#"{"action":"constructor","response":{"error":"Invalid action"}}"#
    );
    assert_eq!(
        replies[4].to_string(),
        r#"{"error":"Failed to process message"}"#
    );
    assert_eq!(replies[5].to_string(), r#"{"error":"Invalid action"}"#);
    assert!(env.calls().is_empty());
}

#[tokio::test]
async fn binary_frames_are_ignored() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    c.ws.send(Message::binary(br#"{"action":"mute"}"#.to_vec()))
        .await
        .unwrap();
    c.quiet().await;
    assert!(env.calls().is_empty());
}

#[tokio::test]
async fn a_failing_state_read_does_not_crash_the_server() {
    let env = env().await;
    env.set(|s| s.fail_volume_reads = true);
    let mut c = env.connect().await;
    c.send(json!({"action":"mute"})).await; // triggers a refresh that fails
    c.quiet().await;
    c.send(json!({"action":"isMuted"})).await;
    assert_eq!(
        c.next().await,
        json!({"muted":true}),
        "the connection still works"
    );
    assert!(env
        .log
        .lines()
        .iter()
        .any(|l| l.contains("Failed to send initial state")));
}

#[tokio::test]
async fn messages_over_1_kb_close_the_connection_with_1009() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    let pad = "x".repeat(2000);
    c.send(json!({"action":"getState","pad":pad})).await;
    assert_eq!(c.closed().await.0, 1009);

    // exactly at the limit is fine
    let mut c = env.connect().await;
    c.next().await;
    let base = json!({"action":"isMuted","pad":""}).to_string().len();
    c.send(json!({"action":"isMuted","pad":"x".repeat(1024 - base)}))
        .await;
    assert_eq!(c.next().await, json!({"muted":false}));
}

#[tokio::test]
async fn browser_origins_are_rejected_allow_listed_ones_accepted() {
    let mut config = base_config();
    let env = env_with(config.clone(), Extensions::default()).await;
    assert_eq!(
        env.try_connect("", &[("Origin", "https://evil.example")])
            .await
            .err(),
        Some(403)
    );
    config.websocket.allowed_origins = vec!["http://localhost:8080".into()];
    env.server.apply_config(config).await.unwrap();
    assert!(env
        .try_connect("", &[("Origin", "http://localhost:8080")])
        .await
        .is_ok());
    assert_eq!(
        env.try_connect("", &[("Origin", "http://localhost:8081")])
            .await
            .err(),
        Some(403)
    );
}

#[tokio::test]
async fn the_token_is_enforced_when_configured() {
    let mut config = base_config();
    config.websocket.token = "secret-token".into();
    let env = env_with(config, Extensions::default()).await;
    assert_eq!(env.try_connect("", &[]).await.err(), Some(401));
    assert_eq!(env.try_connect("/?token=nope", &[]).await.err(), Some(401));
    assert!(env.try_connect("/?token=secret-token", &[]).await.is_ok());
    assert!(env
        .try_connect("", &[("Authorization", "Bearer secret-token")])
        .await
        .is_ok());
    assert!(env
        .log
        .lines()
        .iter()
        .any(|l| l.contains("Rejected WebSocket client") && l.contains("token")));
}

#[tokio::test]
async fn at_most_32_clients_the_33rd_gets_503_and_a_freed_slot_can_be_reused() {
    let env = env().await;
    let mut clients = Vec::new();
    for _ in 0..32 {
        clients.push(env.connect().await);
    }
    assert_eq!(env.try_connect("", &[]).await.err(), Some(503));
    drop(clients.pop());
    for _ in 0..50 {
        if env.server.client_count() < 32 {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert!(env.try_connect("", &[]).await.is_ok());
}

#[tokio::test]
async fn rate_limit_rejects_bursts_beyond_the_bucket() {
    let env = env_with(
        base_config(),
        Extensions {
            rate_limit: Some((5, 0.001)),
            ..Extensions::default()
        },
    )
    .await;
    let mut c = env.connect().await;
    c.next().await;
    for _ in 0..8 {
        c.send(json!({"action":"isMuted"})).await;
    }
    let mut replies = Vec::new();
    for _ in 0..8 {
        replies.push(c.next().await);
    }
    let limited = replies
        .iter()
        .filter(|m| **m == json!({"error": "Rate limit exceeded"}))
        .count();
    assert_eq!(limited, 3);
}

#[tokio::test]
async fn refresh_broadcasts_external_changes_one_message_per_changed_field() {
    let env = env().await;
    let (mut a, mut b) = (env.connect().await, env.connect().await);
    a.next().await;
    b.next().await;
    env.set(|s| s.volume = 12);
    env.server.refresh().await;
    assert_eq!(a.next().await, json!({"volume":12}));
    assert_eq!(b.next().await, json!({"volume":12}));
    env.set(|s| {
        s.device = Some("Headset".into());
        s.muted = true;
    });
    env.server.refresh().await;
    assert_eq!(
        [a.next().await, a.next().await],
        [json!({"muted":true}), json!({"outputDevice":"Headset"})]
    );
    env.server.refresh().await; // nothing changed
    a.quiet().await;
    assert_eq!(a.seen.len(), 4);
}

#[tokio::test]
async fn polling_picks_up_external_changes_when_enabled() {
    let mut config = base_config();
    config.polling = Polling {
        enabled: true,
        interval: 100,
    };
    let env = env_with(config, Extensions::default()).await;
    let mut c = env.connect().await;
    c.next().await;
    env.set(|s| s.volume = 99);
    assert_eq!(c.next().await, json!({"volume":99}));
}

#[tokio::test]
async fn without_polling_external_changes_are_not_pushed_on_their_own() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    env.set(|s| s.volume = 99);
    sleep(Duration::from_millis(300)).await;
    c.quiet().await;
}

#[tokio::test]
async fn os_change_notifications_push_external_changes_without_polling() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    let changed = env.server.change_notifier();
    env.set(|s| s.volume = 98);
    // A burst of notifications from another thread is read in one pass.
    std::thread::spawn(move || (0..5).for_each(|_| changed()))
        .join()
        .unwrap();
    assert_eq!(c.next().await, json!({"volume":98}));
    c.quiet().await;
}

// ---- configuration changes ----------------------------------------------------------------------

#[tokio::test]
async fn restart_applies_a_new_port_and_disconnects_old_clients() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    let addr = env.server.restart().await.unwrap();
    c.closed().await;
    let mut again = format!("ws://127.0.0.1:{}", addr.port())
        .into_client_request()
        .unwrap();
    again
        .headers_mut()
        .insert("X-Test", HeaderValue::from_static("1"));
    let (mut ws, _) = connect_async(again).await.unwrap();
    assert!(ws.next().await.is_some());
}

#[tokio::test]
async fn saving_unrelated_or_unchanged_settings_keeps_the_server_and_clients_untouched() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    let same = env.server.apply_config(env.server.config()).await.unwrap();
    assert_eq!(
        same,
        ApplyResult {
            restarted: false,
            disconnected: 0,
            polling_restarted: false
        }
    );
    c.send(json!({"action":"isMuted"})).await;
    assert_eq!(c.next().await, json!({"muted":false}));
}

#[tokio::test]
async fn changing_polling_only_restarts_the_timer() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    let mut config = env.server.config();
    config.polling = Polling {
        enabled: true,
        interval: 100,
    };
    let result = env.server.apply_config(config).await.unwrap();
    assert_eq!(
        result,
        ApplyResult {
            restarted: false,
            disconnected: 0,
            polling_restarted: true
        }
    );
    env.set(|s| s.volume = 77);
    assert_eq!(
        c.next().await,
        json!({"volume":77}),
        "the same connection receives polled changes"
    );
}

#[tokio::test]
async fn setting_a_token_disconnects_only_clients_without_it() {
    let env = env().await;
    let mut plain = env.connect().await;
    plain.next().await;
    let mut config = env.server.config();
    config.websocket.token = "new-secret".into();
    let result = env.server.apply_config(config.clone()).await.unwrap();
    assert_eq!(
        result,
        ApplyResult {
            restarted: false,
            disconnected: 1,
            polling_restarted: false
        }
    );
    assert_eq!(
        plain.closed().await,
        (1008, "Access settings changed".to_string())
    );
    assert_eq!(env.try_connect("", &[]).await.err(), Some(401));
    let mut authed = env.try_connect("/?token=new-secret", &[]).await.unwrap();
    authed.next().await;

    // Changing an unrelated access rule keeps clients that still pass.
    config.websocket.allowed_origins = vec!["http://localhost:9000".into()];
    assert_eq!(
        env.server
            .apply_config(config.clone())
            .await
            .unwrap()
            .disconnected,
        0
    );
    // Removing the token keeps everyone.
    config.websocket.token = String::new();
    assert_eq!(
        env.server.apply_config(config).await.unwrap().disconnected,
        0
    );
    authed.send(json!({"action":"isMuted"})).await;
    assert_eq!(authed.next().await, json!({"muted":false}));
}

#[tokio::test]
async fn removing_an_allowed_origin_disconnects_that_browser_client_only() {
    let mut config = base_config();
    config.websocket.allowed_origins = vec!["http://localhost:8080".into()];
    let env = env_with(config.clone(), Extensions::default()).await;
    let mut browser = env
        .try_connect("", &[("Origin", "http://localhost:8080")])
        .await
        .unwrap();
    let mut native = env.connect().await;
    browser.next().await;
    native.next().await;
    config.websocket.allowed_origins.clear();
    assert_eq!(
        env.server.apply_config(config).await.unwrap().disconnected,
        1
    );
    assert_eq!(browser.closed().await.0, 1008);
    native.send(json!({"action":"isMuted"})).await;
    assert_eq!(native.next().await, json!({"muted":false}));
}

#[tokio::test]
async fn changing_the_port_or_bind_address_restarts_the_listener() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    let mut config = env.server.config();
    config.websocket.host = "0.0.0.0".into();
    let result = env.server.apply_config(config).await.unwrap();
    assert!(result.restarted);
    c.closed().await;
    assert_eq!(env.server.local_addr().unwrap().ip().to_string(), "0.0.0.0");
}

#[tokio::test]
async fn binding_to_127_0_0_1_is_honoured() {
    let env = env().await;
    assert_eq!(
        env.server.local_addr().unwrap().ip().to_string(),
        "127.0.0.1"
    );
}

#[tokio::test]
async fn start_fails_when_the_port_is_taken() {
    let env = env().await;
    let mut config = base_config();
    config.websocket.port = env.server.local_addr().unwrap().port();
    let audio = AudioHandle::spawn(|| Box::new(FakeBackend(Arc::new(Mutex::new(blank_state())))));
    let second = Server::new(audio, config, Arc::new(NoLog) as Arc<dyn Log>);
    let error = second.start().await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
}

fn blank_state() -> FakeState {
    FakeState {
        volume: 0,
        muted: false,
        device: None,
        features: Features::default(),
        reads: 0,
        calls: Vec::new(),
        fail_volume_reads: false,
        fail_mute: false,
        read_delay_ms: 0,
        sessions: None,
        fail_apps: false,
        media_error: None,
    }
}

#[tokio::test]
async fn stop_disconnects_everyone_and_frees_the_port() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    let port = env.server.local_addr().unwrap().port();
    env.server.stop().await;
    c.closed().await;
    assert!(env.server.local_addr().is_none());
    assert!(tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .is_ok());
}

// ---- protocol v2 --------------------------------------------------------------------------------

#[tokio::test]
async fn get_state_keeps_its_v1_shape_without_capabilities() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"getState"})).await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"volume":40,"muted":false,"outputDevice":"Speakers"}"#
    );
}

#[tokio::test]
async fn get_capabilities_replies_with_the_backends_runtime_answer() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"getCapabilities"})).await;
    assert_eq!(
        c.next().await.to_string(),
        format!(r#"{{"capabilities":{}}}"#, no_caps())
    );

    env.set(|s| {
        s.features = Features {
            devices_list: true,
            devices_select: true,
            apps_volume: true,
            apps_mute: true,
            media_keys: true,
            function_keys: true,
        }
    });
    c.send(json!({"action":"getCapabilities"})).await;
    assert_eq!(
        c.next().await,
        json!({"capabilities":{"protocolVersion":2,"version":remote_volume_core::VERSION,"outputDevices":{"list":true,"select":true},"apps":{"volume":true,"mute":true},"media":{"keys":true},"keyboard":{"functionKeys":true}}})
    );
    assert!(
        env.calls().is_empty(),
        "a capability query does not touch the volume"
    );
}

#[tokio::test]
async fn the_initial_state_carries_the_same_capabilities_object() {
    let env = env().await;
    env.set(|s| s.features.apps_volume = true);
    let mut c = env.connect().await;
    let first = c.next().await;
    assert_eq!(
        first["capabilities"]["apps"],
        json!({"volume":true,"mute":false})
    );
    c.send(json!({"action":"getCapabilities"})).await;
    assert_eq!(c.next().await["capabilities"], first["capabilities"]);
}

#[tokio::test]
async fn capabilities_are_never_broadcast_as_state_changes() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    env.set(|s| {
        s.features.apps_volume = true;
        s.volume = 41;
    });
    env.server.refresh().await;
    assert_eq!(c.next().await, json!({"volume":41}));
    c.quiet().await;
}

#[tokio::test]
async fn media_keys_reach_the_backend_without_reply_or_state_push() {
    let env = env().await;
    env.set(|s| s.features.media_keys = true);
    let mut c = env.connect().await;
    c.next().await;
    for action in ["mediaPlayPause", "mediaNextTrack", "mediaPreviousTrack"] {
        c.send(json!({ "action": action })).await;
    }
    sleep(Duration::from_millis(200)).await;
    c.quiet().await;
    assert_eq!(
        env.calls(),
        ["mediaKey PlayPause", "mediaKey Next", "mediaKey Previous"]
    );
}

#[tokio::test]
async fn media_keys_are_refused_where_unsupported_or_without_a_player() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"mediaPlayPause"})).await;
    assert_eq!(
        c.next().await,
        json!({"action":"mediaPlayPause","response":{"error":"Not supported on this OS"}})
    );

    env.set(|s| {
        s.features.media_keys = true;
        s.media_error = Some(ErrorCode::NotFound);
    });
    c.send(json!({"action":"mediaNextTrack"})).await;
    assert_eq!(
        c.next().await,
        json!({"action":"mediaNextTrack","response":{"error":"No media player found"}})
    );

    env.set(|s| s.media_error = Some(ErrorCode::Internal));
    c.send(json!({"action":"mediaPreviousTrack"})).await;
    assert_eq!(
        c.next().await,
        json!({"action":"mediaPreviousTrack","response":{"error":"Failed to execute action"}})
    );
    assert!(env.calls().is_empty());
}

#[tokio::test]
async fn function_keys_reach_the_backend_or_are_refused() {
    let env = env().await;
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"pressFunctionKey","value":13}))
        .await;
    assert_eq!(
        c.next().await,
        json!({"action":"pressFunctionKey","response":{"error":"Not supported on this OS"}})
    );

    env.set(|s| s.features.function_keys = true);
    c.send(json!({"action":"pressFunctionKey","value":13}))
        .await;
    c.send(json!({"action":"pressFunctionKey","value":24}))
        .await;
    c.send(json!({"action":"pressFunctionKey","value":25}))
        .await;
    assert_eq!(
        c.next().await,
        json!({"action":"pressFunctionKey","response":{"error":"Invalid value for pressFunctionKey. Must be an integer between 1 and 24."}})
    );
    c.quiet().await;
    assert_eq!(env.calls(), ["functionKey F13", "functionKey F24"]);

    env.set(|s| s.media_error = Some(ErrorCode::Internal));
    c.send(json!({"action":"pressFunctionKey","value":1})).await;
    assert_eq!(
        c.next().await,
        json!({"action":"pressFunctionKey","response":{"error":"Failed to execute action"}})
    );
}

// Device actions come with #12. These tests plug a stand-in ("things") into the extension points to
// check the shared machinery: capability gating, string targets, not-found errors, structured state.

struct V2 {
    apps: Arc<Mutex<Value>>,
    calls: Arc<Mutex<Vec<(String, i64)>>>,
    fail: Arc<Mutex<Option<CommandError2>>>,
}

#[derive(Clone, Copy)]
enum CommandError2 {
    Device,
    Boom,
}

fn v2_extensions() -> (Extensions, V2) {
    let apps = Arc::new(Mutex::new(
        json!([{"id":"spotify","name":"Spotify","volume":40,"muted":false}]),
    ));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fail: Arc<Mutex<Option<CommandError2>>> = Arc::new(Mutex::new(None));

    let mut ext = Extensions::default();
    ext.actions.push(ActionSpec {
        value: Some(ValueSpec {
            min: 0,
            max: 100,
            error: "bad value",
        }),
        target: Some(TargetKind::App),
        requires: Some("apps.volume"),
        mutates: true,
        ..ActionSpec::new("setThingVolume")
    });
    ext.actions.push(ActionSpec {
        requires: Some("apps.volume"),
        reply: true,
        ..ActionSpec::new("getThings")
    });

    let (a, c, f) = (apps.clone(), calls.clone(), fail.clone());
    ext.commands.insert(
        "setThingVolume",
        Arc::new(move |msg| {
            let (a, c, f) = (a.clone(), c.clone(), f.clone());
            Box::pin(async move {
                let app = msg.app.clone().unwrap();
                c.lock().unwrap().push((app.clone(), msg.value.unwrap()));
                match *f.lock().unwrap() {
                    Some(CommandError2::Device) => {
                        return Err(CommandError::NotFound(TargetKind::Device))
                    }
                    Some(CommandError2::Boom) => return Err(CommandError::Failed("boom".into())),
                    None => {}
                }
                let mut apps = a.lock().unwrap();
                match apps
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|x| x["id"] == app.as_str())
                {
                    Some(entry) => entry["volume"] = json!(msg.value.unwrap()),
                    None => return Err(CommandError::NotFound(TargetKind::App)),
                }
                Ok(Value::Null)
            })
        }),
    );
    let a = apps.clone();
    ext.commands.insert(
        "getThings",
        Arc::new(move |_| {
            let a = a.clone();
            Box::pin(async move { Ok(json!({ "things": a.lock().unwrap().clone() })) })
        }),
    );
    let a = apps.clone();
    let provider: StateProvider = Arc::new(move || {
        let a = a.clone();
        Box::pin(async move { Ok(a.lock().unwrap().clone()) })
    });
    ext.state.push(("things", provider));
    (ext, V2 { apps, calls, fail })
}

#[tokio::test]
async fn actions_that_need_an_unsupported_capability_are_refused_and_never_run() {
    let (ext, v2) = v2_extensions();
    let env = env_with(base_config(), ext).await;
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"setThingVolume","app":"spotify","value":10}))
        .await;
    c.send(json!({"action":"getThings"})).await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"setThingVolume","response":{"error":"Not supported on this OS"}}"#
    );
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"getThings","response":{"error":"Not supported on this OS"}}"#
    );
    assert!(v2.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn capability_gating_follows_the_runtime_answer_and_commands_push_the_resulting_list() {
    let (ext, v2) = v2_extensions();
    let env = env_with(base_config(), ext).await;
    let mut c = env.connect().await;
    c.next().await;
    env.set(|s| s.features.apps_volume = true); // e.g. the Pulse server came up
    c.send(json!({"action":"setThingVolume","app":"spotify","value":10}))
        .await;
    assert_eq!(
        c.next().await,
        json!({"things":[{"id":"spotify","name":"Spotify","volume":10,"muted":false}]})
    );
    assert_eq!(*v2.calls.lock().unwrap(), [("spotify".to_string(), 10)]);
    c.send(json!({"action":"getThings"})).await;
    assert_eq!(c.next().await["things"][0]["volume"], 10);
}

#[tokio::test]
async fn target_validation_errors_come_back_before_any_capability_or_command_work() {
    let (ext, v2) = v2_extensions();
    let env = env_with(base_config(), ext).await;
    env.set(|s| s.features.apps_volume = true);
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"setThingVolume","value":10})).await;
    c.send(json!({"action":"setThingVolume","app":"a\u{0}b","value":10}))
        .await;
    c.send(json!({"action":"setThingVolume","app":7,"value":10}))
        .await;
    c.send(json!({"action":"setThingVolume","app":"x".repeat(257),"value":10}))
        .await;
    for _ in 0..4 {
        let reply = c.next().await;
        assert_eq!(reply["action"], "setThingVolume");
        assert!(
            reply["response"]["error"]
                .as_str()
                .unwrap()
                .starts_with("Invalid app for setThingVolume."),
            "{reply}"
        );
    }
    assert!(v2.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unknown_target_gets_its_own_error_not_failed_to_execute() {
    let (ext, v2) = v2_extensions();
    let env = env_with(base_config(), ext).await;
    env.set(|s| s.features.apps_volume = true);
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"setThingVolume","app":"nope","value":10}))
        .await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"setThingVolume","response":{"error":"App not found"}}"#
    );

    *v2.fail.lock().unwrap() = Some(CommandError2::Device);
    c.send(json!({"action":"setThingVolume","app":"spotify","value":10}))
        .await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"setThingVolume","response":{"error":"Device not found"}}"#
    );

    *v2.fail.lock().unwrap() = Some(CommandError2::Boom);
    c.send(json!({"action":"setThingVolume","app":"spotify","value":10}))
        .await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"setThingVolume","response":{"error":"Failed to execute action"}}"#
    );
    c.quiet().await; // none of these pushed state
}

#[tokio::test]
async fn list_valued_state_is_broadcast_only_when_its_content_changes_one_message_per_field() {
    let (ext, v2) = v2_extensions();
    let env = env_with(base_config(), ext).await;
    let (mut a, mut b) = (env.connect().await, env.connect().await);
    let first = a.next().await;
    assert_eq!(first["things"], *v2.apps.lock().unwrap());
    b.next().await;

    // an equal list is not a change
    let same = v2.apps.lock().unwrap().clone();
    *v2.apps.lock().unwrap() = same;
    env.server.refresh().await;
    a.quiet().await;

    // a change inside an element is
    v2.apps.lock().unwrap()[0]["volume"] = json!(55);
    env.server.refresh().await;
    let changed = json!({"things":[{"id":"spotify","name":"Spotify","volume":55,"muted":false}]});
    assert_eq!(a.next().await, changed);
    assert_eq!(b.next().await, changed);

    // an app starting together with a scalar change is still one message per field
    v2.apps
        .lock()
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"discord","name":"Discord","volume":100,"muted":true}));
    env.set(|s| s.volume = 12);
    env.server.refresh().await;
    assert_eq!(a.next().await, json!({"volume":12}));
    assert_eq!(a.next().await["things"].as_array().unwrap().len(), 2);

    // an app stopping, then nothing changing
    v2.apps.lock().unwrap().as_array_mut().unwrap().remove(0);
    env.server.refresh().await;
    assert_eq!(a.next().await["things"][0]["id"], "discord");
    env.server.refresh().await;
    a.quiet().await;
    assert_eq!(a.seen.len(), 5);
}

#[tokio::test]
async fn lists_are_part_of_get_state_and_the_initial_state_and_late_joiners_update_others() {
    let (ext, v2) = v2_extensions();
    let env = env_with(base_config(), ext).await;
    let mut a = env.connect().await;
    a.next().await;
    *v2.apps.lock().unwrap() = json!([]);
    let mut b = env.connect().await;
    assert_eq!(b.next().await["things"], json!([]));
    assert_eq!(
        a.until(|m| m.get("things").is_some()).await,
        json!({"things":[]})
    );
    b.send(json!({"action":"getState"})).await;
    assert_eq!(
        b.next().await.to_string(),
        r#"{"volume":40,"muted":false,"outputDevice":"Speakers","things":[]}"#
    );
}

#[tokio::test]
async fn restart_forgets_what_clients_were_told() {
    let (ext, _v2) = v2_extensions();
    let env = env_with(base_config(), ext).await;
    let addr = env.server.restart().await.unwrap();
    let mut ws = format!("ws://127.0.0.1:{}", addr.port())
        .into_client_request()
        .unwrap();
    ws.headers_mut()
        .insert("X-Test", HeaderValue::from_static("1"));
    let (mut socket, _) = connect_async(ws).await.unwrap();
    let Some(Ok(Message::Text(text))) = socket.next().await else {
        panic!("no initial state")
    };
    let first: Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(first["things"][0]["id"], "spotify");
}

// ---- per-app audio (#16) ------------------------------------------------------------------------

fn session(id: &str, name: &str, volume: u8, muted: bool) -> AppSession {
    AppSession {
        id: id.into(),
        name: name.into(),
        volume,
        muted,
    }
}

/// Two Chrome sessions (one muted) and Spotify, with per-app volume and mute supported.
async fn env_apps() -> Env {
    let env = env().await;
    env.set(|s| {
        s.features.apps_volume = true;
        s.features.apps_mute = true;
        s.sessions = Some(vec![
            session("spotify", "Spotify", 80, false),
            session("chrome", "Google Chrome", 30, true),
            session("chrome", "", 70, false),
        ]);
    });
    env
}

const APPS: &str = r#"[{"id":"chrome","name":"Google Chrome","volume":70,"muted":false},{"id":"spotify","name":"Spotify","volume":80,"muted":false}]"#;

#[tokio::test]
async fn app_actions_are_refused_where_unsupported_and_the_state_has_no_apps() {
    let env = env().await;
    let mut c = env.connect().await;
    assert!(c.next().await.get("apps").is_none());
    for action in [
        json!({"action":"getApps"}),
        json!({"action":"setAppVolume","app":"spotify","value":10}),
        json!({"action":"increaseAppVolume","app":"spotify","value":10}),
        json!({"action":"decreaseAppVolume","app":"spotify","value":10}),
        json!({"action":"muteApp","app":"spotify"}),
        json!({"action":"unmuteApp","app":"spotify"}),
        json!({"action":"toggleAppMute","app":"spotify"}),
    ] {
        c.send(action.clone()).await;
        assert_eq!(
            c.next().await,
            json!({"action": action["action"], "response": {"error": "Not supported on this OS"}})
        );
    }
    c.send(json!({"action":"getState"})).await;
    assert!(c.next().await.get("apps").is_none());
    assert!(env.calls().is_empty());
}

#[tokio::test]
async fn sessions_are_grouped_into_apps_in_every_state_snapshot() {
    let env = env_apps().await;
    let mut c = env.connect().await;
    let first = c.next().await;
    assert_eq!(first["apps"].to_string(), APPS);
    assert_eq!(
        first["capabilities"]["apps"],
        json!({"volume":true,"mute":true})
    );
    c.send(json!({"action":"getApps"})).await;
    assert_eq!(c.next().await.to_string(), format!(r#"{{"apps":{APPS}}}"#));
    c.send(json!({"action":"getState"})).await;
    assert_eq!(
        c.next().await.to_string(),
        format!(r#"{{"volume":40,"muted":false,"outputDevice":"Speakers","apps":{APPS}}}"#)
    );
}

#[tokio::test]
async fn setting_an_app_volume_reaches_every_session_and_pushes_the_list_to_everyone() {
    let env = env_apps().await;
    let (mut a, mut b) = (env.connect().await, env.connect().await);
    a.next().await;
    b.next().await;
    a.send(json!({"action":"setAppVolume","app":"Chrome","value":50}))
        .await;
    let pushed = json!({"apps":[
        {"id":"chrome","name":"Google Chrome","volume":50,"muted":false},
        {"id":"spotify","name":"Spotify","volume":80,"muted":false},
    ]});
    assert_eq!(a.next().await, pushed);
    assert_eq!(b.next().await, pushed);
    assert_eq!(env.calls(), ["setAppVolume chrome 50"]);
    a.quiet().await; // no reply, and the master volume did not change
}

#[tokio::test]
async fn increase_and_decrease_start_from_the_loudest_session_and_stop_at_the_ends() {
    let env = env_apps().await;
    let mut c = env.connect().await;
    c.next().await;
    for (action, value) in [
        ("increaseAppVolume", 40),
        ("decreaseAppVolume", 99),
        ("decreaseAppVolume", 5),
        ("increaseAppVolume", 3),
    ] {
        c.send(json!({"action":action,"app":"chrome","value":value}))
            .await;
        c.until(|m| m.get("apps").is_some()).await;
    }
    assert_eq!(
        env.calls(),
        [
            "setAppVolume chrome 100",
            "setAppVolume chrome 1",
            "setAppVolume chrome 0",
            "setAppVolume chrome 3",
        ]
    );
}

#[tokio::test]
async fn mute_unmute_and_toggle_an_app() {
    let env = env_apps().await;
    let mut c = env.connect().await;
    c.next().await;
    // one Chrome session is muted, the other not: the app counts as unmuted, so toggle mutes it
    c.send(json!({"action":"toggleAppMute","app":"chrome"}))
        .await;
    assert_eq!(c.next().await["apps"][0]["muted"], true);
    c.send(json!({"action":"toggleAppMute","app":"chrome"}))
        .await;
    assert_eq!(c.next().await["apps"][0]["muted"], false);
    c.send(json!({"action":"muteApp","app":"spotify.exe"}))
        .await;
    assert_eq!(c.next().await["apps"][1]["muted"], true);
    c.send(json!({"action":"muteApp","app":"Spotify"})).await;
    c.send(json!({"action":"unmuteApp","app":"spotify"})).await;
    assert_eq!(c.next().await["apps"][1]["muted"], false);
    assert_eq!(
        env.calls(),
        [
            "setAppMuted chrome true",
            "setAppMuted chrome false",
            "setAppMuted spotify true",
            "setAppMuted spotify true",
            "setAppMuted spotify false",
        ]
    );
}

#[tokio::test]
async fn an_app_that_is_not_running_is_app_not_found_and_nothing_is_set() {
    let env = env_apps().await;
    let mut c = env.connect().await;
    c.next().await;
    for action in [
        json!({"action":"setAppVolume","app":"discord","value":10}),
        json!({"action":"increaseAppVolume","app":"spot","value":10}),
        json!({"action":"toggleAppMute","app":"discord"}),
    ] {
        c.send(action.clone()).await;
        assert_eq!(
            c.next().await,
            json!({"action": action["action"], "response": {"error": "App not found"}})
        );
    }
    c.quiet().await;
    assert!(env.calls().is_empty());
    assert!(env.log.lines().iter().any(|l| l.contains("App not found")));
}

#[tokio::test]
async fn a_failing_app_list_never_costs_the_volume_state() {
    let env = env_apps().await;
    env.set(|s| s.fail_apps = true);
    let mut c = env.connect().await;
    let first = c.next().await;
    assert_eq!(first["volume"], 40);
    assert!(first.get("apps").is_none());
    c.send(json!({"action":"getApps"})).await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"getApps","response":{"error":"Failed to execute action"}}"#
    );
    c.send(json!({"action":"muteApp","app":"chrome"})).await;
    assert_eq!(
        c.next().await["response"]["error"],
        "Failed to execute action"
    );
}

#[tokio::test]
async fn a_backend_that_says_unsupported_despite_its_capabilities_gets_not_supported() {
    let env = env_apps().await;
    env.set(|s| s.sessions = None);
    let mut c = env.connect().await;
    c.next().await;
    c.send(json!({"action":"muteApp","app":"chrome"})).await;
    assert_eq!(
        c.next().await.to_string(),
        r#"{"action":"muteApp","response":{"error":"Not supported on this OS"}}"#
    );
}

#[tokio::test]
async fn turning_app_tracking_off_hides_the_list_and_refuses_the_actions() {
    let env = env_apps().await;
    let mut c = env.connect().await;
    assert!(c.next().await.get("apps").is_some());

    let mut off = env.server.config();
    off.apps.enabled = false;
    let applied = env.server.apply_config(off.clone()).await.unwrap();
    assert!(!applied.restarted);
    assert_eq!(c.next().await, json!({"apps": null}));
    c.send(json!({"action":"getCapabilities"})).await;
    assert_eq!(
        c.next().await["capabilities"]["apps"],
        json!({"volume":false,"mute":false})
    );
    c.send(json!({"action":"muteApp","app":"chrome"})).await;
    assert_eq!(
        c.next().await["response"]["error"],
        "Not supported on this OS"
    );
    let mut late = env.connect().await;
    assert!(late.next().await.get("apps").is_none());
    assert!(env.calls().is_empty());

    let mut on = off;
    on.apps.enabled = true;
    env.server.apply_config(on).await.unwrap();
    assert_eq!(c.next().await["apps"].to_string(), APPS);
}
