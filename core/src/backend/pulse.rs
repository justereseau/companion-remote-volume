//! Linux: libpulse (`libpulse-binding`). Works on PulseAudio and on PipeWire's pulse server.
//!
//! When no server answers, every capability is reported false and commands fail with
//! `backend_unavailable` instead of falling back to ALSA.
//!
//! Per-app audio: every playback stream (sink-input) belongs to the app named by its
//! `application.process.binary` property, or its `application.name` when there is none.
//!
//! Commands open a fresh connection each (a few ms). Changes made elsewhere are watched on one
//! long-lived connection of their own ([`watch`]), which reconnects when the server restarts.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::subscribe::InterestMaskSet;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::operation::{Operation, State as OpState};
use libpulse_binding::proplist::properties::{APPLICATION_NAME, APPLICATION_PROCESS_BINARY};
use libpulse_binding::volume::{ChannelVolumes, Volume};

use super::{
    AppSession, AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode, Features,
    MediaKey,
};
use crate::apps::app_id;

/// PulseAudio's 100% (`PA_VOLUME_NORM`), the scale `pactl` and pavucontrol show.
const NORM: u64 = 0x10000;

fn percent_to_raw(percent: u8) -> u32 {
    ((u64::from(percent.min(100)) * NORM + 50) / 100) as u32
}

fn raw_to_percent(raw: u32) -> u8 {
    // Sinks can be boosted above 100%; the protocol tops out at 100.
    ((u64::from(raw) * 100 + NORM / 2) / NORM).min(100) as u8
}

fn internal(message: impl Into<String>) -> AudioError {
    AudioError::new(ErrorCode::Internal, message)
}

struct Sink {
    name: String,
    description: String,
    volume: ChannelVolumes,
    mute: bool,
}

/// One playback stream (sink-input) of an app.
struct Stream {
    index: u32,
    id: String,
    name: String,
    volume: ChannelVolumes,
    mute: bool,
}

impl Stream {
    /// With several channels the loudest one is reported (a balance is not a lower volume).
    fn report(&self) -> AppSession {
        AppSession {
            id: self.id.clone(),
            name: self.name.clone(),
            volume: raw_to_percent(self.volume.max().0),
            muted: self.mute,
        }
    }
}

/// The app id and display name of a stream: the id from the process binary, or from the
/// application name when the stream has no binary (some sandboxed or network clients); the name
/// from the application name, or the binary. `None` when the stream has neither.
fn stream_app(binary: Option<&str>, application: Option<&str>) -> Option<(String, String)> {
    let binary = binary.map(str::trim).filter(|b| !b.is_empty());
    let application = application.map(str::trim).filter(|a| !a.is_empty());
    let id = app_id(binary.or(application)?);
    if id.is_empty() {
        return None;
    }
    let name = application.or(binary)?.to_string();
    Some((id, name))
}

/// A connected context. Field order matters: the context must drop before its main loop.
struct Conn {
    context: Context,
    mainloop: Mainloop,
}

impl Conn {
    fn open() -> Result<Conn, String> {
        let mut mainloop = Mainloop::new().ok_or("cannot create the PulseAudio main loop")?;
        let mut context = Context::new(&mainloop, "remote-volume-core")
            .ok_or("cannot create the PulseAudio context")?;
        // NOAUTOSPAWN: never start a sound server just because we asked.
        context
            .connect(None, FlagSet::NOAUTOSPAWN, None)
            .map_err(|e| format!("cannot connect to a PulseAudio server: {e}"))?;
        loop {
            match mainloop.iterate(true) {
                IterateResult::Success(_) => {}
                IterateResult::Quit(_) | IterateResult::Err(_) => {
                    return Err("the PulseAudio main loop stopped".into())
                }
            }
            match context.get_state() {
                State::Ready => return Ok(Conn { context, mainloop }),
                State::Failed | State::Terminated => {
                    return Err("no PulseAudio/PipeWire server answered".into())
                }
                _ => {}
            }
        }
    }

    fn wait<T: ?Sized>(&mut self, op: Operation<T>) -> AudioResult<()> {
        while op.get_state() == OpState::Running {
            match self.mainloop.iterate(true) {
                IterateResult::Success(_) => {}
                IterateResult::Quit(_) | IterateResult::Err(_) => {
                    return Err(internal("the PulseAudio main loop stopped"))
                }
            }
        }
        Ok(())
    }

