//! Self-update of an installed app, from the GitHub releases.
//!
//! Every release carries `update.json` and its Ed25519 signature `update.json.sig`, made by CI with
//! a private key the app does not have. An update is taken only when, in this order:
//! 1. the signature of the manifest verifies against [`PUBLIC_KEY`];
//! 2. the manifest's version is newer than this one (no downgrades, no replays of old manifests);
//! 3. the download for this OS has the SHA-256 the manifest states;
//! 4. the downloaded program runs here and reports that version.
//!
//! The program file is then swapped (a running program can be renamed, even on Windows) and the app
//! restarts. Updates only ever run for an installed service (see [`crate::install`]), never for a
//! program started by hand or a development build.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ring::signature::{UnparsedPublicKey, ED25519};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::log::Log;

/// Where releases are downloaded from.
pub const RELEASES_URL: &str = "https://github.com/justereseau/companion-remote-volume/releases";

/// The public half of the release signing key (raw Ed25519). The private half is the secret
/// `UPDATE_SIGNING_KEY`.
pub const PUBLIC_KEY: [u8; 32] = [
    0xc2, 0x20, 0x1f, 0x6d, 0x96, 0x63, 0x73, 0xdd, 0x6d, 0xce, 0x32, 0xe1, 0xb6, 0x66, 0x49, 0xb6,
    0x5d, 0xff, 0x03, 0x43, 0x30, 0x89, 0x7b, 0x9e, 0xca, 0x69, 0x75, 0x82, 0x8e, 0x19, 0x34, 0xbb,
];

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 1024;
/// Far above the size of the program; a limit so a bad download cannot fill the disk.
const MAX_PROGRAM_BYTES: u64 = 200 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Not at once on start: the app may be restarting in a loop, or the network still coming up.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(5 * 60);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(60 * 60);

/// The name of this build's OS and CPU in the release files, if releases cover it.
pub fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-x64"),
        ("macos", "aarch64") => Some("macos-arm64"),
        ("linux", "x86_64") => Some("linux-x64"),
        _ => None,
    }
}

/// A release version; compares as numbers (`1.10.0` is newer than `1.9.0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// `1.2.3` exactly: a development build (`1.2.3-dev`) is not a release version.
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.split('.').map(|part| {
            let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            digits.then(|| part.parse::<u32>().ok()).flatten()
        });
        let version = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// What a verified manifest says about this OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub file: String,
    pub sha256: String,
}

