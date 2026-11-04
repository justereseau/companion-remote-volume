//! Windows: WASAPI through the `windows` crate (IMMDeviceEnumerator + IAudioEndpointVolume).
//!
//! Unlike the old helper exe, `SetMasterVolumeLevelScalar(0.0)` really sets the endpoint to 0.

use std::ffi::c_void;

use windows::core::Error as WinError;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

use super::{AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode};
use crate::volume::{percent_to_scalar, scalar_to_percent};

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

pub struct Wasapi;

impl Wasapi {
    pub fn new() -> Self {
        // The main thread owns every COM call; initialise it once for the process lifetime.
        // S_FALSE (already initialised) is fine; a mode conflict would surface on first use.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Wasapi
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
}
