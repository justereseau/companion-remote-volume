//! Replacing the program file of a real, running app: what an update does, on the OS at hand.

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use remote_volume_core::update::{check_program, clean_up, replace_program, Version};

/// A folder holding a copy of the app, removed at the end of the test.
struct Installed {
    dir: PathBuf,
    program: PathBuf,
}

impl Installed {
    fn new(name: &str) -> Installed {
        let dir =
            std::env::temp_dir().join(format!("rv-self-update-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let program = dir.join(format!("remote-volume{}", std::env::consts::EXE_SUFFIX));
        fs::copy(env!("CARGO_BIN_EXE_remote-volume"), &program).unwrap();
        Installed { dir, program }
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A port nothing listens on right now. Port 0 will not do: the config clamps it to port 1, which
/// needs admin rights on Linux and macOS.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Runs the copy with a config of its own, on free ports and on the loopback only.
struct Running(Child);

impl Running {
    fn start(program: &Path, config_dir: &Path) -> Running {
        fs::write(
            config_dir.join("config.json"),
            format!(
                r#"{{"websocket":{{"port":{},"host":"127.0.0.1"}}}}"#,
                free_port()
            ),
        )
        .unwrap();
        let child = Command::new(program)
            .args(["--settings-port", "0", "--config-dir"])
            .arg(config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Running(child)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn this_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
}

#[test]
fn a_running_program_can_be_replaced_and_the_old_file_cleaned_up_after_it_exits() {
    let installed = Installed::new("swap");
    let mut running = Running::start(&installed.program, &installed.dir);
    thread::sleep(Duration::from_millis(300));
    assert!(
        running.0.try_wait().unwrap().is_none(),
        "the app should be running"
    );

    let program = fs::read(&installed.program).unwrap();
    let marked: Vec<u8> = [&program[..], b"new version"].concat();
    let mut checked = false;
    replace_program(&installed.program, &marked, |new| {
        checked = true;
        assert_eq!(fs::read(new).unwrap().len(), marked.len());
        Ok(())
    })
    .expect("the running program to be replaced");
    assert!(checked);

    assert_eq!(fs::read(&installed.program).unwrap(), marked);
    assert!(
        running.0.try_wait().unwrap().is_none(),
        "the old version keeps running until it restarts"
    );
    let previous = installed.dir.join("remote-volume.old");
    assert!(previous.exists());

    let _ = running.0.kill();
    let _ = running.0.wait();
    // The file can stay locked for a moment after the process exits.
    let start = Instant::now();
    loop {
        clean_up(&installed.program);
        if !previous.exists() || start.elapsed() > Duration::from_secs(5) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!previous.exists());
    assert_eq!(fs::read(&installed.program).unwrap(), marked);
}

#[test]
fn a_program_that_fails_its_check_leaves_the_running_one_in_place() {
    let installed = Installed::new("refuse");
    let original = fs::read(&installed.program).unwrap();
    let error = replace_program(&installed.program, b"not a program", |new| {
        check_program(new, &this_version())
    })
    .unwrap_err();
    // Not starting at all on some OSes, starting without reporting the version on others.
    assert!(error.contains("the new version"), "{error}");
    assert_eq!(fs::read(&installed.program).unwrap(), original);
    assert!(!installed.dir.join("remote-volume.old").exists());
}

/// A debug build reports `-dev`, so the exact version check says no; this pins down its message.
#[test]
fn check_program_reads_the_version_of_the_real_app() {
    let installed = Installed::new("version");
    let result = check_program(&installed.program, &this_version());
    if cfg!(debug_assertions) {
        assert!(result.unwrap_err().contains("-dev"));
    } else {
        result.unwrap();
    }
}
