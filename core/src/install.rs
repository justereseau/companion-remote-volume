//! Installing the app for the current user, with no admin rights: one program file in a per-user
//! folder (so an update can replace it) that starts at login with `--service`.
//!
//! - Windows: `%LOCALAPPDATA%\Programs\RemoteVolume`, and a value under the `Run` registry key.
//!   Not a Windows service: services run in a separate session, where key presses reach no desktop.
//! - macOS: `~/Library/Application Support/RemoteVolume`, and a LaunchAgent that keeps it running.
//! - Linux: `~/.local/share/remote-volume`, and a systemd user service that keeps it running.
//!
//! The settings in `config.json` are left alone by installing and by uninstalling.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::update::{self, no_window};

const PROGRAM: &str = if cfg!(windows) {
    "remote-volume.exe"
} else {
    "remote-volume"
};

/// The argument that makes a run the installed service.
pub const SERVICE_FLAG: &str = "--service";

fn env_dir(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Where the installed program lives for this user.
pub fn installed_program() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        env_dir("LOCALAPPDATA")?
            .join("Programs")
            .join("RemoteVolume")
    } else if cfg!(target_os = "macos") {
        env_dir("HOME")?
            .join("Library")
            .join("Application Support")
            .join("RemoteVolume")
    } else {
        env_dir("XDG_DATA_HOME")
            .or_else(|| env_dir("HOME").map(|home| home.join(".local").join("share")))?
            .join("remote-volume")
    };
    Some(dir.join(PROGRAM))
}

/// Whether `program` is the installed program of this user.
pub fn is_installed_program(program: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).ok();
    match installed_program() {
        Some(installed) => {
            canonical(program).is_some() && canonical(program) == canonical(&installed)
        }
        None => false,
    }
}

