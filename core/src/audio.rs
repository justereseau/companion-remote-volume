//! The audio backend on its own thread, reachable from async code.
//!
//! OS audio APIs are not thread-safe in general (COM apartments on Windows, a single libpulse
//! mainloop on Linux), and calls block. So the backend is created and used on one dedicated thread;
//! everyone else sends it jobs and awaits the answer. Jobs run strictly in order, which also
//! serialises concurrent commands from different clients.

use std::sync::mpsc;
use std::thread;

use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::apps::{self, App};
use crate::backend::{AudioError, AudioResult, Backend, ErrorCode, Features, MediaKey};

type Job = Box<dyn FnOnce(&mut dyn Backend) + Send>;

/// Cheap to clone; every clone talks to the same backend thread. The thread ends when the last
/// handle is dropped.
#[derive(Clone)]
pub struct AudioHandle {
    jobs: mpsc::Sender<Job>,
}

/// Volume, mute and default-device name, read together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioState {
    pub volume: u8,
    pub muted: bool,
    /// Name of the default output device, `None` when there is none.
    pub output_device: Option<String>,
}

impl AudioState {
    /// The v1 `getState` shape: `{ volume, muted, outputDevice }`.
    pub fn to_json(&self) -> Value {
        json!({ "volume": self.volume, "muted": self.muted, "outputDevice": self.output_device })
    }
}

impl AudioHandle {
    /// Start the backend thread. `make` runs on that thread, so the backend never has to be `Send`.
    pub fn spawn(make: impl FnOnce() -> Box<dyn Backend> + Send + 'static) -> AudioHandle {
        let (jobs, queue) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let mut backend = make();
                for job in queue {
                    job(backend.as_mut());
                }
            })
            .expect("failed to start the audio thread");
        AudioHandle { jobs }
    }

    /// The backend for the OS this binary was built for.
    pub fn spawn_default() -> AudioHandle {
        AudioHandle::spawn(crate::backend::create)
    }

    async fn run<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut dyn Backend) -> R + Send + 'static,
    ) -> AudioResult<R> {
        let gone = || AudioError::new(ErrorCode::Internal, "audio thread is not running");
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |backend| {
            let _ = reply.send(f(backend));
        });
        self.jobs.send(job).map_err(|_| gone())?;
        answer.await.map_err(|_| gone())
    }

    pub async fn set_volume(&self, percent: u8) -> AudioResult<u8> {
        self.run(move |b| b.set_volume(percent)).await?
    }

    /// Raise the volume by `amount` percent, stopping at 100. Read and write happen in one job, so
    /// concurrent commands cannot interleave between them.
    pub async fn increase_volume(&self, amount: u8) -> AudioResult<u8> {
        self.run(move |b| {
            let now = b.get_volume()?;
            b.set_volume(now.saturating_add(amount).min(100))
        })
        .await?
    }

    /// Lower the volume by `amount` percent, stopping at 0.
    pub async fn decrease_volume(&self, amount: u8) -> AudioResult<u8> {
        self.run(move |b| {
            let now = b.get_volume()?;
            b.set_volume(now.saturating_sub(amount))
        })
        .await?
    }

    pub async fn is_muted(&self) -> AudioResult<bool> {
        self.run(|b| b.get_muted()).await?
    }

    pub async fn set_muted(&self, muted: bool) -> AudioResult<bool> {
        self.run(move |b| b.set_muted(muted)).await?
    }

    pub async fn toggle_mute(&self) -> AudioResult<bool> {
        self.run(|b| {
            let muted = b.get_muted()?;
            b.set_muted(!muted)
        })
        .await?
    }

    /// Volume, mute state and device name in one pass. Fails if volume or mute cannot be read
    /// (e.g. no output device); a missing device name alone is just `None`.
    pub async fn read_state(&self) -> AudioResult<AudioState> {
        self.run(|b| {
            let volume = b.get_volume()?;
            let muted = b.get_muted()?;
            let output_device = b
                .list_output_devices()
                .ok()
                .and_then(|devices| devices.into_iter().find(|d| d.default))
                .map(|d| d.name);
            Ok(AudioState {
                volume,
                muted,
                output_device,
            })
        })
        .await?
    }

    /// Protocol v2 features available right now.
    pub async fn features(&self) -> AudioResult<Features> {
        self.run(|b| b.features()).await
    }

    /// Every app playing audio, grouped and sorted (see [`crate::apps`]).
    pub async fn list_apps(&self) -> AudioResult<Vec<App>> {
        self.run(|b| b.list_app_sessions().map(apps::group)).await?
    }

    /// Set the volume of every session of the app `target` names.
    pub async fn set_app_volume(&self, target: String, percent: u8) -> AudioResult<()> {
        self.run(move |b| {
            let app = find_app(b, &target)?;
            b.set_app_volume(&app.id, percent)
        })
        .await?
    }

    /// Move an app's volume by `delta` percent from its reported (highest) volume, within 0-100.
    /// Every session ends up at the same level.
    pub async fn change_app_volume(&self, target: String, delta: i16) -> AudioResult<()> {
        self.run(move |b| {
            let app = find_app(b, &target)?;
            let next = (i16::from(app.volume) + delta).clamp(0, 100) as u8;
            b.set_app_volume(&app.id, next)
        })
        .await?
    }

    pub async fn set_app_muted(&self, target: String, muted: bool) -> AudioResult<()> {
        self.run(move |b| {
            let app = find_app(b, &target)?;
            b.set_app_muted(&app.id, muted)
        })
        .await?
    }

    pub async fn press_media_key(&self, key: MediaKey) -> AudioResult<()> {
        self.run(move |b| b.press_media_key(key)).await?
    }

    /// Mute the app unless it is muted (every session muted); then unmute it.
    pub async fn toggle_app_mute(&self, target: String) -> AudioResult<()> {
        self.run(move |b| {
            let app = find_app(b, &target)?;
            b.set_app_muted(&app.id, !app.muted)
        })
        .await?
    }
}

/// The app a client named, looked up fresh: apps come and go, and Pulse stream indexes don't last.
fn find_app(backend: &mut dyn Backend, target: &str) -> AudioResult<App> {
    let apps = apps::group(backend.list_app_sessions()?);
    apps::resolve(&apps, target)
        .cloned()
        .ok_or_else(|| AudioError::new(ErrorCode::NotFound, format!("no app matches {target:?}")))
}
