//! Hyprland IPC without `hyprctl`: the request socket (`.socket.sock`) for queries and
//! dispatches, and the event socket (`.socket2.sock`) for change notifications.
//! Targets Hyprland 0.55 (dispatchers use the Lua syntax, e.g. `hl.dsp.focus({ workspace = "3" })`).

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

fn socket(name: &str) -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").context("HYPRLAND_INSTANCE_SIGNATURE not set (not running under Hyprland?)")?;
    Ok(PathBuf::from(runtime).join("hypr").join(sig).join(name))
}

/// Send one raw request (e.g. `j/clients`) and return the reply.
pub fn request(cmd: &str) -> Result<String> {
    let path = socket(".socket.sock")?;
    let mut s = UnixStream::connect(&path).with_context(|| format!("connect {}", path.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(cmd.as_bytes())?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out)
}

/// A JSON query, e.g. `query::<Vec<Client>>("clients")`.
pub fn query<T: DeserializeOwned>(what: &str) -> Result<T> {
    let reply = request(&format!("j/{what}"))?;
    serde_json::from_str(&reply).with_context(|| format!("parse reply of j/{what}"))
}

/// Run a dispatcher, given as a Lua expression: `dispatch("hl.dsp.focus({ workspace = \"3\" })")`.
pub fn dispatch(lua: &str) -> Result<()> {
    let reply = request(&format!("dispatch {lua}"))?;
    if reply.trim() != "ok" {
        bail!("dispatch {lua}: {}", reply.trim());
    }
    Ok(())
}

/// Quote a string for use in a Lua dispatcher expression.
pub fn lua_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Watch the event socket on a thread. Whenever one of `names` happens (e.g.
/// `"workspacev2"`), `msg()` is sent — at most once per 50 ms burst of events.
/// Reconnects if Hyprland restarts. Stops when `tx` is closed.
pub fn watch<M: Send + 'static>(names: &'static [&'static str], tx: crate::Tx<M>, msg: impl Fn() -> M + Send + 'static) {
    std::thread::spawn(move || loop {
        if let Err(e) = watch_once(names, &tx, &msg) {
            log::debug!("hyprland event socket: {e:#}");
        }
        if !tx.send(msg()) {
            return; // plugin is shutting down
        }
        std::thread::sleep(Duration::from_secs(2));
    });
}

fn watch_once<M>(names: &[&str], tx: &crate::Tx<M>, msg: &impl Fn() -> M) -> Result<()> {
    let s = UnixStream::connect(socket(".socket2.sock")?)?;
    s.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut reader = BufReader::new(s);
    let mut pending = false;
    let mut line = String::new();
    loop {
        match reader.read_line(&mut line) {
            Ok(0) => bail!("event socket closed"),
            Ok(_) => {
                let name = line.split(">>").next().unwrap_or("");
                pending |= names.contains(&name);
                line.clear();
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                // A quiet period: deliver the burst as one message. A partial line stays in `line`.
                if pending && !tx.send(msg()) {
                    return Ok(());
                }
                pending = false;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct WorkspaceRef {
    pub id: i64,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    /// Physical pixels.
    pub width: f64,
    pub height: f64,
    /// Logical layout coordinates.
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub transform: i64,
    pub focused: bool,
    pub active_workspace: WorkspaceRef,
}

impl Monitor {
    /// (x, y, w, h) in logical layout coordinates — the space client positions are in.
    pub fn logical_bounds(&self) -> (f64, f64, f64, f64) {
        let s = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (w, h) = if self.transform % 2 == 1 { (self.height, self.width) } else { (self.width, self.height) };
        (self.x, self.y, w / s, h / s)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Client {
    pub address: String,
    pub mapped: bool,
    pub hidden: bool,
    pub at: [f64; 2],
    pub size: [f64; 2],
    pub workspace: WorkspaceRef,
    pub floating: bool,
    /// Monitor id.
    pub monitor: i64,
    pub class: String,
    pub title: String,
    pub pinned: bool,
    #[serde(rename = "focusHistoryID")]
    pub focus_history_id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_bounds_handle_scale_and_rotation() {
        let m = Monitor { x: 1920.0, y: 0.0, width: 2560.0, height: 1440.0, scale: 1.25, transform: 1, ..Default::default() };
        assert_eq!(m.logical_bounds(), (1920.0, 0.0, 1152.0, 2048.0));
    }

    #[test]
    fn parses_client_json() {
        let c: Client = serde_json::from_str(
            r#"{"address":"0x1","at":[5,6],"size":[7,8],"workspace":{"id":2,"name":"2"},"floating":true,"monitor":0,"class":"kitty","title":"t","focusHistoryID":3,"extra":1}"#,
        )
        .unwrap();
        assert_eq!((c.at, c.workspace.id, c.focus_history_id), ([5.0, 6.0], 2, 3));
    }

    #[test]
    fn lua_strings_are_quoted() {
        assert_eq!(lua_str(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