/// The command in the Windows `Run` key. `conhost --headless` gives the program a console that
/// nobody sees; started directly, it would open a window at every login.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_run_command(program: &Path) -> String {
    format!(
        "conhost.exe --headless \"{}\" {SERVICE_FLAG}",
        program.display()
    )
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_unit(program: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Remote Volume\n\
         \n\
         [Service]\n\
         ExecStart=\"{}\" {SERVICE_FLAG}\n\
         Restart=always\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        program.display()
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_plist(program: &Path) -> String {
    let path = program
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCHD_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{path}</string>
		<string>{SERVICE_FLAG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
</dict>
</plist>
"#
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const LAUNCHD_LABEL: &str = "com.justereseau.remote-volume";

/// Runs a helper command and turns a failure into a message.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let output = no_window(command)
        .output()
        .map_err(|e| format!("could not {what}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not {what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "Remote Volume";

    /// Stops the installed program if it runs, and only that one: not a copy started elsewhere,
    /// and not this process. The path goes through the environment, so no quoting is involved.
    pub fn stop(program: &Path) -> Result<(), String> {
        const SCRIPT: &str = "Get-Process -Name remote-volume -ErrorAction SilentlyContinue | \
            Where-Object { $_.Path -eq $env:RV_PROGRAM -and $_.Id -ne $env:RV_SELF } | \
            ForEach-Object { Stop-Process -Id $_.Id -Force; $_.WaitForExit(5000) | Out-Null }";
        run(
            Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
                .env("RV_PROGRAM", program)
                .env("RV_SELF", std::process::id().to_string()),
            "stop the running app",
        )
    }

    pub fn register(program: &Path) -> Result<(), String> {
        let command = windows_run_command(program);
        run(
            Command::new("reg.exe").args([
                "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f",
            ]),
            "set up the start at login",
        )
    }

    pub fn start(program: &Path) -> Result<(), String> {
        start_detached(program)
    }

    pub fn unregister() -> Result<(), String> {
        // Failing means the value is not there, which is what we want.
        let _ = run(
            Command::new("reg.exe").args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]),
            "remove the start at login",
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    const UNIT: &str = "remote-volume.service";

    fn unit_path() -> Option<PathBuf> {
        let config = env_dir("XDG_CONFIG_HOME")
            .or_else(|| env_dir("HOME").map(|home| home.join(".config")))?;
        Some(config.join("systemd").join("user").join(UNIT))
    }

    fn systemctl(args: &[&str], what: &str) -> Result<(), String> {
        run(Command::new("systemctl").arg("--user").args(args), what)
    }

    pub fn stop(_program: &Path) -> Result<(), String> {
        // Failing means it is not installed yet.
        let _ = systemctl(&["stop", UNIT], "stop the running app");
        Ok(())
    }

    pub fn register(program: &Path) -> Result<(), String> {
        let path = unit_path().ok_or("HOME is not set")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("could not create {dir:?}: {e}"))?;
        }
        fs::write(&path, systemd_unit(program))
            .map_err(|e| format!("could not write {path:?}: {e}"))?;
        systemctl(&["daemon-reload"], "reload systemd")?;
        systemctl(&["enable", UNIT], "set up the start at login")
    }

    pub fn start(_program: &Path) -> Result<(), String> {
        systemctl(&["restart", UNIT], "start the app")
    }

    pub fn unregister() -> Result<(), String> {
        let _ = systemctl(&["disable", UNIT], "remove the start at login");
        if let Some(path) = unit_path() {
            let _ = fs::remove_file(path);
        }
        let _ = systemctl(&["daemon-reload"], "reload systemd");
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn plist_path() -> Option<PathBuf> {
        Some(
            env_dir("HOME")?
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
        )
    }

    fn domain() -> Result<String, String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| format!("could not read the user id: {e}"))?;
        Ok(format!(
            "gui/{}",
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    }

    pub fn stop(_program: &Path) -> Result<(), String> {
        // Failing means it is not loaded.
        let service = format!("{}/{LAUNCHD_LABEL}", domain()?);
        let _ = Command::new("launchctl")
            .args(["bootout", &service])
            .output();
        Ok(())
    }

    pub fn register(program: &Path) -> Result<(), String> {
        let path = plist_path().ok_or("HOME is not set")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("could not create {dir:?}: {e}"))?;
        }
        fs::write(&path, launchd_plist(program))
            .map_err(|e| format!("could not write {path:?}: {e}"))
    }

    pub fn start(_program: &Path) -> Result<(), String> {
        let path = plist_path().ok_or("HOME is not set")?;
        run(
            Command::new("launchctl")
                .arg("bootstrap")
                .arg(domain()?)
                .arg(path),
            "start the app",
        )
    }

    pub fn unregister() -> Result<(), String> {
        if let Some(path) = plist_path() {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    const UNSUPPORTED: &str = "installing is not supported on this OS";

    pub fn stop(_program: &Path) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn register(_program: &Path) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn start(_program: &Path) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn unregister() -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

/// Starts the program as the service, on its own, and returns. Windows only: elsewhere the service
/// manager restarts the app when it exits.
#[cfg(windows)]
fn start_detached(program: &Path) -> Result<(), String> {
    no_window(Command::new(program).arg(SERVICE_FLAG))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start {}: {e}", program.display()))
}

/// After an update: gets the new program running once this process exits.
pub fn restart_service(program: &Path) -> Result<(), String> {
    #[cfg(windows)]
    return start_detached(program);
    #[cfg(not(windows))]
    {
        let _ = program;
        Ok(())
    }
}

/// Copies the program to `target` without ever leaving a half-written one there.
fn copy_program(from: &Path, target: &Path) -> io::Result<()> {
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = target.with_extension("partial");
    fs::copy(from, &partial)?;
    update::make_executable(&partial)?;
    let replaced = fs::rename(&partial, target);
    if replaced.is_err() {
        let _ = fs::remove_file(&partial);
    }
    replaced
}

/// `--install`: this program becomes the installed one and starts, now and at every login.
/// Running it again upgrades or repairs an installation.
pub fn install() -> Result<PathBuf, String> {
    let this = env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let target = installed_program().ok_or("cannot find the per-user program folder")?;
    platform::stop(&target)?;
    if !is_installed_program(&this) {
        copy_program(&this, &target)
            .map_err(|e| format!("could not copy the program to {}: {e}", target.display()))?;
    }
    update::clean_up(&target);
    platform::register(&target)?;
    platform::start(&target)?;
    Ok(target)
}

/// `--uninstall`: stops the app, removes the start at login and the program. Settings stay.
pub fn uninstall() -> Result<(), String> {
    let target = installed_program().ok_or("cannot find the per-user program folder")?;
    platform::unregister()?;
    platform::stop(&target)?;
    update::clean_up(&target);
    // Windows cannot delete a running program: the setup's uninstaller removes the whole folder
    // after this, and when run by hand the file just stays behind, without starting any more.
    let _ = fs::remove_file(&target);
    if let Some(dir) = target.parent() {
        let _ = fs::remove_dir(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_command_quotes_the_path_and_hides_the_console() {
        assert_eq!(
            windows_run_command(Path::new(r"C:\Users\A B\remote-volume.exe")),
            r#"conhost.exe --headless "C:\Users\A B\remote-volume.exe" --service"#
        );
    }

    #[test]
    fn the_systemd_unit_runs_the_service_and_restarts_it() {
        let unit = systemd_unit(Path::new("/home/a b/remote-volume"));
        assert!(unit.contains("ExecStart=\"/home/a b/remote-volume\" --service\n"));
        assert!(unit.contains("Restart=always\n"));
        assert!(unit.contains("WantedBy=default.target\n"));
    }

    #[test]
    fn the_launchd_plist_escapes_the_path_and_keeps_the_app_running() {
        let plist = launchd_plist(Path::new("/Users/a&b/remote-volume"));
        assert!(plist.contains("<string>/Users/a&amp;b/remote-volume</string>"));
        assert!(plist.contains("<string>--service</string>"));
        assert!(plist.contains("<key>KeepAlive</key>\n\t<true/>"));
        assert!(plist.contains("<string>com.justereseau.remote-volume</string>"));
    }

    #[test]
    fn this_test_program_is_not_the_installed_one() {
        let this = env::current_exe().unwrap();
        assert!(!is_installed_program(&this));
        assert!(!is_installed_program(Path::new("does-not-exist")));
    }

    #[test]
    fn a_program_is_copied_whole_and_replaces_the_previous_one() {
        let dir = env::temp_dir().join(format!("rv-install-{}", std::process::id()));
        let source = dir.join("source");
        let target = dir.join("nested").join(PROGRAM);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&source, b"first").unwrap();
        copy_program(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"first");
        fs::write(&source, b"second").unwrap();
        copy_program(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
        assert!(!target.with_extension("partial").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