    fn default_sink_name(&mut self) -> AudioResult<Option<String>> {
        let out = Rc::new(RefCell::new(None::<String>));
        let sink = out.clone();
        let op = self.context.introspect().get_server_info(move |info| {
            *sink.borrow_mut() = info
                .default_sink_name
                .as_ref()
                .map(|n| n.to_string())
                .filter(|n| !n.is_empty());
        });
        self.wait(op)?;
        let name = out.borrow().clone();
        Ok(name)
    }

    fn sinks(&mut self) -> AudioResult<Vec<Sink>> {
        let out = Rc::new(RefCell::new(Vec::<Sink>::new()));
        let failed = Rc::new(Cell::new(false));
        let (sinks, err) = (out.clone(), failed.clone());
        let op = self
            .context
            .introspect()
            .get_sink_info_list(move |result| match result {
                ListResult::Item(info) => sinks.borrow_mut().push(Sink {
                    name: info
                        .name
                        .as_ref()
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    description: info
                        .description
                        .as_ref()
                        .map(|d| d.to_string())
                        .unwrap_or_default(),
                    volume: info.volume,
                    mute: info.mute,
                }),
                ListResult::End => {}
                ListResult::Error => err.set(true),
            });
        self.wait(op)?;
        if failed.get() {
            return Err(internal("PulseAudio failed to list sinks"));
        }
        let list = std::mem::take(&mut *out.borrow_mut());
        Ok(list)
    }

    /// Every playback stream that has a volume and belongs to an app, on every sink.
    fn streams(&mut self) -> AudioResult<Vec<Stream>> {
        let out = Rc::new(RefCell::new(Vec::<Stream>::new()));
        let failed = Rc::new(Cell::new(false));
        let (streams, err) = (out.clone(), failed.clone());
        let op = self
            .context
            .introspect()
            .get_sink_input_info_list(move |result| match result {
                ListResult::Item(info) => {
                    // Passthrough streams (e.g. AC3 to a receiver) have no volume to set.
                    if !info.has_volume {
                        return;
                    }
                    let props = &info.proplist;
                    let Some((id, name)) = stream_app(
                        props.get_str(APPLICATION_PROCESS_BINARY).as_deref(),
                        props.get_str(APPLICATION_NAME).as_deref(),
                    ) else {
                        return;
                    };
                    streams.borrow_mut().push(Stream {
                        index: info.index,
                        id,
                        name,
                        volume: info.volume,
                        mute: info.mute,
                    });
                }
                ListResult::End => {}
                ListResult::Error => err.set(true),
            });
        self.wait(op)?;
        if failed.get() {
            return Err(internal("PulseAudio failed to list the playback streams"));
        }
        let list = std::mem::take(&mut *out.borrow_mut());
        Ok(list)
    }

    /// Applies `change` to every stream of the app `id` (indexes do not last, so they are looked
    /// up again on every command): `not_found` when the app has no stream, an error only when
    /// every change failed (a stream can end between listing and changing it).
    fn change_app(
        &mut self,
        id: &str,
        change: impl Fn(&Context, &Stream, Box<dyn FnMut(bool)>) -> Operation<dyn FnMut(bool)>,
    ) -> AudioResult<()> {
        let streams: Vec<Stream> = self.streams()?.into_iter().filter(|s| s.id == id).collect();
        if streams.is_empty() {
            return Err(AudioError::new(
                ErrorCode::NotFound,
                format!("no playback stream for app {id}"),
            ));
        }
        let mut changed = false;
        for stream in &streams {
            let accepted = Rc::new(Cell::new(false));
            let flag = accepted.clone();
            let op = change(
                &self.context,
                stream,
                Box::new(move |success| flag.set(success)),
            );
            self.wait(op)?;
            changed |= accepted.get();
        }
        if changed {
            Ok(())
        } else {
            Err(internal("PulseAudio refused the change"))
        }
    }

    /// The default sink, or `no_device` when there is none.
    fn default_sink(&mut self) -> AudioResult<Sink> {
        let name = self
            .default_sink_name()?
            .ok_or_else(|| AudioError::new(ErrorCode::NoDevice, "no default output sink"))?;
        self.sinks()?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| AudioError::new(ErrorCode::NoDevice, "default output sink not found"))
    }

    fn confirm(
        &mut self,
        accepted: Rc<Cell<bool>>,
        op: Operation<dyn FnMut(bool)>,
    ) -> AudioResult<()> {
        self.wait(op)?;
        if accepted.get() {
            Ok(())
        } else {
            Err(internal("PulseAudio refused the change"))
        }
    }
}

