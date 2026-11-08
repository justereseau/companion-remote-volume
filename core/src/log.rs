//! Minimal logging seam: the app logs to stderr, or to a file as a service (it has no terminal
//! there); tests log nowhere (or collect).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Log: Send + Sync {
    fn info(&self, message: &str);
    fn warn(&self, message: &str);
    fn error(&self, message: &str);
}

/// One line per message on stderr: `INFO  message`. stdout stays free for the user.
pub struct StderrLog;

impl Log for StderrLog {
    fn info(&self, message: &str) {
        eprintln!("INFO  {message}");
    }
    fn warn(&self, message: &str) {
        eprintln!("WARN  {message}");
    }
    fn error(&self, message: &str) {
        eprintln!("ERROR {message}");
    }
}

/// Once a log file is this big when the app starts, it is kept as `<name>.old` and a new one begun.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// One line per message in a file: `2026-11-08 14:03:21Z INFO  message`.
pub struct FileLog(Mutex<File>);

impl FileLog {
    pub fn open(path: &Path) -> io::Result<FileLog> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        if fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
            let mut old = path.as_os_str().to_owned();
            old.push(".old");
            fs::rename(path, old)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(FileLog(Mutex::new(file)))
    }

    fn write(&self, level: &str, message: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let line = format!("{} {level} {message}\n", utc_timestamp(now));
        // Nowhere left to report a failing log to.
        let _ = self.0.lock().unwrap().write_all(line.as_bytes());
    }
}

impl Log for FileLog {
    fn info(&self, message: &str) {
        self.write("INFO ", message);
    }
    fn warn(&self, message: &str) {
        self.write("WARN ", message);
    }
    fn error(&self, message: &str) {
        self.write("ERROR", message);
    }
}

/// `YYYY-MM-DD HH:MM:SSZ` for seconds since 1970 (Howard Hinnant's `civil_from_days`).
fn utc_timestamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let time = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        time / 3_600,
        time / 60 % 60,
        time % 60
    )
}

/// Discards everything.
pub struct NoLog;

impl Log for NoLog {
    fn info(&self, _: &str) {}
    fn warn(&self, _: &str) {}
    fn error(&self, _: &str) {}
}

/// Remembers messages so tests can assert on them.
#[derive(Default)]
pub struct MemoryLog(pub Mutex<Vec<String>>);

impl MemoryLog {
    pub fn shared() -> Arc<MemoryLog> {
        Arc::new(MemoryLog::default())
    }
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn push(&self, level: &str, message: &str) {
        self.0.lock().unwrap().push(format!("{level} {message}"));
    }
}

impl Log for MemoryLog {
    fn info(&self, message: &str) {
        self.push("INFO", message);
    }
    fn warn(&self, message: &str) {
        self.push("WARN", message);
    }
    fn error(&self, message: &str) {
        self.push("ERROR", message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_dates() {
        assert_eq!(utc_timestamp(0), "1970-01-01 00:00:00Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29 00:00:00Z");
        assert_eq!(utc_timestamp(1_704_067_199), "2023-12-31 23:59:59Z");
    }

    #[test]
    fn a_big_log_is_set_aside_on_open() {
        let dir = std::env::temp_dir().join(format!("rv-log-{}", std::process::id()));
        let path = dir.join("nested").join("app.log");
        let log = FileLog::open(&path).unwrap();
        log.info("first");
        log.warn("second");
        log.error("third");
        drop(log);
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with("Z INFO  first"), "{}", lines[0]);
        assert!(lines[1].ends_with("Z WARN  second"), "{}", lines[1]);
        assert!(lines[2].ends_with("Z ERROR third"), "{}", lines[2]);

        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();
        FileLog::open(&path).unwrap().info("after");
        assert!(fs::read_to_string(&path).unwrap().ends_with("after\n"));
        assert!(path.with_file_name("app.log.old").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
