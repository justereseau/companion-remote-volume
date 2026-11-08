//! Remote Volume: control the system volume and mute from Bitfocus Companion over a WebSocket.
//!
//! One binary does everything: it talks to the OS audio API, serves the WebSocket protocol, and
//! serves a settings page on `http://127.0.0.1:<settings port>/`.

use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::Duration;

use remote_volume_core::audio::AudioHandle;
use remote_volume_core::backend;
use remote_volume_core::cli::{parse_args, Command as Cli, Options, HELP};
use remote_volume_core::config::{config_dir, legacy_candidates, Loaded, Store};
use remote_volume_core::install;
use remote_volume_core::log::{FileLog, Log, StderrLog};
use remote_volume_core::server::Server;
use remote_volume_core::settings::{SettingsApp, SettingsHttp};
use remote_volume_core::update::{self, UpdateHandle, Updater};
use remote_volume_core::VERSION;
use tokio::sync::Notify;

const BIND_ATTEMPTS: u32 = 30;
const BIND_RETRY_DELAY: Duration = Duration::from_secs(1);

fn open_in_browser(url: &str) {
    let result = if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(error) = result {
        eprintln!("WARN  could not open the browser: {error}");
    }
}

fn main() -> ExitCode {
    let parsed = parse_args(
        env::args().skip(1),
        env::var("RV_SETTINGS_PORT").ok().as_deref(),
        config_dir(),
    );
    let options = match parsed {
        Ok(Cli::Run(options)) => options,
        Ok(Cli::Install) => {
            return report(install::install().map(|program| {
                println!("Remote Volume is installed in {}", program.display());
                println!("It runs now and starts at every login. Settings: http://127.0.0.1:2502/");
            }))
        }
        Ok(Cli::Uninstall) => {
            return report(install::uninstall().map(|()| println!("Remote Volume is removed.")))
        }
        Ok(Cli::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Version) => {
            println!("remote-volume {}", VERSION);
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("remote-volume: {message}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    report(run(options))
}

fn report(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("ERROR {message}");
            ExitCode::FAILURE
        }
    }
}

/// The program to keep up to date: this one, when it runs as the installed service of a release.
fn managed_program(service: bool) -> Option<PathBuf> {
    let program = env::current_exe().ok()?;
    (service && install::is_installed_program(&program)).then_some(program)
}

/// Starts the update loop. `restart` is signalled when a new program is in place.
fn start_updates(
    program: PathBuf,
    server: &Server,
    restart: Arc<Notify>,
    log: &Arc<dyn Log>,
) -> Option<UpdateHandle> {
    update::clean_up(&program);
    let updater = Updater::for_this_program(program, log.clone())?;
    let settings = server.clone();
    let enabled = move || settings.config().updates.enabled;
    match update::spawn(updater, enabled, move || restart.notify_one()) {
        Ok(handle) => Some(handle),
        Err(error) => {
            log.error(&format!("Could not start the updates: {error}"));
            None
        }
    }
}

fn run(options: Options) -> Result<(), String> {
    let log: Arc<dyn Log> = if options.service {
        // No terminal to read: without a file there would be no trace of what happened.
        let path = options.config_dir.join("remote-volume.log");
        Arc::new(FileLog::open(&path).map_err(|e| format!("could not open {path:?}: {e}"))?)
    } else {
        Arc::new(StderrLog)
    };
    log.info(&format!("Remote Volume {} is starting...", VERSION));

    let store = Store::new(&options.config_dir);
    let (config, loaded) = store.load(&legacy_candidates());
    match &loaded {
        Loaded::File => log.info(&format!("Configuration: {}", store.path().display())),
        Loaded::Imported(from) => {
            log.info(&format!(
                "Imported the settings of the Electron app from {}",
                from.display()
            ));
            store
                .save(&config)
                .map_err(|e| format!("could not write {}: {e}", store.path().display()))?;
        }
        Loaded::Defaults => log.info("No configuration yet, using the defaults."),
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("could not start the async runtime: {e}"))?;

    runtime.block_on(async {
        let server = Server::new(AudioHandle::spawn_default(), config.clone(), log.clone());
        // A service retries: the previous version may still be exiting after an update.
        let attempts = if options.service { BIND_ATTEMPTS } else { 1 };
        let mut attempt = 1;
        let address = loop {
            match server.start().await {
                Ok(address) => break address,
                Err(_) if attempt < attempts => {
                    attempt += 1;
                    tokio::time::sleep(BIND_RETRY_DELAY).await;
                }
                Err(e) => {
                    return Err(format!(
                        "could not listen on {}:{}: {e}",
                        config.websocket.host, config.websocket.port
                    ))
                }
            }
        };
        log.info(&format!(
            "Companion connects to ws://<this computer>:{}",
            address.port()
        ));
        if backend::watch(Box::new(server.change_notifier())) {
            log.info("Watching for volume changes made in other apps.");
        }

        let restart = Arc::new(Notify::new());
        let program = managed_program(options.service);
        let mut app = SettingsApp::new(server.clone(), store, config, log.clone());
        if let Some(program) = program.clone() {
            if let Some(updates) = start_updates(program, &server, restart.clone(), &log) {
                app = app.with_updates(updates);
            }
        }
        let app = Arc::new(app);
        let mut attempt = 1;
        let settings = loop {
            match SettingsHttp::start(
                app.clone(),
                options.settings_port,
                tokio::runtime::Handle::current(),
            ) {
                Ok(settings) => break settings,
                Err(_) if attempt < attempts => {
                    attempt += 1;
                    tokio::time::sleep(BIND_RETRY_DELAY).await;
                }
                Err(e) => {
                    return Err(format!(
                        "could not serve the settings page on 127.0.0.1:{}: {e}",
                        options.settings_port
                    ))
                }
            }
        };
        log.info(&format!("Settings: {}", settings.url()));
        if options.open {
            open_in_browser(&settings.url());
        }

        let updated = tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|e| format!("could not wait for Ctrl+C: {e}"))?;
                false
            }
            () = restart.notified() => true,
        };
        log.info("Shutting down.");
        settings.stop();
        server.stop().await;
        if updated {
            if let Some(program) = &program {
                install::restart_service(program)?;
            }
        }
        Ok(())
    })
}