/// How long the watcher waits before connecting again after losing (or not finding) the server.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// See [`super::watch`]: sinks (volume, mute, plugged in or out), playback streams (apps) and the
/// server (default sink), on a thread with a connection of its own. Without a server it keeps
/// trying, so a server started later (or restarted) is picked up.
pub fn watch(changed: Box<dyn Fn() + Send>) -> bool {
    thread::Builder::new()
        .name("audio-events".into())
        .spawn(move || {
            let changed: Rc<dyn Fn()> = Rc::new(changed);
            loop {
                // Whatever the reason it ended, try again shortly.
                let _ = listen(&changed);
                thread::sleep(RECONNECT_DELAY);
            }
        })
        .is_ok()
}

/// Connects, subscribes and reports changes until the connection ends.
fn listen(changed: &Rc<dyn Fn()>) -> Result<(), String> {
    let mut conn = Conn::open()?;
    // Anything may have changed while not watching (first start, server restart).
    changed();
    let notify = changed.clone();
    conn.context
        .set_subscribe_callback(Some(Box::new(move |_, _, _| notify())));
    let mask = InterestMaskSet::SINK | InterestMaskSet::SINK_INPUT | InterestMaskSet::SERVER;
    let subscribed = Rc::new(Cell::new(false));
    let flag = subscribed.clone();
    let op = conn
        .context
        .subscribe(mask, move |success| flag.set(success));
    conn.wait(op).map_err(|e| e.message)?;
    if !subscribed.get() {
        return Err("PulseAudio refused the subscription".into());
    }
    loop {
        match conn.mainloop.iterate(true) {
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) | IterateResult::Err(_) => {
                return Err("the PulseAudio main loop stopped".into())
            }
        }
        if matches!(conn.context.get_state(), State::Failed | State::Terminated) {
            return Err("the PulseAudio connection ended".into());
        }
    }
}

pub struct Pulse {
    /// Media keys go through `dbus-send` (see [`super::mpris`]); checked once, at start.
    media_keys: bool,
    /// Function keys go through `xdotool` (see [`super::xdotool`]); checked once, at start.
    function_keys: bool,
}

impl Pulse {
    pub fn new() -> Self {
        Pulse {
            media_keys: super::mpris::available(),
            function_keys: super::xdotool::available(),
        }
    }

    fn connect(&self) -> AudioResult<Conn> {
        Conn::open().map_err(|m| AudioError::new(ErrorCode::BackendUnavailable, m))
    }
}

impl Backend for Pulse {
    fn capabilities(&mut self) -> Capabilities {
        match self.connect() {
            Ok(_) => Capabilities {
                backend: "pulse",
                available: true,
                devices: true,
                volume: true,
                mute: true,
            },
            Err(_) => Capabilities::none("pulse"),
        }
    }

    fn list_output_devices(&mut self) -> AudioResult<Vec<Device>> {
        let mut conn = self.connect()?;
        let default = conn.default_sink_name()?;
        Ok(conn
            .sinks()?
            .into_iter()
            .map(|s| Device {
                default: default.as_deref() == Some(s.name.as_str()),
                name: if s.description.is_empty() {
                    s.name.clone()
                } else {
                    s.description
                },
                id: s.name,
            })
            .collect())
    }

    fn get_volume(&mut self) -> AudioResult<u8> {
        let sink = self.connect()?.default_sink()?;
        Ok(raw_to_percent(sink.volume.avg().0))
    }

    fn set_volume(&mut self, percent: u8) -> AudioResult<u8> {
        let mut conn = self.connect()?;
        let sink = conn.default_sink()?;
        let mut volume = sink.volume;
        volume.set(volume.len(), Volume(percent_to_raw(percent)));
        let accepted = Rc::new(Cell::new(false));
        let flag = accepted.clone();
        let op = conn.context.introspect().set_sink_volume_by_name(
            &sink.name,
            &volume,
            Some(Box::new(move |success| flag.set(success))),
        );
        conn.confirm(accepted, op)?;
        Ok(raw_to_percent(conn.default_sink()?.volume.avg().0))
    }

    fn get_muted(&mut self) -> AudioResult<bool> {
        Ok(self.connect()?.default_sink()?.mute)
    }

    fn set_muted(&mut self, muted: bool) -> AudioResult<bool> {
        let mut conn = self.connect()?;
        let sink = conn.default_sink()?;
        let accepted = Rc::new(Cell::new(false));
        let flag = accepted.clone();
        let op = conn.context.introspect().set_sink_mute_by_name(
            &sink.name,
            muted,
            Some(Box::new(move |success| flag.set(success))),
        );
        conn.confirm(accepted, op)?;
        Ok(conn.default_sink()?.mute)
    }

    fn features(&mut self) -> Features {
        // Streams exist on any Pulse server, even with no sink (the list is then empty).
        let available = self.connect().is_ok();
        Features {
            apps_volume: available,
            apps_mute: available,
            media_keys: self.media_keys,
            function_keys: self.function_keys,
            ..Features::default()
        }
    }

