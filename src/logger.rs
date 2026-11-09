//! Minimaler Datei-Logger (keine zusätzliche Abhängigkeit). Datei wird ab 1 MB neu begonnen.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct Logger {
    file: Option<Mutex<File>>,
    level: log::LevelFilter,
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        // Eigene Meldungen nach Einstellung, Bibliotheken (wgpu …) nur Warnungen/Fehler.
        if m.target().starts_with("veyra") { m.level() <= self.level } else { m.level() <= log::Level::Warn }
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
        let line = format!("{h:02}:{m:02}:{s:02} UTC {:<5} {}\n", r.level(), r.args());
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {}
}

pub fn init(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let too_big = std::fs::metadata(path).map(|m| m.len() > 1_000_000).unwrap_or(false);
    let file = OpenOptions::new().create(true).append(!too_big).write(true).truncate(too_big).open(path).ok();
    let level = match std::env::var("VEYRA_LOG").as_deref() {
        Ok("debug") => log::LevelFilter::Debug,
        _ => log::LevelFilter::Info,
    };
    let logger = Box::leak(Box::new(Logger { file: file.map(Mutex::new), level }));
    let _ = log::set_logger(logger);
    log::set_max_level(level.max(log::LevelFilter::Warn));
}
