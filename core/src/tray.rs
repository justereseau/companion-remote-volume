//! The app's icon and menu: in the notification area on Windows, in the menu bar on macOS. Only an
//! installed service shows it (a program run in a terminal has its window). Linux has none: desktops
//! disagree on tray icons, and the libraries for them would pull in GTK.
//!
//! The icon has to live on the main thread, which then only runs this loop. The app itself runs on
//! another thread, and everything between the two goes through the closures in [`TrayApp`].

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::log::Log;

/// Whether this OS shows an icon, so that the main thread has to be kept for it.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

/// What the menu shows and does.
pub struct TrayApp {
    pub title: String,
    /// The status line, read again about twice a second.
    pub status: Box<dyn Fn() -> String>,
    pub open_settings: Box<dyn Fn()>,
    pub quit: Box<dyn Fn()>,
    pub log: Arc<dyn Log>,
}

/// The status line for this many connected clients.
pub fn clients_status(clients: usize) -> String {
    match clients {
        0 => "No client connected".to_string(),
        1 => "1 client connected".to_string(),
        n => format!("{n} clients connected"),
    }
}

/// Shows the icon and handles the menu until `stop` is set, then removes the icon.
#[cfg(any(windows, target_os = "macos"))]
pub fn run(app: TrayApp, stop: &AtomicBool) {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    /// Before the taskbar is up at login, an icon cannot be added yet.
    const RETRY_EVERY: Duration = Duration::from_secs(5);
    const ICON_SIZE: u32 = 32;
    const ICON: &[u8] = include_bytes!("../assets/tray-icon-32.rgba");

    let events = platform::Events::new();

    let mut shown_status = (app.status)();
    let title = MenuItem::new(&app.title, false, None);
    let status = MenuItem::new(&shown_status, false, None);
    let open_settings = MenuItem::new("Open settings", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    let items = menu.append_items(&[
        &title,
        &status,
        &PredefinedMenuItem::separator(),
        &open_settings,
        &quit,
    ]);
    if let Err(error) = items {
        app.log.error(&format!("Could not build the menu: {error}"));
        return;
    }

    let mut tray: Option<TrayIcon> = None;
    let mut last_try: Option<Instant> = None;
    let mut reported = false;
    while !stop.load(Ordering::SeqCst) {
        if tray.is_none() && last_try.map_or(true, |at| at.elapsed() >= RETRY_EVERY) {
            last_try = Some(Instant::now());
            let built = Icon::from_rgba(ICON.to_vec(), ICON_SIZE, ICON_SIZE)
                .map_err(|e| e.to_string())
                .and_then(|icon| {
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_tooltip(&app.title)
                        .with_icon(icon)
                        .build()
                        .map_err(|e| e.to_string())
                });
            match built {
                Ok(icon) => tray = Some(icon),
                Err(error) if !reported => {
                    reported = true;
                    app.log
                        .warn(&format!("Could not show the icon yet: {error}"));
                }
                Err(_) => {}
            }
        }

        events.pump();

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id() == open_settings.id() {
                (app.open_settings)();
            } else if event.id() == quit.id() {
                (app.quit)();
            }
        }
        let now = (app.status)();
        if now != shown_status {
            status.set_text(&now);
            shown_status = now;
        }
    }
    drop(tray);
}

/// Never called: see [`SUPPORTED`].
#[cfg(not(any(windows, target_os = "macos")))]
pub fn run(_app: TrayApp, _stop: &AtomicBool) {}

#[cfg(windows)]
mod platform {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, SetTimer, TranslateMessage, MSG,
    };

    /// The thread's window messages, woken by a timer so that the loop can look at its flags too.
    pub struct Events;

    impl Events {
        pub fn new() -> Events {
            unsafe { SetTimer(None, 0, 500, None) };
            Events
        }

        /// Handles one message, waiting for it.
        pub fn pump(&self) {
            let mut message = MSG::default();
            unsafe {
                if GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    /// The app's event queue. Accessory: a menu bar item, with no icon in the Dock.
    pub struct Events(Retained<NSApplication>);

    impl Events {
        pub fn new() -> Events {
            let main = MainThreadMarker::new().expect("the menu must run on the main thread");
            let app = NSApplication::sharedApplication(main);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            app.finishLaunching();
            Events(app)
        }

        /// Handles one event, waiting half a second at most.
        pub fn pump(&self) {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.5);
            let event = unsafe {
                self.0.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            if let Some(event) = event {
                self.0.sendEvent(&event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_counts_clients() {
        assert_eq!(clients_status(0), "No client connected");
        assert_eq!(clients_status(1), "1 client connected");
        assert_eq!(clients_status(3), "3 clients connected");
    }
}
