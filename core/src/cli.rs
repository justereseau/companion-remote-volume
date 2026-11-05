//! Command line of `remote-volume`: options, their defaults and the help text.

use std::path::PathBuf;

pub const DEFAULT_SETTINGS_PORT: u16 = 2502;

pub const HELP: &str = "\
remote-volume - control the system volume from Bitfocus Companion over a WebSocket

Usage: remote-volume [options]

Options:
  --settings-port <port>  Port of the settings page on 127.0.0.1 (default 2502, env RV_SETTINGS_PORT)
  --config-dir <dir>      Where config.json is kept (env RV_CONFIG_DIR)
  --open                  Open the settings page in the default browser
  -h, --help              Show this help
  -V, --version           Show the version
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub settings_port: u16,
    pub config_dir: PathBuf,
    pub open: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
}

/// Parse the arguments after the program name. `env_settings_port` is `RV_SETTINGS_PORT` (an
/// unusable value falls back to the default) and `default_config_dir` what `RV_CONFIG_DIR` or the
/// OS convention gives; flags override both.
pub fn parse_args(
    args: impl IntoIterator<Item = String>,
    env_settings_port: Option<&str>,
    default_config_dir: PathBuf,
) -> Result<Command, String> {
    let mut options = Options {
        settings_port: env_settings_port
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_SETTINGS_PORT),
        config_dir: default_config_dir,
        open: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
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
    Ok(Command::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str], env_port: Option<&str>) -> Result<Command, String> {
        parse_args(
            args.iter().map(|s| s.to_string()),
            env_port,
            PathBuf::from("default-dir"),
        )
    }

    fn run(options: Command) -> Options {
        match options {
            Command::Run(o) => o,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn defaults() {
        let o = run(parse(&[], None).unwrap());
        assert_eq!(o.settings_port, 2502);
        assert_eq!(o.config_dir, PathBuf::from("default-dir"));
        assert!(!o.open);
    }

    #[test]
    fn flags_override_the_defaults_and_the_environment() {
        let o = run(parse(
            &[
                "--open",
                "--settings-port",
                "3000",
                "--config-dir",
                "/tmp/rv",
            ],
            Some("4000"),
        )
        .unwrap());
        assert_eq!(o.settings_port, 3000);
        assert_eq!(o.config_dir, PathBuf::from("/tmp/rv"));
        assert!(o.open);
    }

    #[test]
    fn the_environment_port_is_used_when_valid_and_ignored_otherwise() {
        assert_eq!(run(parse(&[], Some("4000")).unwrap()).settings_port, 4000);
        for bad in ["", "abc", "70000", "-1"] {
            assert_eq!(
                run(parse(&[], Some(bad)).unwrap()).settings_port,
                2502,
                "{bad}"
            );
        }
    }

    #[test]
    fn help_and_version_win_over_what_follows() {
        assert_eq!(parse(&["-h"], None), Ok(Command::Help));
        assert_eq!(parse(&["--version", "--bogus"], None), Ok(Command::Version));
        assert_eq!(parse(&["-V"], None), Ok(Command::Version));
        assert!(HELP.contains("--settings-port") && HELP.contains("--config-dir"));
    }

    #[test]
    fn bad_arguments_are_errors_with_a_reason() {
        assert_eq!(
            parse(&["--bogus"], None),
            Err("unknown option: --bogus".into())
        );
        assert_eq!(
            parse(&["--settings-port"], None),
            Err("--settings-port needs a value".into())
        );
        assert_eq!(
            parse(&["--settings-port", "99999"], None),
            Err("not a port: 99999".into())
        );
        assert_eq!(
            parse(&["--config-dir"], None),
            Err("--config-dir needs a value".into())
        );
    }
}
