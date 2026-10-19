//! aether.rs - things real eframe doesn't have, for app shells that know
//! they run on æther: the system file picker, and a stderr logger.
//!
//! The backend lives in a process-wide slot so these work from inside
//! `App::ui` while `run_native` owns the loop.

use std::sync::Mutex;

use aether_host::{Backend, PickMode};

static BACKEND: Mutex<Option<Box<dyn Backend + Send>>> = Mutex::new(None);

/// Runs `f` with the process's backend, opening it on first use
/// (æther when AETHER=1, headless otherwise).
pub fn with_backend<R>(f: impl FnOnce(&mut dyn Backend) -> R) -> R {
    let mut slot = BACKEND.lock().unwrap_or_else(|e| e.into_inner());
    let b = slot.get_or_insert_with(aether_host::open);
    f(b.as_mut())
}

/// Use this backend instead (tests); call before `run_native`.
pub fn set_backend(b: Box<dyn Backend + Send>) {
    *BACKEND.lock().unwrap_or_else(|e| e.into_inner()) = Some(b);
}

/// "aether" or "headless".
pub fn backend_name() -> &'static str {
    with_backend(|b| b.name())
}

/// Asks the user for a file to open; returns its name and contents.
/// `None` when they cancel (or the app lacks CAP_PICK).
pub fn pick_and_read() -> Option<(String, Vec<u8>)> {
    with_backend(|b| {
        let p = b.pick_file(PickMode::Open, "").map_err(|e| log::info!("file picker: {e}")).ok()?;
        let mut data = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let ok = loop {
            match b.read(p.handle, &mut buf) {
                Ok(0) => break true,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(e) => {
                    log::warn!("reading {}: {e}", p.name());
                    break false;
                }
            }
        };
        let _ = b.close(p.handle);
        ok.then(|| (p.name().to_owned(), data))
    })
}

/// Asks the user where to save, proposing `suggested`, and writes `bytes`.
/// Returns the name they chose.
pub fn pick_and_write(suggested: &str, bytes: &[u8]) -> Option<String> {
    with_backend(|b| {
        let p = b.pick_file(PickMode::Save, suggested).map_err(|e| log::info!("file picker: {e}")).ok()?;
        let mut rest = bytes;
        let mut ok = true;
        while !rest.is_empty() {
            match b.write(p.handle, rest) {
                Ok(0) | Err(_) => {
                    ok = false;
                    break;
                }
                Ok(n) => rest = &rest[n..],
            }
        }
        let _ = b.close(p.handle);
        ok.then(|| p.name().to_owned())
    })
}

struct StderrLogger(log::LevelFilter);

impl log::Log for StderrLogger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= self.0
    }

    fn log(&self, r: &log::Record<'_>) {
        if self.enabled(r.metadata()) {
            eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
        }
    }

    fn flush(&self) {}
}

/// Logs to stderr at the level in RUST_LOG (a plain level name; default warn).
pub fn init_logger() {
    let level = std::env::var("RUST_LOG").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(log::LevelFilter::Warn);
    if log::set_boxed_logger(Box::new(StderrLogger(level))).is_ok() {
        log::set_max_level(level);
    }
}
