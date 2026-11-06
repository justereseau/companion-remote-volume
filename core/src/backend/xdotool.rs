//! Linux function keys: a real key press in the X11 session through `xdotool`, like a keyboard.
//!
//! Unlike media keys, a function key has no D-Bus route to an app: it has to be typed. Only X11
//! accepts key presses from another program (`xdotool`, XTest). On a Wayland session they reach only
//! the apps running through XWayland, and only while one of them has the focus.

use std::env;
use std::process::Command;

use super::{AudioError, AudioResult, ErrorCode};

/// Whether there is an X display to type into and `xdotool` runs.
pub fn available() -> bool {
    env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
        && Command::new("xdotool")
            .arg("version")
            .output()
            .is_ok_and(|out| out.status.success())
}

pub fn press_function_key(number: u8) -> AudioResult<()> {
    let out = Command::new("xdotool")
        .args(["key", "--clearmodifiers", &key_name(number)])
        .output()
        .map_err(|e| {
            AudioError::new(
                ErrorCode::BackendUnavailable,
                format!("cannot run xdotool: {e}"),
            )
        })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AudioError::new(
            ErrorCode::Internal,
            format!("xdotool: {}", String::from_utf8_lossy(&out.stderr).trim()),
        ))
    }
}

/// The X keysym name of F`number`, within F1-F24.
fn key_name(number: u8) -> String {
    format!("F{}", number.clamp(1, 24))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_keys_are_x_keysyms() {
        assert_eq!(key_name(1), "F1");
        assert_eq!(key_name(13), "F13");
        assert_eq!(key_name(24), "F24");
        assert_eq!(key_name(30), "F24");
    }

    /// CI has no X display: the capability is off, and a press fails without crashing.
    #[test]
    fn without_a_display_function_keys_are_unavailable() {
        if env::var_os("DISPLAY").is_some() {
            eprintln!("skipped: an X display is set");
            return;
        }
        assert!(!available());
        assert!(press_function_key(13).is_err());
    }
}
