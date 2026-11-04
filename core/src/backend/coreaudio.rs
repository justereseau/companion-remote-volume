//! macOS: CoreAudio's AudioObject property API.
//!
//! The spike needs six C functions and a few FourCC constants, so they are declared by hand
//! instead of pulling in a binding crate. Swapping to `objc2-core-audio` later only touches this
//! file.
//!
//! Mute is the weak spot (issue #19): many devices (HDMI, DisplayPort, some USB) expose no mute
//! control at all. Then `capabilities.mute` is false and `getMuted`/`setMuted` fail with
//! `unsupported`.

use std::ffi::{c_char, c_void};
use std::mem::size_of;
use std::ptr;

use super::{AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode};
use crate::volume::{percent_to_scalar, scalar_to_percent};

type OSStatus = i32;
type AudioObjectId = u32;

#[repr(C)]
#[derive(Clone, Copy)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyData(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectSetPropertyData(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: u32,
        data: *const c_void,
    ) -> OSStatus;
    fn AudioObjectGetPropertyDataSize(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: *mut u32,
    ) -> OSStatus;
    fn AudioObjectHasProperty(object: AudioObjectId, address: *const PropertyAddress) -> u8;
    fn AudioObjectIsPropertySettable(
        object: AudioObjectId,
        address: *const PropertyAddress,
        settable: *mut u8,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(object: *const c_void);
}

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const SYSTEM_OBJECT: AudioObjectId = 1;
const UNKNOWN_OBJECT: AudioObjectId = 0;
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const SCOPE_OUTPUT: u32 = fourcc(b"outp");
const ELEMENT_MAIN: u32 = 0;
const UTF8: u32 = 0x0800_0100;

const HW_DEFAULT_OUTPUT_DEVICE: u32 = fourcc(b"dOut");
const HW_DEVICES: u32 = fourcc(b"dev#");
const DEVICE_STREAMS: u32 = fourcc(b"stm#");
const DEVICE_UID: u32 = fourcc(b"uid ");
const OBJECT_NAME: u32 = fourcc(b"lnam");
/// `kAudioHardwareServiceDeviceProperty_VirtualMainVolume`: what the system volume keys control.
const VIRTUAL_MAIN_VOLUME: u32 = fourcc(b"vmvc");
const VOLUME_SCALAR: u32 = fourcc(b"volm");
const MUTE: u32 = fourcc(b"mute");

fn out_addr(selector: u32, element: u32) -> PropertyAddress {
    PropertyAddress {
        selector,
        scope: SCOPE_OUTPUT,
        element,
    }
}

fn global_addr(selector: u32) -> PropertyAddress {
    PropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    }
}

fn status_err(what: &str, status: OSStatus) -> AudioError {
    AudioError::new(
        ErrorCode::Internal,
        format!("{what} failed (OSStatus {status})"),
    )
}

/// Read a fixed-size property. `init` provides the type and the starting value.
fn get<T: Copy>(object: AudioObjectId, address: PropertyAddress, init: T) -> Result<T, OSStatus> {
    let mut value = init;
    let mut size = size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut value as *mut T as *mut c_void,
        )
    };
    if status == 0 {
        Ok(value)
    } else {
        Err(status)
    }
}

fn set<T: Copy>(object: AudioObjectId, address: PropertyAddress, value: T) -> Result<(), OSStatus> {
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            size_of::<T>() as u32,
            &value as *const T as *const c_void,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(status)
    }
}

fn has(object: AudioObjectId, address: PropertyAddress) -> bool {
    unsafe { AudioObjectHasProperty(object, &address) != 0 }
}

fn settable(object: AudioObjectId, address: PropertyAddress) -> bool {
    let mut flag = 0u8;
    let status = unsafe { AudioObjectIsPropertySettable(object, &address, &mut flag) };
    status == 0 && flag != 0
}

fn property_size(object: AudioObjectId, address: PropertyAddress) -> Result<u32, OSStatus> {
    let mut size = 0u32;
    let status =
        unsafe { AudioObjectGetPropertyDataSize(object, &address, 0, ptr::null(), &mut size) };
    if status == 0 {
        Ok(size)
    } else {
        Err(status)
    }
}

/// A CFString-valued property as UTF-8. The returned CFString is owned by us and released here.
fn string_property(object: AudioObjectId, selector: u32) -> Option<String> {
    let cf = get::<*const c_void>(object, global_addr(selector), ptr::null()).ok()?;
    if cf.is_null() {
        return None;
    }
    let mut buffer = vec![0 as c_char; 512];
    let ok = unsafe { CFStringGetCString(cf, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) };
    unsafe { CFRelease(cf) };
    if ok == 0 {
        return None;
    }
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8(bytes).ok()
}

fn default_output() -> AudioResult<AudioObjectId> {
    match get::<AudioObjectId>(
        SYSTEM_OBJECT,
        global_addr(HW_DEFAULT_OUTPUT_DEVICE),
        UNKNOWN_OBJECT,
    ) {
        Ok(device) if device != UNKNOWN_OBJECT => Ok(device),
        Ok(_) => Err(AudioError::new(
            ErrorCode::NoDevice,
            "no default output device",
        )),
        Err(status) => Err(status_err("reading the default output device", status)),
    }
}

