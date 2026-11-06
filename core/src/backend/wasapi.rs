//! Windows: WASAPI through the `windows` crate (IMMDeviceEnumerator + IAudioEndpointVolume for
//! the default device, IAudioSessionManager2 + ISimpleAudioVolume for per-app audio).
//!
//! Unlike the old helper exe, `SetMasterVolumeLevelScalar(0.0)` really sets the endpoint to 0.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::{Error as WinError, Interface, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionStateExpired, IAudioSessionControl2, IAudioSessionManager2,
    IMMDevice, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_F1, VK_MEDIA_NEXT_TRACK,
    VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};
use windows::Win32::UI::Shell::SHLoadIndirectString;

use super::{
    AppSession, AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode, Features,
    MediaKey,
};
use crate::apps::app_id;
use crate::volume::{percent_to_scalar, scalar_to_percent};

/// The app id and fallback name of the system sounds session (notifications, alerts).
const SYSTEM_SOUNDS_ID: &str = "system";
const SYSTEM_SOUNDS_NAME: &str = "System Sounds";

/// Passed with every session change we make, so session events can tell our changes from others.
const EVENT_CONTEXT: GUID = GUID::from_u128(0x5f1e_8c2a_7b4d_4e61_9a03_2c6d_8e4f_1b70);

// HRESULTs worth telling apart (as u32 for readability).
const E_NOTFOUND: u32 = 0x8007_0490; // no default endpoint: hosted CI runners, unplugged headset
const AUDCLNT_E_DEVICE_INVALIDATED: u32 = 0x8889_0004;
const AUDCLNT_E_SERVICE_NOT_RUNNING: u32 = 0x8889_0010;
const RPC_S_SERVER_UNAVAILABLE: u32 = 0x8007_06BA;

fn map_err(e: WinError) -> AudioError {
    let hr = e.code().0 as u32;
    let code = match hr {
        E_NOTFOUND | AUDCLNT_E_DEVICE_INVALIDATED => ErrorCode::NoDevice,
        AUDCLNT_E_SERVICE_NOT_RUNNING | RPC_S_SERVER_UNAVAILABLE => ErrorCode::BackendUnavailable,
        _ => ErrorCode::Internal,
    };
    AudioError::new(code, format!("{} (0x{hr:08X})", e.message().trim()))
}

pub struct Wasapi {
    /// Executable path → display name taken from its version resource (or its file name). Reading
    /// the version resource on every poll would be wasteful; paths of audio apps are few.
    names: HashMap<String, String>,
}

impl Wasapi {
    pub fn new() -> Self {
        // The main thread owns every COM call; initialise it once for the process lifetime.
        // S_FALSE (already initialised) is fine; a mode conflict would surface on first use.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Wasapi {
            names: HashMap::new(),
        }
    }

    /// Every live session of every app on every active output device (an app can play on a device
    /// that is not the default). Sessions that cannot be read (the process just exited, a protected
    /// process) are skipped rather than failing the whole list.
    fn sessions(&mut self) -> AudioResult<Vec<Session>> {
        let enumerator = enumerator()?;
        let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
            .map_err(map_err)?;
        let count = unsafe { devices.GetCount() }.map_err(map_err)?;
        let mut sessions = Vec::new();
        for i in 0..count {
            let Ok(device) = (unsafe { devices.Item(i) }) else {
                continue;
            };
            let Ok(manager) =
                (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) })
            else {
                continue;
            };
            let Ok(list) = (unsafe { manager.GetSessionEnumerator() }) else {
                continue;
            };
            let n = unsafe { list.GetCount() }.unwrap_or(0);
            for j in 0..n {
                let Ok(control) = (unsafe { list.GetSession(j) }) else {
                    continue;
                };
                if let Some(session) = unsafe { self.read_session(control) } {
                    sessions.push(session);
                }
            }
        }
        Ok(sessions)
    }

    unsafe fn read_session(
        &mut self,
        control: windows::Win32::Media::Audio::IAudioSessionControl,
    ) -> Option<Session> {
        if control.GetState().ok()? == AudioSessionStateExpired {
            return None;
        }
        let control: IAudioSessionControl2 = control.cast().ok()?;
        // S_OK means "this is the system sounds session"; S_FALSE means it is not. It belongs to
        // no single process, so it gets a fixed id and the name the Windows mixer shows.
        if control.IsSystemSoundsSession() == HRESULT(0) {
            let volume: ISimpleAudioVolume = control.cast().ok()?;
            let name = pick_name(
                session_display_name(&control).as_deref(),
                SYSTEM_SOUNDS_NAME,
            );
            return Some(Session {
                id: SYSTEM_SOUNDS_ID.into(),
                name,
                volume,
            });
        }
        let pid = control.GetProcessId().ok()?;
        if pid == 0 {
            return None;
        }
        let path = process_image(pid)?;
        let id = app_id(&path);
        if id.is_empty() {
            return None;
        }
        let volume: ISimpleAudioVolume = control.cast().ok()?;
        let fallback = self
            .names
            .entry(path)
            .or_insert_with_key(|path| file_description(path).unwrap_or_else(|| exe_stem(path)))
            .clone();
        let name = pick_name(session_display_name(&control).as_deref(), &fallback);
        Some(Session { id, name, volume })
    }
}