/// Verifies the signature, then reads the manifest. Nothing in it is looked at before that.
pub fn read_manifest(
    manifest: &[u8],
    signature: &[u8],
    public_key: &[u8],
    target: &str,
) -> Result<Release, String> {
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(manifest, signature)
        .map_err(|_| "the update manifest has an invalid signature".to_string())?;
    let value: Value =
        serde_json::from_slice(manifest).map_err(|e| format!("bad update manifest: {e}"))?;
    let version = value["version"]
        .as_str()
        .and_then(Version::parse)
        .ok_or("the update manifest has no valid version")?;
    let asset = &value["assets"][target];
    let file = asset["file"]
        .as_str()
        .filter(|f| {
            !f.is_empty()
                && f.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
        .ok_or_else(|| format!("the update manifest has no file for {target}"))?;
    let sha256 = asset["sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| format!("the update manifest has no checksum for {target}"))?;
    Ok(Release {
        version,
        file: file.to_string(),
        sha256: sha256.to_ascii_lowercase(),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The file does not exist: e.g. the latest release has no manifest (yet).
    NotFound,
    Failed(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::NotFound => write!(f, "not found"),
            FetchError::Failed(message) => write!(f, "{message}"),
        }
    }
}

/// Downloads a file, up to `limit` bytes.
pub trait Fetch: Send + Sync {
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError>;
}

/// The real thing: HTTPS only, with the root certificates built into the program.
pub struct HttpsFetch;

impl Fetch for HttpsFetch {
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .user_agent(concat!("remote-volume/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut response = agent.get(url).call().map_err(|e| match e {
            ureq::Error::StatusCode(404) => FetchError::NotFound,
            other => FetchError::Failed(other.to_string()),
        })?;
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| FetchError::Failed(e.to_string()))
    }
}

/// Runs `command` with no window of its own, also when this process has no console.
pub fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Checks that the program at `path` runs here and is `version`. A program that cannot start on this
/// computer (a missing library, the wrong CPU) stops the update here.
pub fn check_program(path: &Path, version: &Version) -> Result<(), String> {
    let mut child = no_window(Command::new(path).arg("--version"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("the new version does not start: {e}"))?;
    let start = Instant::now();
    while child
        .try_wait()
        .map_err(|e| format!("the new version does not start: {e}"))?
        .is_none()
    {
        if start.elapsed() > VERSION_CHECK_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the new version does not answer".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("the new version does not start: {e}"))?;
    let reported = String::from_utf8_lossy(&output.stdout);
    let expected = format!("remote-volume {version}");
    if output.status.success() && reported.trim() == expected {
        Ok(())
    } else {
        Err(format!(
            "the new version reports {:?} instead of {expected:?}",
            reported.trim()
        ))
    }
}

fn sibling(exe: &Path, stem: &str, extension: &str) -> PathBuf {
    exe.with_file_name(format!("{stem}{extension}"))
}

/// The downloaded program, while it is being checked.
fn new_path(exe: &Path) -> PathBuf {
    sibling(exe, "remote-volume.new", std::env::consts::EXE_SUFFIX)
}

/// The previous program, kept until the next start: on Windows it cannot be deleted while it runs.
fn old_path(exe: &Path) -> PathBuf {
    sibling(exe, "remote-volume.old", "")
}

/// Removes what an earlier update left behind. Called on start, when the old program has exited.
pub fn clean_up(exe: &Path) {
    let _ = fs::remove_file(old_path(exe));
    let _ = fs::remove_file(new_path(exe));
}

pub(crate) fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Puts `program` in place of `exe` once `check` accepts it. On any failure `exe` stays as it was.
pub fn replace_program(
    exe: &Path,
    program: &[u8],
    check: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let new = new_path(exe);
    let old = old_path(exe);
    let cannot = |what: &str, e: io::Error| format!("could not {what}: {e}");
    let result = (|| {
        fs::write(&new, program).map_err(|e| cannot("write the new version", e))?;
        make_executable(&new).map_err(|e| cannot("make the new version executable", e))?;
        check(&new)?;
        match fs::remove_file(&old) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(cannot("remove the previous version", e)),
        }
        fs::rename(exe, &old).map_err(|e| cannot("move the running version aside", e))?;
        if let Err(e) = fs::rename(&new, exe) {
            let _ = fs::rename(&old, exe);
            return Err(cannot("put the new version in place", e));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&new);
    }
    result
}

/// What one check came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The program file was replaced: restart to run it.
    Updated(Version),
    UpToDate,
    /// The latest release has no manifest (an older release, or one still being published).
    NoManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Status {
    state: &'static str,
    latest: Option<String>,
    message: String,
}

type ProgramCheck = dyn Fn(&Path, &Version) -> Result<(), String> + Send + Sync;

pub struct Updater {
    fetch: Box<dyn Fetch>,
    base_url: String,
    public_key: Vec<u8>,
    target: String,
    current: Version,
    exe: PathBuf,
    check: Box<ProgramCheck>,
    status: Mutex<Status>,
    log: Arc<dyn Log>,
}

impl Updater {
    /// Everything injected, for tests. The app uses [`Updater::for_this_program`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fetch: Box<dyn Fetch>,
        base_url: &str,
        public_key: &[u8],
        target: &str,
        current: Version,
        exe: PathBuf,
        check: Box<ProgramCheck>,
        log: Arc<dyn Log>,
    ) -> Updater {
        Updater {
            fetch,
            base_url: base_url.trim_end_matches('/').to_string(),
            public_key: public_key.to_vec(),
            target: target.to_string(),
            current,
            exe,
            check,
            status: Mutex::new(Status {
                state: "idle",
                latest: None,
                message: String::new(),
            }),
            log,
        }
    }

    /// The updater of this program at `exe`, on HTTPS with the real key; `None` for a development
    /// build (its version has a suffix) or an OS without releases.
    pub fn for_this_program(exe: PathBuf, log: Arc<dyn Log>) -> Option<Updater> {
        let current = Version::parse(crate::VERSION)?;
        Some(Updater::new(
            Box::new(HttpsFetch),
            RELEASES_URL,
            &PUBLIC_KEY,
            target()?,
            current,
            exe,
            Box::new(check_program),
            log,
        ))
    }

    fn set_status(&self, state: &'static str, latest: Option<String>, message: String) {
        *self.status.lock().unwrap() = Status {
            state,
            latest,
            message,
        };
    }

    /// `{ managed, current, state, latest, message }` for the settings page.
    pub fn status_json(&self) -> Value {
        let status = self.status.lock().unwrap().clone();
        json!({
            "managed": true,
            "current": self.current.to_string(),
            "state": status.state,
            "latest": status.latest,
            "message": status.message,
        })
    }

    /// One check, and the update when there is one. See the module docs for the order.
    pub fn check_and_update(&self) -> Result<Outcome, String> {
        self.set_status("checking", None, "Checking for updates...".into());
        let result = self.run_check();
        match &result {
            Ok(Outcome::Updated(version)) => {
                self.log.info(&format!("Updated to {version}; restarting."));
                self.set_status(
                    "restarting",
                    Some(version.to_string()),
                    format!("Updated to {version}, restarting..."),
                );
            }
            Ok(Outcome::UpToDate) => {
                let latest = Some(self.current.to_string());
                self.set_status("up-to-date", latest, "This is the latest version.".into());
            }
            Ok(Outcome::NoManifest) => {
                self.set_status(
                    "up-to-date",
                    None,
                    "The latest release offers no update.".into(),
                );
            }
            Err(error) => {
                self.log.warn(&format!("Update failed: {error}"));
                self.set_status("error", None, format!("Update failed: {error}"));
            }
        }
        result
    }

    fn run_check(&self) -> Result<Outcome, String> {
        let latest = format!("{}/latest/download", self.base_url);
        let download = |url: String, limit| self.fetch.get(&url, limit);
        let manifest = match download(format!("{latest}/update.json"), MAX_MANIFEST_BYTES) {
            Ok(manifest) => manifest,
            Err(FetchError::NotFound) => return Ok(Outcome::NoManifest),
            Err(e) => return Err(format!("could not download the manifest: {e}")),
        };
        let signature = download(format!("{latest}/update.json.sig"), MAX_SIGNATURE_BYTES)
            .map_err(|e| format!("could not download the signature: {e}"))?;
        let release = read_manifest(&manifest, &signature, &self.public_key, &self.target)?;
        if release.version <= self.current {
            return Ok(Outcome::UpToDate);
        }

        let version = release.version;
        self.log.info(&format!("Downloading version {version}."));
        self.set_status(
            "downloading",
            Some(version.to_string()),
            format!("Downloading version {version}..."),
        );
        let url = format!("{}/download/v{version}/{}", self.base_url, release.file);
        let program = download(url, MAX_PROGRAM_BYTES)
            .map_err(|e| format!("could not download version {version}: {e}"))?;
        if sha256_hex(&program) != release.sha256 {
            return Err(format!("the download of version {version} is corrupt"));
        }
        replace_program(&self.exe, &program, |new| (self.check)(new, &version))?;
        Ok(Outcome::Updated(version))
    }
}

/// The running update loop, for the settings page.
#[derive(Clone)]
pub struct UpdateHandle {
    updater: Arc<Updater>,
    trigger: mpsc::Sender<()>,
}

impl UpdateHandle {
    pub fn status_json(&self) -> Value {
        self.updater.status_json()
    }

    /// Check right away, even with automatic updates turned off.
    pub fn check_now(&self) {
        let _ = self.trigger.send(());
    }
}

/// Starts the update loop on its own thread: a first check after a few minutes, then daily, each
/// only while `enabled` says so. `updated` is called once the new program is in place, and the app
/// then has to restart. The loop ends with that.
pub fn spawn(
    updater: Updater,
    enabled: impl Fn() -> bool + Send + 'static,
    updated: impl FnOnce() + Send + 'static,
) -> io::Result<UpdateHandle> {
    run_loop(updater, FIRST_CHECK_AFTER, enabled, updated)
}

fn run_loop(
    updater: Updater,
    first_check_after: Duration,
    enabled: impl Fn() -> bool + Send + 'static,
    updated: impl FnOnce() + Send + 'static,
) -> io::Result<UpdateHandle> {
    let updater = Arc::new(updater);
    let (trigger, requests) = mpsc::channel();
    let worker = updater.clone();
    thread::Builder::new()
        .name("updater".into())
        .spawn(move || {
            let mut wait = first_check_after;
            loop {
                let forced = match requests.recv_timeout(wait) {
                    Ok(()) => true,
                    Err(mpsc::RecvTimeoutError::Timeout) => false,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                if !forced && !enabled() {
                    wait = CHECK_EVERY;
                    continue;
                }
                match worker.check_and_update() {
                    Ok(Outcome::Updated(_)) => {
                        updated();
                        return;
                    }
                    Ok(_) => wait = CHECK_EVERY,
                    Err(_) => wait = RETRY_AFTER_ERROR,
                }
            }
        })?;
    Ok(UpdateHandle { updater, trigger })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{MemoryLog, NoLog};
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    const BASE: &str = "https://example.test/releases";
    const LATEST: &str = "https://example.test/releases/latest/download";

    fn key_pair() -> Ed25519KeyPair {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
    }

    fn manifest(version: &str, file: &str, sha256: &str) -> Vec<u8> {
        json!({
            "version": version,
            "assets": { "test-os": { "file": file, "sha256": sha256 } },
        })
        .to_string()
        .into_bytes()
    }

    /// A directory of its own under the system temp directory, removed at the end of the test.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "rv-update-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        /// A fake installed program.
        fn exe(&self) -> PathBuf {
            let exe = self.0.join("remote-volume");
            fs::write(&exe, b"old program").unwrap();
            exe
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct FakeFetch(HashMap<String, Result<Vec<u8>, String>>);

    impl Fetch for FakeFetch {
        fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
            match self.0.get(url) {
                None => Err(FetchError::NotFound),
                Some(Err(message)) => Err(FetchError::Failed(message.clone())),
                Some(Ok(bytes)) if bytes.len() as u64 > limit => {
                    Err(FetchError::Failed("too large".into()))
                }
                Some(Ok(bytes)) => Ok(bytes.clone()),
            }
        }
    }

    /// A signed release 2.0.0 with this program, as the server would offer it.
    fn offer(keys: &Ed25519KeyPair, program: &[u8]) -> FakeFetch {
        let manifest = manifest("2.0.0", "rv-test", &sha256_hex(program));
        let signature = keys.sign(&manifest).as_ref().to_vec();
        let mut files = HashMap::new();
        files.insert(format!("{LATEST}/update.json"), Ok(manifest));
        files.insert(format!("{LATEST}/update.json.sig"), Ok(signature));
        files.insert(
            format!("{BASE}/download/v2.0.0/rv-test"),
            Ok(program.to_vec()),
        );
        FakeFetch(files)
    }

    fn updater(keys: &Ed25519KeyPair, fetch: FakeFetch, exe: &Path) -> Updater {
        Updater::new(
            Box::new(fetch),
            BASE,
            keys.public_key().as_ref(),
            "test-os",
            Version(1, 0, 0),
            exe.to_path_buf(),
            Box::new(|_, _| Ok(())),
            Arc::new(NoLog),
        )
    }

    #[test]
    fn versions_compare_as_numbers_and_only_plain_releases_parse() {
        assert!(Version::parse("1.10.0").unwrap() > Version::parse("1.9.9").unwrap());
        assert!(Version::parse("2.0.0").unwrap() > Version::parse("1.99.99").unwrap());
        assert_eq!(Version::parse("1.2.3").unwrap().to_string(), "1.2.3");
        for bad in [
            "1.2.3-dev",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "",
            "1..3",
            "+1.2.3",
            "a.b.c",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_real_public_key_is_a_valid_key_shape_and_targets_are_named() {
        let keys = key_pair();
        let signed = keys.sign(b"x");
        // Not signed by the release key, so it is refused, without any error about the key itself.
        assert!(read_manifest(b"x", signed.as_ref(), &PUBLIC_KEY, "any")
            .unwrap_err()
            .contains("invalid signature"));
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(target(), Some("windows-x64"));
        }
    }

    #[test]
    fn a_manifest_is_only_read_with_a_valid_signature() {
        let keys = key_pair();
        let good = manifest("2.0.0", "rv-test", &"a".repeat(64));
        let signature = keys.sign(&good);
        let public = keys.public_key().as_ref();
        let release = read_manifest(&good, signature.as_ref(), public, "test-os").unwrap();
        assert_eq!(release.version, Version(2, 0, 0));
        assert_eq!(release.file, "rv-test");

        let tampered = manifest("9.0.0", "rv-test", &"a".repeat(64));
        for (data, sig, key) in [
            (&tampered[..], signature.as_ref(), public),
            (&good[..], &signature.as_ref()[..63], public),
            (
                &good[..],
                signature.as_ref(),
                key_pair().public_key().as_ref(),
            ),
        ] {
            assert!(read_manifest(data, sig, key, "test-os")
                .unwrap_err()
                .contains("invalid signature"));
        }
    }

    #[test]
    fn a_signed_manifest_must_still_be_well_formed() {
        let keys = key_pair();
        let public = keys.public_key().as_ref();
        let read = |data: Vec<u8>, target: &str| {
            let signature = keys.sign(&data);
            read_manifest(&data, signature.as_ref(), public, target).map(|_| ())
        };
        let hash = "a".repeat(64);
        assert!(read(b"not json".to_vec(), "test-os").is_err());
        assert!(read(manifest("1.0", "f", &hash), "test-os").is_err());
        assert!(read(manifest("1.0.0", "f", &hash), "other-os").is_err());
        assert!(read(manifest("1.0.0", "../evil", &hash), "test-os").is_err());
        assert!(read(manifest("1.0.0", "", &hash), "test-os").is_err());
        assert!(read(manifest("1.0.0", "f", "abc"), "test-os").is_err());
        assert!(read(manifest("1.0.0", "f", &"z".repeat(64)), "test-os").is_err());
        assert!(read(manifest("1.0.0", "f", &hash), "test-os").is_ok());
    }

    #[test]
    fn a_newer_signed_release_replaces_the_program() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let keys = key_pair();
        let updater = updater(&keys, offer(&keys, b"new program"), &exe);
        assert_eq!(
            updater.check_and_update(),
            Ok(Outcome::Updated(Version(2, 0, 0)))
        );
        assert_eq!(fs::read(&exe).unwrap(), b"new program");
        assert_eq!(fs::read(old_path(&exe)).unwrap(), b"old program");
        assert!(!new_path(&exe).exists());
        assert_eq!(updater.status_json()["state"], "restarting");
        assert_eq!(updater.status_json()["latest"], "2.0.0");

        clean_up(&exe);
        assert!(!old_path(&exe).exists());
        assert_eq!(fs::read(&exe).unwrap(), b"new program");
    }

    #[test]
    fn the_same_or_an_older_release_is_not_installed() {
        for current in [Version(2, 0, 0), Version(3, 1, 0)] {
            let dir = TempDir::new();
            let exe = dir.exe();
            let keys = key_pair();
            let mut updater = updater(&keys, offer(&keys, b"new program"), &exe);
            updater.current = current;
            assert_eq!(updater.check_and_update(), Ok(Outcome::UpToDate));
            assert_eq!(fs::read(&exe).unwrap(), b"old program");
            assert_eq!(updater.status_json()["state"], "up-to-date");
        }
    }

    #[test]
    fn a_release_without_a_manifest_is_no_update_and_no_error() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let updater = updater(&key_pair(), FakeFetch::default(), &exe);
        assert_eq!(updater.check_and_update(), Ok(Outcome::NoManifest));
        assert_eq!(fs::read(&exe).unwrap(), b"old program");
    }

    fn assert_refused(updater: &Updater, exe: &Path, expected: &str) {
        let error = updater.check_and_update().unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(fs::read(exe).unwrap(), b"old program");
        assert!(!new_path(exe).exists());
        assert!(!old_path(exe).exists());
        assert_eq!(updater.status_json()["state"], "error");
    }

    #[test]
    fn a_manifest_signed_with_another_key_changes_nothing() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let attacker = key_pair();
        let updater = updater(&key_pair(), offer(&attacker, b"evil program"), &exe);
        assert_refused(&updater, &exe, "invalid signature");
    }

    #[test]
    fn a_swapped_download_changes_nothing() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let keys = key_pair();
        let mut fetch = offer(&keys, b"new program");
        fetch.0.insert(
            format!("{BASE}/download/v2.0.0/rv-test"),
            Ok(b"evil program".to_vec()),
        );
        let updater = updater(&keys, fetch, &exe);
        assert_refused(&updater, &exe, "corrupt");
    }

    #[test]
    fn a_program_that_fails_its_check_changes_nothing() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let keys = key_pair();
        let mut updater = updater(&keys, offer(&keys, b"new program"), &exe);
        updater.check = Box::new(|new, version| {
            assert_eq!(fs::read(new).unwrap(), b"new program");
            Err(format!("{version} does not start"))
        });
        assert_refused(&updater, &exe, "2.0.0 does not start");
    }

    #[test]
    fn network_errors_are_reported() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let keys = key_pair();
        let log = MemoryLog::shared();

        let mut fetch = offer(&keys, b"new program");
        fetch
            .0
            .insert(format!("{LATEST}/update.json"), Err("offline".into()));
        let mut first = updater(&keys, fetch, &exe);
        first.log = log.clone();
        assert_refused(&first, &exe, "could not download the manifest: offline");
        assert_eq!(
            log.lines(),
            ["WARN Update failed: could not download the manifest: offline"]
        );

        let mut fetch = offer(&keys, b"new program");
        fetch.0.remove(&format!("{LATEST}/update.json.sig"));
        assert_refused(&updater(&keys, fetch, &exe), &exe, "the signature");

        let mut fetch = offer(&keys, b"new program");
        fetch.0.remove(&format!("{BASE}/download/v2.0.0/rv-test"));
        assert_refused(&updater(&keys, fetch, &exe), &exe, "version 2.0.0");
    }

    #[test]
    fn a_failed_swap_puts_the_running_program_back() {
        let dir = TempDir::new();
        let exe = dir.0.join("remote-volume");
        // No program at this path, so moving it aside fails.
        let error = replace_program(&exe, b"new", |_| Ok(())).unwrap_err();
        assert!(error.contains("move the running version aside"), "{error}");
        assert!(!new_path(&exe).exists());
    }

    #[test]
    fn check_program_wants_the_exact_version_from_a_program_that_runs() {
        let dir = TempDir::new();
        let missing = dir.0.join("does-not-exist");
        assert!(check_program(&missing, &Version(1, 0, 0))
            .unwrap_err()
            .contains("does not start"));
        // This test program is no remote-volume: it exits without reporting the version.
        let this_test = std::env::current_exe().unwrap();
        assert!(check_program(&this_test, &Version(1, 0, 0)).is_err());
    }

    /// Waits for `condition`, or fails the test after a few seconds.
    fn until(what: &str, condition: impl Fn() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(start.elapsed() < Duration::from_secs(5), "no {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn check_now_works_with_automatic_updates_off_and_reports_the_update() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let keys = key_pair();
        let updater = updater(&keys, offer(&keys, b"new program"), &exe);
        let (done, updated) = mpsc::channel();
        let handle = run_loop(
            updater,
            Duration::from_secs(600),
            || false,
            move || done.send(()).unwrap(),
        )
        .unwrap();
        assert_eq!(handle.status_json()["state"], "idle");
        assert_eq!(handle.status_json()["current"], "1.0.0");
        handle.check_now();
        updated
            .recv_timeout(Duration::from_secs(5))
            .expect("the update to be reported");
        assert_eq!(fs::read(&exe).unwrap(), b"new program");
    }

    #[test]
    fn the_timer_checks_only_while_enabled() {
        let dir = TempDir::new();
        let exe = dir.exe();
        let keys = key_pair();
        let off = updater(&keys, offer(&keys, b"new program"), &exe);
        let handle = run_loop(off, Duration::from_millis(1), || false, || ()).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(handle.status_json()["state"], "idle");
        assert_eq!(fs::read(&exe).unwrap(), b"old program");

        let on = updater(&keys, offer(&keys, b"new program"), &exe);
        let (done, updated) = mpsc::channel();
        let handle = run_loop(
            on,
            Duration::from_millis(1),
            || true,
            move || done.send(()).unwrap(),
        )
        .unwrap();
        updated.recv_timeout(Duration::from_secs(5)).unwrap();
        until("restarting state", || {
            handle.status_json()["state"] == "restarting"
        });
    }
}
