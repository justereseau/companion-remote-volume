//! Linux: libpulse (`libpulse-binding`). Works on PulseAudio and on PipeWire's pulse server.
//!
//! When no server answers, every capability is reported false and commands fail with
//! `backend_unavailable` instead of falling back to ALSA.
//!
//! Spike simplification: each call opens a fresh connection (a few ms). The persistent
//! connection with change subscriptions comes with the events work.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::operation::{Operation, State as OpState};
use libpulse_binding::volume::{ChannelVolumes, Volume};

use super::{AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode};

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
                    name: info.name.as_ref().map(|n| n.to_string()).unwrap_or_default(),
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

    fn confirm(&mut self, accepted: Rc<Cell<bool>>, op: Operation<dyn FnMut(bool)>) -> AudioResult<()> {
        self.wait(op)?;
        if accepted.get() {
            Ok(())
        } else {
            Err(internal("PulseAudio refused the change"))
        }
    }
}

pub struct Pulse;

impl Pulse {
    pub fn new() -> Self {
        Pulse
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
}