    fn press_media_key(&mut self, key: MediaKey) -> AudioResult<()> {
        super::mpris::press(key)
    }

    fn press_function_key(&mut self, number: u8) -> AudioResult<()> {
        super::xdotool::press_function_key(number)
    }

    fn list_app_sessions(&mut self) -> AudioResult<Vec<AppSession>> {
        Ok(self
            .connect()?
            .streams()?
            .iter()
            .map(Stream::report)
            .collect())
    }

    fn set_app_volume(&mut self, id: &str, percent: u8) -> AudioResult<()> {
        let level = Volume(percent_to_raw(percent));
        self.connect()?.change_app(id, |context, stream, done| {
            // Every channel gets the same value, so a set also resets the balance.
            let mut volume = stream.volume;
            volume.set(volume.len(), level);
            context
                .introspect()
                .set_sink_input_volume(stream.index, &volume, Some(done))
        })
    }

    fn set_app_muted(&mut self, id: &str, muted: bool) -> AudioResult<()> {
        self.connect()?.change_app(id, |context, stream, done| {
            context
                .introspect()
                .set_sink_input_mute(stream.index, muted, Some(done))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_round_trips() {
        for p in 0..=100u8 {
            assert_eq!(raw_to_percent(percent_to_raw(p)), p, "{p}%");
        }
    }

    #[test]
    fn zero_is_zero_and_full_is_norm() {
        assert_eq!(percent_to_raw(0), 0);
        assert_eq!(percent_to_raw(100), 0x10000);
    }

    #[test]
    fn boost_is_clamped_to_100() {
        assert_eq!(raw_to_percent(0x18000), 100);
    }

    #[test]
    fn a_stream_is_named_after_its_binary_and_application() {
        let app = |b, a| stream_app(b, a);
        assert_eq!(
            app(Some("firefox"), Some("Firefox")),
            Some(("firefox".into(), "Firefox".into()))
        );
        // No binary (sandboxed or network clients): the application name gives the id too.
        assert_eq!(
            app(None, Some("Spotify")),
            Some(("spotify".into(), "Spotify".into()))
        );
        assert_eq!(
            app(Some("/usr/bin/mpv"), Some(" ")),
            Some(("mpv".into(), "/usr/bin/mpv".into()))
        );
        assert_eq!(app(Some(""), None), None);
        assert_eq!(app(None, None), None);
    }

    /// Plays silence with `pacat` on the default sink (CI: a null sink) and drives its stream.
    #[test]
    fn app_streams_can_be_listed_and_changed() {
        if std::env::var("RV_INTEGRATION")
            .unwrap_or_default()
            .is_empty()
        {
            eprintln!("skipped: set RV_INTEGRATION to play a stream with pacat");
            return;
        }
        struct Player(std::process::Child);
        impl Drop for Player {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _player = Player(
            std::process::Command::new("pacat")
                .args(["--property=application.name=RV Test", "/dev/zero"])
                .spawn()
                .expect("pacat (pulseaudio-utils) plays a test stream"),
        );

        let mut backend = Pulse::new();
        let find = |backend: &mut Pulse| {
            backend
                .list_app_sessions()
                .expect("listing playback streams")
                .into_iter()
                .find(|s| s.id == "pacat")
        };
        let start = std::time::Instant::now();
        let stream = loop {
            if let Some(stream) = find(&mut backend) {
                break stream;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the pacat stream never appeared"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        assert_eq!(stream.name, "RV Test");

        let features = backend.features();
        assert!(features.apps_volume && features.apps_mute);

        backend.set_app_volume("pacat", 37).expect("set volume");
        assert_eq!(find(&mut backend).unwrap().volume, 37);
        backend.set_app_muted("pacat", true).expect("mute");
        assert!(find(&mut backend).unwrap().muted);
        backend.set_app_muted("pacat", false).expect("unmute");
        assert!(!find(&mut backend).unwrap().muted);

        let missing = backend
            .set_app_muted("no-such-app-rv-test", true)
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);

        // Changes are reported: once on connecting, then for a stream volume change.
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        assert!(watch(Box::new(move || {
            let _ = tx.lock().unwrap().send(());
        })));
        let wait = std::time::Duration::from_secs(5);
        rx.recv_timeout(wait).expect("a notification on connecting");
        while rx.try_recv().is_ok() {}
        backend.set_app_volume("pacat", 64).expect("set volume");
        rx.recv_timeout(wait)
            .expect("a notification for the volume change");
    }
}
