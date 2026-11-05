//! Minimal logging seam: the app logs to stderr, tests log nowhere (or collect).

use std::sync::{Arc, Mutex};

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