/// One session, with what is needed to report it and to change it.
struct Session {
    id: String,
    name: String,
    volume: ISimpleAudioVolume,
}

impl Session {
    fn report(&self) -> Option<AppSession> {
        let level = unsafe { self.volume.GetMasterVolume() }.ok()?;
        let muted = unsafe { self.volume.GetMute() }.ok()?;
        Some(AppSession {
            id: self.id.clone(),
            name: self.name.clone(),
            volume: scalar_to_percent(level),
            muted: muted.as_bool(),
        })
    }
}

/// Applies `change` to every session of the app `id`: `NotFound` when it has none, the error when
/// every change failed (a session can vanish between listing and changing it).
fn change_app(
    sessions: Vec<Session>,
    id: &str,
    change: impl Fn(&ISimpleAudioVolume) -> windows::core::Result<()>,
) -> AudioResult<()> {
    let mut matched = false;
    let mut changed = false;
    let mut error = None;
    for session in sessions.iter().filter(|s| s.id == id) {
        matched = true;
        match change(&session.volume) {
            Ok(()) => changed = true,
            Err(e) => error = Some(map_err(e)),
        }
    }
    match (matched, changed, error) {
        (false, _, _) => Err(AudioError::new(
            ErrorCode::NotFound,
            format!("no audio session for app {id}"),
        )),
        (true, false, Some(e)) => Err(e),
        _ => Ok(()),
    }
}

/// The full path of a process's executable, if this (non-elevated) process may read it.
unsafe fn process_image(pid: u32) -> Option<String> {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let result = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        PWSTR(buffer.as_mut_ptr()),
        &mut size,
    );
    let _ = CloseHandle(handle);
    result.ok()?;
    Some(String::from_utf16_lossy(&buffer[..size as usize]))
}

/// The session's own display name. Many apps leave it empty; some (the system sounds session)
/// set an indirect resource string (`@%SystemRoot%\...,-123`), resolved here in the user's
/// language. An unresolved one is returned as is and rejected by [`pick_name`].
unsafe fn session_display_name(control: &IAudioSessionControl2) -> Option<String> {
    let raw = control.GetDisplayName().ok()?;
    if raw.is_null() {
        return None;
    }
    let name = raw.to_string().ok();
    CoTaskMemFree(Some(raw.0 as *const c_void));
    let name = name?;
    if name.trim_start().starts_with('@') {
        return Some(indirect_string(name.trim()).unwrap_or(name));
    }
    Some(name)
}

/// Loads an indirect resource string such as `@%SystemRoot%\System32\AudioSrv.Dll,-202`.
unsafe fn indirect_string(source: &str) -> Option<String> {
    let wide: Vec<u16> = source.encode_utf16().chain(Some(0)).collect();
    let mut buffer = [0u16; 512];
    SHLoadIndirectString(PCWSTR(wide.as_ptr()), &mut buffer, None).ok()?;
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..end]).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The `FileDescription` of an executable's version resource: `Google Chrome` for chrome.exe.
unsafe fn file_description(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let file = PCWSTR(wide.as_ptr());
    let size = GetFileVersionInfoSizeW(file, None);
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    GetFileVersionInfoW(file, None, size, data.as_mut_ptr().cast()).ok()?;
    let block = data.as_ptr().cast::<c_void>();

    // The first language/code page the resource declares, then US English, Unicode.
    let mut translations = Vec::new();
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    let key: Vec<u16> = "\\VarFileInfo\\Translation"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    if VerQueryValueW(block, PCWSTR(key.as_ptr()), &mut ptr, &mut len).as_bool() && len >= 4 {
        let pair = std::slice::from_raw_parts(ptr.cast::<u16>(), 2);
        translations.push(format!("{:04x}{:04x}", pair[0], pair[1]));
    }
    translations.push("040904b0".into());

    for translation in translations {
        let key: Vec<u16> = format!("\\StringFileInfo\\{translation}\\FileDescription")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        if VerQueryValueW(block, PCWSTR(key.as_ptr()), &mut ptr, &mut len).as_bool() && len > 0 {
            let text = std::slice::from_raw_parts(ptr.cast::<u16>(), len as usize);
            let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            let description = String::from_utf16_lossy(&text[..end]).trim().to_string();
            if !description.is_empty() {
                return Some(description);
            }
        }
    }
    None
}

