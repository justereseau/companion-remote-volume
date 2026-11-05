//! Remote Volume: control the system volume and mute from Bitfocus Companion over a WebSocket.
//!
//! One binary does everything: it talks to the OS audio API, serves the WebSocket protocol, and
//! serves a settings page on `http://127.0.0.1:<settings port>/`.

use std::env;
use std::process::{Command, ExitCode};
use std::sync::Arc;

use remote_volume_core::audio::AudioHandle;
use remote_volume_core::cli::{parse_args, Command as Cli, Options, HELP};
use remote_volume_core::config::{config_dir, legacy_candidates, Loaded, Store};
use remote_volume_core::log::{Log, StderrLog};
use remote_volume_core::server::Server;
use remote_volume_core::settings::{SettingsApp, SettingsHttp};

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
        Ok(Cli::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Cli::Version) => {
            println!("remote-volume {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
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
