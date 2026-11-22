//! Finding installed plugins: executables named `dool-plugin-<name>` in `plugin_path`
//! and then `$PATH`. The first match wins, like a shell.

use dool_plugin_sdk::protocol::Description;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PREFIX: &str = "dool-plugin-";

pub fn search_dirs(plugin_path: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = plugin_path.to_vec();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

pub fn discover(dirs: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(PREFIX)) else { continue };
            if !name.is_empty() && is_executable(&path) {
                found.entry(name.to_owned()).or_insert(path);
            }
        }
    }
    found
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Run a plugin with one flag and return its stdout (2 s timeout).
pub fn run_flag(path: &Path, flag: &str) -> anyhow::Result<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(path).arg(flag).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let out = reader.join().unwrap_or_default();
            anyhow::ensure!(status.success(), "{} {flag} exited with {status}", path.display());
            return Ok(out);
        }
        if start.elapsed() > Duration::from_secs(2) {
            let _ = child.kill();
            anyhow::bail!("{} {flag} timed out", path.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn describe(path: &Path) -> anyhow::Result<Description> {
    let out = run_flag(path, "--describe")?;
    Ok(serde_json::from_str(out.lines().next().unwrap_or(""))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_executable_match_wins() {
        let base = std::env::temp_dir().join(format!("dool-discovery-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mk = |p: PathBuf, exec: bool| {
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
        };
        mk(a.join("dool-plugin-clock"), true);
        mk(b.join("dool-plugin-clock"), true);
        mk(a.join("dool-plugin-notexec"), false);
        mk(b.join("unrelated"), true);
        let found = discover(&[a.clone(), b]);
        assert_eq!(found.into_iter().collect::<Vec<_>>(), vec![("clock".to_string(), a.join("dool-plugin-clock"))]);
    }
}