/// The file name without `.exe`, case kept: `Spotify` for `C:\...\Spotify.exe`.
fn exe_stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match file.len().checked_sub(4) {
        Some(cut) if file[cut..].eq_ignore_ascii_case(".exe") && cut > 0 => file[..cut].to_string(),
        _ => file.to_string(),
    }
}

/// The session's display name when it has a usable one, otherwise the executable's.
fn pick_name(session: Option<&str>, executable: &str) -> String {
    match session.map(str::trim) {
        Some(name) if !name.is_empty() && !name.starts_with('@') => name.to_string(),
        _ => executable.to_string(),
    }
}

/// Presses and releases a key, like a keyboard does: Windows sends media keys to the player that
/// handles them (the one shown in the volume flyout), other keys to the focused window or a global
/// hotkey. Media keys are extended keys. Windows silently drops input sent while an elevated window
/// has the focus.
fn press_key(key: VIRTUAL_KEY, extended: bool) -> AudioResult<()> {
    let base = if extended {
        KEYEVENTF_EXTENDEDKEY
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    let input = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: base | flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [input(KEYBD_EVENT_FLAGS(0)), input(KEYEVENTF_KEYUP)];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(map_err(WinError::from_thread()))
    }
}

fn enumerator() -> AudioResult<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(map_err)
}

/// Re-resolved on every call: the default device can change at any time.
fn default_endpoint() -> AudioResult<IAudioEndpointVolume> {
    let device =
        unsafe { enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole) }.map_err(map_err)?;
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }.map_err(map_err)
}

unsafe fn device_id(device: &IMMDevice) -> AudioResult<String> {
    let raw = device.GetId().map_err(map_err)?;
    let id = raw.to_string();
    CoTaskMemFree(Some(raw.0 as *const c_void));
    id.map_err(|e| AudioError::new(ErrorCode::Internal, format!("device id is not UTF-16: {e}")))
}

unsafe fn device_name(device: &IMMDevice) -> AudioResult<String> {
    let store = device.OpenPropertyStore(STGM_READ).map_err(map_err)?;
    let value = store.GetValue(&PKEY_Device_FriendlyName).map_err(map_err)?;
    let raw = PropVariantToStringAlloc(&value).map_err(map_err)?;
    let name = raw.to_string();
    CoTaskMemFree(Some(raw.0 as *const c_void));
    name.map_err(|e| {
        AudioError::new(
            ErrorCode::Internal,
            format!("device name is not UTF-16: {e}"),
        )
    })
}

impl Backend for Wasapi {
    fn capabilities(&mut self) -> Capabilities {
        match enumerator() {
            Ok(_) => Capabilities {
                backend: "wasapi",
                available: true,
                devices: true,
                volume: true,
                mute: true,
            },
            Err(_) => Capabilities::none("wasapi"),
        }
    }

