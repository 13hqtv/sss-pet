//! File-based logger. Writes to `{data_dir}/log/swd-pet-YYYY-MM-DD.log`
//! with daily rotation (one file per day, named by date). Also mirrors
//! to stderr when `to_stderr=true` (GUI mode); in MCP server mode stderr
//! is suppressed because stdout is the JSON-RPC transport.
//!
//! Usage: call [`init`] once at startup (from `lib.rs::run` for GUI mode,
//! from `main.rs` for MCP server mode). After init, use the standard
//! `log` macros: `log::info!`, `log::warn!`, `log::error!`, `log::debug!`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// The global logger instance. Held in a `Mutex<File>` so concurrent
/// threads (brain tick, reminder scheduler, chat drain) can write safely.
struct FileLogger {
    file: Mutex<File>,
    to_stderr: bool,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Trace
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let line = format!(
            "{} {:>5} [{}] {}\n",
            now,
            record.level(),
            record.target(),
            record.args()
        );
        // Write to file (best-effort — never panic on write failure).
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
        // Mirror to stderr if enabled (GUI dev mode, terminal launch).
        if self.to_stderr {
            eprint!("{}", line);
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Initialize the global logger. Must be called exactly once at startup.
/// If the log directory can't be created or the file can't be opened,
/// logging falls back to stderr-only (no panic — logging is best-effort).
///
/// # Arguments
/// - `to_stderr`: whether to also print to stderr. `true` for GUI mode
///   (useful when launched from a terminal / dev mode); `false` for MCP
///   server mode (stdout is the JSON-RPC channel — must stay clean).
pub fn init(to_stderr: bool) {
    // Resolve the log directory: {data_dir}/log/
    let log_dir = crate::db::resolve_data_dir()
        .map(|d| d.join("log"))
        .unwrap_or_else(|| std::path::PathBuf::from("log"));

    // Create the log directory if it doesn't exist.
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        log::error!("[logger] failed to create log dir {:?}: {e}", log_dir);
        // Fall back to stderr-only logger.
        let _ = log::set_logger(Box::leak(Box::new(StderrOnlyLogger)))
            .map(|_| log::set_max_level(LevelFilter::Info));
        return;
    }

    // Daily log file: swd-pet-YYYY-MM-DD.log (append mode — multiple
    // launches on the same day append to the same file).
    let today = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("swd-pet-{today}.log"));

    let file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            log::error!("[logger] failed to open log file {:?}: {e}", log_path);
            let _ = log::set_logger(Box::leak(Box::new(StderrOnlyLogger)))
                .map(|_| log::set_max_level(LevelFilter::Info));
            return;
        }
    };

    let logger = FileLogger {
        file: Mutex::new(file),
        to_stderr,
    };

    let level = if cfg!(debug_assertions) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };

    let boxed = Box::leak(Box::new(logger));
    let _ = log::set_logger(boxed).map(|_| log::set_max_level(level));

    log::info!("=== swd-pet started ===");
    log::info!("log file: {}", log_path.display());
}

/// Fallback logger that only writes to stderr (used when the log file
/// can't be created).
struct StderrOnlyLogger;

impl Log for StderrOnlyLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        log::error!(
            "{} {:>5} [{}] {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {}
}
