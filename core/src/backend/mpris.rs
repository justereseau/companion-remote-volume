//! Linux media keys. Desktops (GNOME, KDE, ...) route the keyboard's media keys to a media player
//! over MPRIS, the D-Bus interface players implement; this does the same through `dbus-send`, which
//! comes with D-Bus itself, so no D-Bus library is linked.
//!
//! The player is picked like desktops do: the one playing, else a paused one, else the first.

use std::process::Command;

use super::{AudioError, AudioResult, ErrorCode, MediaKey};

const PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

/// Whether `dbus-send` can be run at all. A session bus is only needed when a key is pressed.
pub fn available() -> bool {
    Command::new("dbus-send")
        .arg("--help")
        .output()
        .is_ok_and(|out| out.status.success())
}

pub fn press(key: MediaKey) -> AudioResult<()> {
    let players: Vec<(String, Option<String>)> = players()?
        .into_iter()
        .map(|name| {
            let status = playback_status(&name);
            (name, status)
        })
        .collect();
    let player = pick(&players)
        .ok_or_else(|| AudioError::new(ErrorCode::NotFound, "no MPRIS media player is running"))?;
    let method = match key {
        MediaKey::PlayPause => "PlayPause",
        MediaKey::Next => "Next",
        MediaKey::Previous => "Previous",
    };
    dbus(&[
        &format!("--dest={player}"),
        PLAYER_PATH,
        &format!("org.mpris.MediaPlayer2.Player.{method}"),
    ])
    .map(|_| ())
}

/// One `dbus-send` call on the session bus, with its printed reply.
fn dbus(args: &[&str]) -> AudioResult<String> {
    let out = Command::new("dbus-send")
        .args(["--session", "--print-reply", "--reply-timeout=2000"])
        .args(args)
        .output()
        .map_err(|e| {
            AudioError::new(
                ErrorCode::BackendUnavailable,
                format!("cannot run dbus-send: {e}"),
            )
        })?;
    if !out.status.success() {
        return Err(AudioError::new(
            ErrorCode::Internal,
            format!("dbus-send: {}", String::from_utf8_lossy(&out.stderr).trim()),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The bus names of the running MPRIS players.
fn players() -> AudioResult<Vec<String>> {
    let reply = dbus(&[
        "--dest=org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus.ListNames",
    ])?;
    Ok(reply_strings(&reply)
        .into_iter()
        .filter(|name| name.starts_with(PLAYER_PREFIX))
        .collect())
}

/// `Playing`, `Paused` or `Stopped`; `None` when the player does not answer.
fn playback_status(player: &str) -> Option<String> {
    let reply = dbus(&[
        &format!("--dest={player}"),
        PLAYER_PATH,
        "org.freedesktop.DBus.Properties.Get",
        "string:org.mpris.MediaPlayer2.Player",
        "string:PlaybackStatus",
    ])
    .ok()?;
    reply_strings(&reply).into_iter().next()
}

/// The `string "..."` values of a `dbus-send --print-reply` output, in order.
fn reply_strings(reply: &str) -> Vec<String> {
    reply
        .lines()
        .filter_map(|line| {
            let start = line.find("string \"")? + "string \"".len();
            let rest = &line[start..];
            Some(rest.strip_suffix('"').unwrap_or(rest).to_string())
        })
        .collect()
}

/// The player playing, else a paused one, else the first one listed.
fn pick(players: &[(String, Option<String>)]) -> Option<&str> {
    let rank = |status: &Option<String>| match status.as_deref() {
        Some("Playing") => 0,
        Some("Paused") => 1,
        _ => 2,
    };
    players
        .iter()
        .min_by_key(|(_, status)| rank(status))
        .map(|(name, _)| name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_read_from_dbus_send_replies() {
        let names = r#"method return time=1.2 sender=org.freedesktop.DBus -> destination=:1.9 serial=3 reply_serial=2
   array [
      string "org.freedesktop.DBus"
      string ":1.7"
      string "org.mpris.MediaPlayer2.spotify"
      string "org.mpris.MediaPlayer2.firefox.instance_1_42"
   ]
"#;
        assert_eq!(
            reply_strings(names),
            [
                "org.freedesktop.DBus",
                ":1.7",
                "org.mpris.MediaPlayer2.spotify",
                "org.mpris.MediaPlayer2.firefox.instance_1_42"
            ]
        );
        let status = "method return time=1.2 sender=:1.7 -> destination=:1.9 serial=5 reply_serial=2\n   variant       string \"Playing\"\n";
        assert_eq!(reply_strings(status), ["Playing"]);
        assert!(reply_strings("method return time=1.2\n").is_empty());
    }

    #[test]
    fn the_playing_player_wins_then_a_paused_one_then_the_first() {
        let player =
            |name: &str, status: Option<&str>| (name.to_string(), status.map(String::from));
        assert_eq!(pick(&[]), None);
        assert_eq!(
            pick(&[player("a", Some("Stopped")), player("b", None)]),
            Some("a")
        );
        assert_eq!(
            pick(&[player("a", Some("Stopped")), player("b", Some("Paused"))]),
            Some("b")
        );
        assert_eq!(
            pick(&[
                player("a", Some("Paused")),
                player("b", Some("Playing")),
                player("c", Some("Playing"))
            ]),
            Some("b")
        );
    }

    /// Without a player (CI), a key press is `not_found`, or fails when there is no session bus.
    #[test]
    fn a_key_press_without_a_player_is_not_found_or_fails() {
        if std::env::var("RV_INTEGRATION")
            .unwrap_or_default()
            .is_empty()
        {
            eprintln!("skipped: set RV_INTEGRATION to call dbus-send");
            return;
        }
        if !available() {
            eprintln!("skipped: dbus-send is not installed");
            return;
        }
        let error = press(MediaKey::PlayPause).unwrap_err();
        assert!(
            matches!(error.code, ErrorCode::NotFound | ErrorCode::Internal),
            "{error}"
        );
    }
}
