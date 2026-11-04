//! Placeholder for operating systems without a backend: every capability is false.

use super::{AudioError, AudioResult, Backend, Capabilities, Device, ErrorCode};

pub struct Unsupported;

fn unsupported<T>() -> AudioResult<T> {
    Err(AudioError::new(
        ErrorCode::Unsupported,
        format!("no audio backend for {}", std::env::consts::OS),
    ))
}

impl Backend for Unsupported {
    fn capabilities(&mut self) -> Capabilities {
        Capabilities::none("none")
    }
    fn list_output_devices(&mut self) -> AudioResult<Vec<Device>> {
        unsupported()
    }
    fn get_volume(&mut self) -> AudioResult<u8> {
        unsupported()
    }
    fn set_volume(&mut self, _percent: u8) -> AudioResult<u8> {
        unsupported()
    }
    fn get_muted(&mut self) -> AudioResult<bool> {
        unsupported()
    }
    fn set_muted(&mut self, _muted: bool) -> AudioResult<bool> {
        unsupported()
    }
}
