//! Platform audio backends behind one trait. Exactly one is compiled per target OS.

use std::fmt;

#[cfg(target_os = "macos")]
mod coreaudio;
#[cfg(target_os = "linux")]
mod mpris;
#[cfg(target_os = "linux")]
mod pulse;
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod wasapi;
#[cfg(target_os = "linux")]
mod xdotool;

/// Machine-readable error codes sent to clients. See CONTRACT.md.
#[allow(dead_code)] // some codes are only produced by some platform backends
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ParseError,
    InvalidRequest,
    LineTooLong,
    UnknownAction,
    InvalidValue,
    NoDevice,
    /// The named app (or device) does not exist (any more).
    NotFound,
    Unsupported,
    BackendUnavailable,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ParseError => "parse_error",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::LineTooLong => "line_too_long",
            ErrorCode::UnknownAction => "unknown_action",
            ErrorCode::InvalidValue => "invalid_value",
            ErrorCode::NoDevice => "no_device",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::BackendUnavailable => "backend_unavailable",
            ErrorCode::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioError {
    pub code: ErrorCode,
    pub message: String,
}

impl AudioError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AudioError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

pub type AudioResult<T> = Result<T, AudioError>;

/// One output (render) device. `id` is stable and OS-specific; `name` is for humans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub default: bool,
}

/// One audio session (Windows) or stream (Pulse sink-input) of an app. Backends report every
/// session as it is; [`crate::apps::group`] merges the sessions of one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSession {
    /// The app id, from [`crate::apps::app_id`]: the lowercased executable name without `.exe`.
    pub id: String,
    /// Display name; may be empty when the OS has none.
    pub name: String,
    /// Integer percent, 0-100, relative to the master volume.
    pub volume: u8,
    pub muted: bool,
}

/// What the backend can do right now. `volume` and `mute` describe the default output device when
/// there is one, otherwise what the OS API supports in general.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub backend: &'static str,
    /// The OS audio API (or server) answered.
    pub available: bool,
    pub devices: bool,
    pub volume: bool,
    pub mute: bool,
}

impl Capabilities {
    #[allow(dead_code)] // not used by every platform backend
    pub fn none(backend: &'static str) -> Self {
        Capabilities {
            backend,
            available: false,
            devices: false,
            volume: false,
            mute: false,
        }
    }
}

/// Features of the WebSocket protocol v2 (per-app audio, device selection) that this machine
/// supports right now. Windows and Linux (Pulse) report per-app audio; macOS reports none yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    pub devices_list: bool,
    pub devices_select: bool,
    pub apps_volume: bool,
    pub apps_mute: bool,
    /// Play/pause, next and previous track, like the media keys of a keyboard.
    pub media_keys: bool,
    /// F1 to F24, pressed like on a keyboard.
    pub function_keys: bool,
}

/// A media key of a keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKey {
    PlayPause,
    Next,
    Previous,
}

/// Everything operates on the default output device. Volume is an integer percent, 0-100, where
/// 0 is really 0 (not "unchanged", not "quietest step above 0").
pub trait Backend {
    fn capabilities(&mut self) -> Capabilities;
    fn list_output_devices(&mut self) -> AudioResult<Vec<Device>>;
    fn get_volume(&mut self) -> AudioResult<u8>;
    /// Returns the volume read back from the OS after setting it.
    fn set_volume(&mut self, percent: u8) -> AudioResult<u8>;
    fn get_muted(&mut self) -> AudioResult<bool>;
    /// Returns the mute state read back from the OS after setting it.
    fn set_muted(&mut self, muted: bool) -> AudioResult<bool>;
    /// Protocol v2 features available right now (asked on every capability query, because the
    /// answer can change at runtime, e.g. when a Pulse server starts). Default: none.
    fn features(&mut self) -> Features {
        Features::default()
    }
    /// Every audio session of every app, on every active output device. Default: unsupported.
    fn list_app_sessions(&mut self) -> AudioResult<Vec<AppSession>> {
        Err(unsupported_apps())
    }
    /// Set the volume of every session whose `id` matches; `NotFound` when there is none.
    fn set_app_volume(&mut self, _id: &str, _percent: u8) -> AudioResult<()> {
        Err(unsupported_apps())
    }
    /// Mute or unmute every session whose `id` matches; `NotFound` when there is none.
    fn set_app_muted(&mut self, _id: &str, _muted: bool) -> AudioResult<()> {
        Err(unsupported_apps())
    }
    /// Press a media key, with the same effect as on a keyboard: the OS sends it to the media
    /// player it picks. `NotFound` when the OS knows there is no player. Default: unsupported.
    fn press_media_key(&mut self, _key: MediaKey) -> AudioResult<()> {
        Err(unsupported_media_keys())
    }
    /// Press and release the function key F`number` (1-24), like a keyboard: the focused window
    /// gets it. Default: unsupported.
    fn press_function_key(&mut self, _number: u8) -> AudioResult<()> {
        Err(AudioError::new(
            ErrorCode::Unsupported,
            "function keys are not supported on this OS",
        ))
    }
}

/// Default of [`Backend::press_media_key`].
fn unsupported_media_keys() -> AudioError {
    AudioError::new(
        ErrorCode::Unsupported,
        "media keys are not supported on this OS",
    )
}

fn unsupported_apps() -> AudioError {
    AudioError::new(
        ErrorCode::Unsupported,
        "per-app audio is not supported on this OS",
    )
}

/// Starts watching the OS for audio changes made outside this app (another mixer, an app starting
/// or stopping, the default device changing), on a thread of its own, and calls `changed` after
/// each; the caller reads the new state itself. Returns false where the backend has no change
/// events (Windows and macOS for now): polling is then the only way to see such changes.
pub fn watch(changed: Box<dyn Fn() + Send>) -> bool {
    #[cfg(target_os = "linux")]
    return pulse::watch(changed);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = changed;
        false
    }
}

/// The backend for the OS this binary was built for.
pub fn create() -> Box<dyn Backend> {
    #[cfg(target_os = "windows")]
    return Box::new(wasapi::Wasapi::new());
    #[cfg(target_os = "macos")]
    return Box::new(coreaudio::CoreAudio::new());
    #[cfg(target_os = "linux")]
    return Box::new(pulse::Pulse::new());
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    return Box::new(unsupported::Unsupported);
}