    fn list_output_devices(&mut self) -> AudioResult<Vec<Device>> {
        let enumerator = enumerator()?;
        let default_id = match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
            Ok(device) => Some(unsafe { device_id(&device)? }),
            Err(e) if map_err(e.clone()).code == ErrorCode::NoDevice => None,
            Err(e) => return Err(map_err(e)),
        };
        let collection = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
            .map_err(map_err)?;
        let count = unsafe { collection.GetCount() }.map_err(map_err)?;
        let mut devices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let device = unsafe { collection.Item(i) }.map_err(map_err)?;
            let id = unsafe { device_id(&device)? };
            let name = unsafe { device_name(&device)? };
            let default = default_id.as_deref() == Some(id.as_str());
            devices.push(Device { id, name, default });
        }
        Ok(devices)
    }

    fn get_volume(&mut self) -> AudioResult<u8> {
        let scalar =
            unsafe { default_endpoint()?.GetMasterVolumeLevelScalar() }.map_err(map_err)?;
        Ok(scalar_to_percent(scalar))
    }

    fn set_volume(&mut self, percent: u8) -> AudioResult<u8> {
        let endpoint = default_endpoint()?;
        unsafe {
            endpoint.SetMasterVolumeLevelScalar(percent_to_scalar(percent), std::ptr::null())
        }
        .map_err(map_err)?;
        let scalar = unsafe { endpoint.GetMasterVolumeLevelScalar() }.map_err(map_err)?;
        Ok(scalar_to_percent(scalar))
    }

    fn get_muted(&mut self) -> AudioResult<bool> {
        let muted = unsafe { default_endpoint()?.GetMute() }.map_err(map_err)?;
        Ok(muted.as_bool())
    }

    fn set_muted(&mut self, muted: bool) -> AudioResult<bool> {
        let endpoint = default_endpoint()?;
        unsafe { endpoint.SetMute(muted, std::ptr::null()) }.map_err(map_err)?;
        let now = unsafe { endpoint.GetMute() }.map_err(map_err)?;
        Ok(now.as_bool())
    }

    fn features(&mut self) -> Features {
        // Audio sessions exist on every Windows with the audio service running, even without an
        // output device (the list is then empty).
        let available = enumerator().is_ok();
        Features {
            apps_volume: available,
            apps_mute: available,
            // Key presses go to the user's session, whatever the audio service does.
            media_keys: true,
            function_keys: true,
            ..Features::default()
        }
    }

    fn press_function_key(&mut self, number: u8) -> AudioResult<()> {
        // VK_F1 to VK_F24 are consecutive.
        press_key(
            VIRTUAL_KEY(VK_F1.0 + u16::from(number.clamp(1, 24)) - 1),
            false,
        )
    }

    fn press_media_key(&mut self, key: MediaKey) -> AudioResult<()> {
        press_key(
            match key {
                MediaKey::PlayPause => VK_MEDIA_PLAY_PAUSE,
                MediaKey::Next => VK_MEDIA_NEXT_TRACK,
                MediaKey::Previous => VK_MEDIA_PREV_TRACK,
            },
            true,
        )
    }

    fn list_app_sessions(&mut self) -> AudioResult<Vec<AppSession>> {
        Ok(self
            .sessions()?
            .iter()
            .filter_map(Session::report)
            .collect())
    }

    fn set_app_volume(&mut self, id: &str, percent: u8) -> AudioResult<()> {
        let level = percent_to_scalar(percent);
        change_app(self.sessions()?, id, |volume| unsafe {
            volume.SetMasterVolume(level, &EVENT_CONTEXT)
        })
    }

    fn set_app_muted(&mut self, id: &str, muted: bool) -> AudioResult<()> {
        change_app(self.sessions()?, id, |volume| unsafe {
            volume.SetMute(muted, &EVENT_CONTEXT)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_names_keep_their_case_without_exe() {
        assert_eq!(
            exe_stem(r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe"),
            "Spotify"
        );
        assert_eq!(exe_stem(r"C:\Program Files\VLC\vlc.EXE"), "vlc");
        assert_eq!(exe_stem("/odd/path/tool"), "tool");
        assert_eq!(exe_stem(".exe"), ".exe");
    }

    #[test]
    fn a_session_name_is_used_unless_empty_or_an_indirect_resource() {
        assert_eq!(pick_name(Some("Spotify"), "Spotify.exe"), "Spotify");
        assert_eq!(pick_name(Some("  "), "Google Chrome"), "Google Chrome");
        assert_eq!(
            pick_name(Some("@%SystemRoot%\\System32\\AudioSrv.Dll,-202"), "Host"),
            "Host"
        );
        assert_eq!(pick_name(None, "Discord"), "Discord");
    }

    #[test]
    fn the_version_resource_names_an_executable() {
        // Every Windows ships notepad.exe with a FileDescription.
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let description = unsafe { file_description(&format!(r"{windir}\System32\notepad.exe")) };
        assert!(description.is_some_and(|d| !d.is_empty()));
        assert_eq!(unsafe { file_description(r"C:\does\not\exist.exe") }, None);
    }

    #[test]
    fn indirect_resource_strings_are_resolved() {
        // The name the system sounds session points to, in the user's language.
        let name = unsafe { indirect_string(r"@%SystemRoot%\System32\AudioSrv.Dll,-202") };
        assert!(name.is_some_and(|n| !n.is_empty() && !n.starts_with('@')));
        assert_eq!(
            unsafe { indirect_string(r"@C:\does\not\exist.dll,-1") },
            None
        );
    }

    /// Talks to the real audio service: an empty list on a machine without output devices.
    #[test]
    fn app_sessions_can_be_listed() {
        if std::env::var("RV_INTEGRATION")
            .unwrap_or_default()
            .is_empty()
        {
            eprintln!("skipped: set RV_INTEGRATION to list the real audio sessions");
            return;
        }
        let mut backend = Wasapi::new();
        let sessions = backend.list_app_sessions().expect("listing audio sessions");
        for session in &sessions {
            assert!(
                !session.id.is_empty() && !session.name.is_empty(),
                "{session:?}"
            );
            assert!(session.volume <= 100);
        }
        let missing = backend
            .set_app_muted("no-such-app-rv-test", true)
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
    }
}
