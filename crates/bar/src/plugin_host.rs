//! One running plugin process: spawn it, write bar messages to its stdin, read its
//! stdout/stderr on threads, and stop it (close stdin → SIGTERM after 2 s → SIGKILL after 4 s).

use dool_plugin_sdk::protocol::{BarMsg, PluginMsg};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAX_LINE: usize = 8 * 1024 * 1024;

pub enum HostEvent {
    Msg(PluginMsg),
    Exited(String),
}

pub struct PluginProcess {
    pid: i32,
    stdin: Option<ChildStdin>,
    exited: Arc<AtomicBool>,
}

impl PluginProcess {
    /// `label` prefixes the plugin's log lines, e.g. `calendar@main/DP-1`.
    pub fn spawn(path: &Path, label: &str, env: &[(&str, String)], init: &BarMsg) -> std::io::Result<(Self, async_channel::Receiver<HostEvent>)> {
        let mut cmd = Command::new(path);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(home) = dirs::home_dir() {
            cmd.current_dir(home);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe calls between fork and exec. The bar blocks
        // SIGTERM/SIGINT/SIGHUP for its sigwait thread; plugins must get them normally.
        unsafe {
            cmd.pre_exec(|| {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigfillset(&mut set);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let (tx, rx) = async_channel::unbounded();
        let exited = Arc::new(AtomicBool::new(false));

        let stdout = child.stdout.take().unwrap();
        let (tx_out, label_out) = (tx.clone(), label.to_owned());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.len() > MAX_LINE => log::warn!("[{label_out}] message over 8 MiB ignored"),
                    Ok(_) => match serde_json::from_str::<PluginMsg>(&line) {
                        Ok(msg) => {
                            if tx_out.send_blocking(HostEvent::Msg(msg)).is_err() {
                                break;
                            }
                        }
                        Err(e) => log::warn!("[{label_out}] bad message: {e}"),
                    },
                }
            }
        });

        let stderr = child.stderr.take().unwrap();
        let label_err = label.to_owned();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::info!("[{label_err}] {line}");
            }
        });

        let pid = child.id() as i32;
        let stdin = child.stdin.take();
        let exited_flag = exited.clone();
        std::thread::spawn(move || {
            let status = child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
            exited_flag.store(true, Ordering::SeqCst);
            let _ = tx.send_blocking(HostEvent::Exited(status));
        });

        let mut p = PluginProcess { pid, stdin, exited };
        p.send(init);
        Ok((p, rx))
    }

    pub fn send(&mut self, msg: &BarMsg) {
        let Some(stdin) = self.stdin.as_mut() else { return };
        let mut line = serde_json::to_string(msg).expect("serialisable message");
        line.push('\n');
        // A plugin that exited makes this fail with EPIPE; the exit is reported separately.
        if stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_err() {
            self.stdin = None;
        }
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        self.stdin = None; // EOF: plugins exit on their own
        let (pid, exited) = (self.pid, self.exited.clone());
        std::thread::spawn(move || {
            for signal in [libc::SIGTERM, libc::SIGKILL] {
                std::thread::sleep(Duration::from_secs(2));
                if exited.load(Ordering::SeqCst) {
                    return;
                }
                // SAFETY: plain syscall; the pid is still our unreaped child (the waiter
                // thread hasn't seen it exit), so it cannot have been reused.
                unsafe { libc::kill(pid, signal) };
            }
        });
    }
}
