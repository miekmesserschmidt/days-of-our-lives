//! A small logger: every message goes to stderr (the journal under systemd) and to
//! `$XDG_STATE_HOME/days-of-our-lives/bar.log`, rotated at 5 MiB (one old file kept).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_LOG: u64 = 5 * 1024 * 1024;

struct Logger {
    file: Mutex<Option<(PathBuf, std::fs::File)>>,
}

pub fn log_file_path() -> PathBuf {
    dirs::state_dir().unwrap_or_else(std::env::temp_dir).join("days-of-our-lives").join("bar.log")
}

pub fn init(level: &str) {
    let level = level.parse().unwrap_or(log::LevelFilter::Info);
    let path = log_file_path();
    let file = std::fs::create_dir_all(path.parent().unwrap())
        .and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(&path))
        .ok()
        .map(|f| (path, f));
    let _ = log::set_boxed_logger(Box::new(Logger { file: Mutex::new(file) }));
    log::set_max_level(level);
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::max_level()
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
        let line = format!("{ts} {:<5} {}\n", r.level(), r.args());
        let _ = std::io::stderr().write_all(line.as_bytes());
        let mut guard = self.file.lock().unwrap();
        if let Some((path, file)) = guard.as_mut() {
            if file.metadata().is_ok_and(|m| m.len() > MAX_LOG) {
                let _ = std::fs::rename(&*path, path.with_extension("log.1"));
                if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&*path) {
                    *file = f;
                }
            }
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}
