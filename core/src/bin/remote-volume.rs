//! Remote Volume: control the system volume and mute from Bitfocus Companion over a WebSocket.
//!
//! One binary does everything: it talks to the OS audio API, serves the WebSocket protocol, and
//! serves a settings page on `http://127.0.0.1:<settings port>/`.

use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;

use remote_volume_core::audio::AudioHandle;
use remote_volume_core::config::{config_dir, legacy_candidates, Loaded, Store};
use remote_volume_core::log::{Log, StderrLog};
use remote_volume_core::server::Server;
use remote_volume_core::settings::{SettingsApp, SettingsHttp};

const DEFAULT_SETTINGS_PORT: u16 = 2502;

const HELP: &str = "\
remote-volume - control the system volume from Bitfocus Companion over a WebSocket

Usage: remote-volume [options]

Options:
  --settings-port <port>  Port of the settings page on 127.0.0.1 (default 2502, env RV_SETTINGS_PORT)
  --config-dir <dir>      Where config.json is kept (env RV_CONFIG_DIR)
  --open                  Open the settings page in the default browser
  -h, --help              Show this help
  -V, --version           Show the version
";

struct Options {
    settings_port: u16,
    config_dir: PathBuf,
    open: bool,
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut options = Options {
        settings_port: env::var("RV_SETTINGS_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_SETTINGS_PORT),
        config_dir: config_dir(),
        open: false,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("remote-volume {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--open" => options.open = true,
            "--settings-port" => {
                let value = args.next().ok_or("--settings-port needs a value")?;
                options.settings_port =
                    value.parse().map_err(|_| format!("not a port: {value}"))?;
            }
            "--config-dir" => {
                options.config_dir = args.next().ok_or("--config-dir needs a value")?.into()
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(Some(options))
}

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
    let options = match parse_args() {
        Ok(Some(options)) => options,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("remote-volume: {message}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("ERROR {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(options: Options) -> Result<(), String> {
    let log: Arc<dyn Log> = Arc::new(StderrLog);
    log.info(&format!(
        "Remote Volume {} is starting...",
        env!("CARGO_PKG_VERSION")
    ));

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
        let address = server.start().await.map_err(|e| {
            format!(
                "could not listen on {}:{}: {e}",
                config.websocket.host, config.websocket.port
            )
        })?;
        log.info(&format!(
            "Companion connects to ws://<this computer>:{}",
            address.port()
        ));

        let app = Arc::new(SettingsApp::new(server.clone(), store, config, log.clone()));
        let settings = SettingsHttp::start(
            app,
            options.settings_port,
            tokio::runtime::Handle::current(),
        )
        .map_err(|e| {
            format!(
                "could not serve the settings page on 127.0.0.1:{}: {e}",
                options.settings_port
            )
        })?;
        log.info(&format!("Settings: {}", settings.url()));
        if options.open {
            open_in_browser(&settings.url());
        }

        tokio::signal::ctrl_c()
            .await
            .map_err(|e| format!("could not wait for Ctrl+C: {e}"))?;
        log.info("Shutting down.");
        settings.stop();
        server.stop().await;
        Ok(())
    })
}
