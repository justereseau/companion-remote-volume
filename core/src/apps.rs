//! Per-app audio, the OS-independent part: what an "app" is, how sessions are grouped into apps,
//! and how a client's `app` target is matched. Backends only report sessions (see
//! [`crate::backend::AppSession`]); everything here is plain data and unit-tested directly.
//!
//! An **app** is every session of one executable. Its `id` is the lowercased executable name
//! without `.exe` (`spotify`, `chrome`, `discord`), so it stays the same across restarts.
//! - volume: the highest of its sessions
//! - muted: only when every session is muted
//! - a set applies to every session

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::backend::AppSession;

/// One app as clients see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub volume: u8,
    pub muted: bool,
}

impl App {
    /// `{ id, name, volume, muted }`, in that order.
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "volume": self.volume, "muted": self.muted })
    }
}

/// The `apps` state field.
pub fn to_json(apps: &[App]) -> Value {
    Value::Array(apps.iter().map(App::to_json).collect())
}

/// The app id for an executable path or name: the file name, without a trailing `.exe`,
/// lowercased. `C:\Program Files\Spotify\Spotify.exe` and `Spotify` both give `spotify`.
pub fn app_id(executable: &str) -> String {
    let file = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .trim();
    let lower = file.to_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => lower,
    }
}

/// Merge sessions into apps, sorted by id so the list only changes when its content does.
/// Sessions without an id are dropped.
pub fn group(sessions: Vec<AppSession>) -> Vec<App> {
    let mut apps: BTreeMap<String, App> = BTreeMap::new();
    for session in sessions {
        if session.id.is_empty() {
            continue;
        }
        match apps.get_mut(&session.id) {
            Some(app) => {
                app.volume = app.volume.max(session.volume);
                app.muted &= session.muted;
                if app.name.is_empty() {
                    app.name = session.name;
                }
            }
            None => {
                apps.insert(
                    session.id.clone(),
                    App {
                        id: session.id,
                        name: session.name,
                        volume: session.volume,
                        muted: session.muted,
                    },
                );
            }
        }
    }
    apps.into_values()
        .map(|mut app| {
            if app.name.is_empty() {
                app.name = app.id.clone();
            }
            app
        })
        .collect()
}

/// The app a client means: by id (`spotify`, and also `Spotify` or `spotify.exe`), else by
/// display name, ignoring case.
pub fn resolve<'a>(apps: &'a [App], target: &str) -> Option<&'a App> {
    let id = app_id(target);
    apps.iter().find(|a| a.id == id).or_else(|| {
        let name = target.trim().to_lowercase();
        apps.iter().find(|a| a.name.to_lowercase() == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, name: &str, volume: u8, muted: bool) -> AppSession {
        AppSession {
            id: id.into(),
            name: name.into(),
            volume,
            muted,
        }
    }

    #[test]
    fn ids_are_the_lowercased_executable_name() {
        assert_eq!(app_id(r"C:\Program Files\Spotify\Spotify.exe"), "spotify");
        assert_eq!(app_id("/usr/lib/firefox/firefox"), "firefox");
        assert_eq!(app_id("Discord.EXE"), "discord");
        assert_eq!(app_id("spotify"), "spotify");
        assert_eq!(app_id(" Spotify "), "spotify");
        assert_eq!(app_id("my.app.exe"), "my.app");
        assert_eq!(app_id(".exe"), ".exe", "nothing left to strip to");
        assert_eq!(app_id("Ärger.exe"), "ärger");
        assert_eq!(app_id(""), "");
    }

    #[test]
    fn sessions_of_one_app_are_merged() {
        let apps = group(vec![
            session("chrome", "Google Chrome", 30, true),
            session("spotify", "Spotify", 80, false),
            session("chrome", "", 70, false),
            session("chrome", "Chrome tab", 10, true),
        ]);
        assert_eq!(
            apps,
            [
                App {
                    id: "chrome".into(),
                    name: "Google Chrome".into(),
                    volume: 70,
                    muted: false,
                },
                App {
                    id: "spotify".into(),
                    name: "Spotify".into(),
                    volume: 80,
                    muted: false,
                },
            ]
        );
    }

    #[test]
    fn an_app_is_muted_only_when_every_session_is() {
        let apps = group(vec![
            session("discord", "Discord", 50, true),
            session("discord", "Discord", 50, true),
        ]);
        assert!(apps[0].muted);
    }

    #[test]
    fn names_fall_back_to_the_first_non_empty_one_then_the_id() {
        let apps = group(vec![
            session("a", "", 1, false),
            session("a", "Alpha", 1, false),
            session("b", "", 1, false),
        ]);
        assert_eq!(apps[0].name, "Alpha");
        assert_eq!(apps[1].name, "b");
    }

    #[test]
    fn the_order_does_not_depend_on_the_session_order_and_empty_ids_are_dropped() {
        let one = group(vec![
            session("b", "B", 1, false),
            session("a", "A", 2, false),
        ]);
        let two = group(vec![
            session("a", "A", 2, false),
            session("", "ghost", 3, false),
            session("b", "B", 1, false),
        ]);
        assert_eq!(one, two);
        assert_eq!(one[0].id, "a");
    }

    #[test]
    fn targets_match_the_id_in_any_spelling_then_the_name() {
        let apps = group(vec![
            session("spotify", "Spotify", 1, false),
            session("msedge", "Microsoft Edge", 1, false),
        ]);
        for target in ["spotify", "Spotify", "spotify.exe", r"C:\x\Spotify.exe"] {
            assert_eq!(resolve(&apps, target).unwrap().id, "spotify", "{target}");
        }
        assert_eq!(resolve(&apps, "microsoft edge").unwrap().id, "msedge");
        assert!(resolve(&apps, "discord").is_none());
        assert!(resolve(&apps, "spot").is_none(), "no prefix matching");
    }

    #[test]
    fn json_shape_and_key_order() {
        let apps = group(vec![session("spotify", "Spotify", 40, false)]);
        assert_eq!(
            to_json(&apps).to_string(),
            r#"[{"id":"spotify","name":"Spotify","volume":40,"muted":false}]"#
        );
        assert_eq!(to_json(&[]).to_string(), "[]");
    }
}