fn has_output_streams(device: AudioObjectId) -> bool {
    property_size(device, out_addr(DEVICE_STREAMS, ELEMENT_MAIN)).is_ok_and(|size| size > 0)
}

/// The properties that carry `selector` on this device: the main element if it has one, otherwise
/// the individual channels 1 and 2 that the device lets us set.
fn controls(device: AudioObjectId, selector: u32) -> Vec<PropertyAddress> {
    let main = out_addr(selector, ELEMENT_MAIN);
    if has(device, main) && settable(device, main) {
        return vec![main];
    }
    (1..=2)
        .map(|channel| out_addr(selector, channel))
        .filter(|a| has(device, *a) && settable(device, *a))
        .collect()
}

/// Volume controls: the virtual main volume (what the keyboard keys use) when the device has it.
fn volume_controls(device: AudioObjectId) -> Vec<PropertyAddress> {
    let virtual_main = out_addr(VIRTUAL_MAIN_VOLUME, ELEMENT_MAIN);
    if has(device, virtual_main) {
        return vec![virtual_main];
    }
    controls(device, VOLUME_SCALAR)
}

fn unsupported(what: &str) -> AudioError {
    AudioError::new(
        ErrorCode::Unsupported,
        format!("the default output device has no {what} control"),
    )
}

fn read_volume(device: AudioObjectId, controls: &[PropertyAddress]) -> AudioResult<u8> {
    let mut sum = 0.0f32;
    for address in controls {
        sum +=
            get::<f32>(device, *address, 0.0).map_err(|s| status_err("reading the volume", s))?;
    }
    Ok(scalar_to_percent(sum / controls.len() as f32))
}

fn read_mute(device: AudioObjectId, controls: &[PropertyAddress]) -> AudioResult<bool> {
    let mut all = true;
    for address in controls {
        all &= get::<u32>(device, *address, 0)
            .map_err(|s| status_err("reading the mute state", s))?
            != 0;
    }
    Ok(all)
}

pub struct CoreAudio;

impl CoreAudio {
    pub fn new() -> Self {
        CoreAudio
    }
}

impl Backend for CoreAudio {
    fn capabilities(&mut self) -> Capabilities {
        let (volume, mute) = match default_output() {
            Ok(device) => (
                !volume_controls(device).is_empty(),
                !controls(device, MUTE).is_empty(),
            ),
            // No device right now: the API can do both once there is one.
            Err(_) => (true, true),
        };
        Capabilities {
            backend: "coreaudio",
            available: true,
            devices: true,
            volume,
            mute,
        }
    }

    fn list_output_devices(&mut self) -> AudioResult<Vec<Device>> {
        let size = property_size(SYSTEM_OBJECT, global_addr(HW_DEVICES))
            .map_err(|s| status_err("listing audio devices", s))?;
        let mut ids = vec![UNKNOWN_OBJECT; size as usize / size_of::<AudioObjectId>()];
        let mut bytes = size;
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &global_addr(HW_DEVICES),
                0,
                ptr::null(),
                &mut bytes,
                ids.as_mut_ptr() as *mut c_void,
            )
        };
        if status != 0 {
            return Err(status_err("listing audio devices", status));
        }
        ids.truncate(bytes as usize / size_of::<AudioObjectId>());
        let default = default_output().ok();
        Ok(ids
            .into_iter()
            .filter(|&id| has_output_streams(id))
            .filter_map(|id| {
                let uid = string_property(id, DEVICE_UID)?;
                let name = string_property(id, OBJECT_NAME).unwrap_or_else(|| uid.clone());
                Some(Device {
                    id: uid,
                    name,
                    default: Some(id) == default,
                })
            })
            .collect())
    }

    fn get_volume(&mut self) -> AudioResult<u8> {
        let device = default_output()?;
        let controls = volume_controls(device);
        if controls.is_empty() {
            return Err(unsupported("volume"));
        }
        read_volume(device, &controls)
    }

    fn set_volume(&mut self, percent: u8) -> AudioResult<u8> {
        let device = default_output()?;
        let controls = volume_controls(device);
        if controls.is_empty() {
            return Err(unsupported("volume"));
        }
        for address in &controls {
            set::<f32>(device, *address, percent_to_scalar(percent))
                .map_err(|s| status_err("setting the volume", s))?;
        }
        read_volume(device, &controls)
    }

    fn get_muted(&mut self) -> AudioResult<bool> {
        let device = default_output()?;
        let controls = controls(device, MUTE);
        if controls.is_empty() {
            return Err(unsupported("mute"));
        }
        read_mute(device, &controls)
    }

    fn set_muted(&mut self, muted: bool) -> AudioResult<bool> {
        let device = default_output()?;
        let controls = controls(device, MUTE);
        if controls.is_empty() {
            return Err(unsupported("mute"));
        }
        for address in &controls {
            set::<u32>(device, *address, u32::from(muted))
                .map_err(|s| status_err("setting the mute state", s))?;
        }
        read_mute(device, &controls)
    }
}
