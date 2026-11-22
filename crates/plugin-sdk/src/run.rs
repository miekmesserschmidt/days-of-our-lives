//! The plugin runtime: read `init`, then turn bar messages and the plugin's own
//! messages into calls on [`Plugin`], and send a new SVG frame whenever asked to.

use crate::protocol::{BarMsg, Button, Description, Init, PluginMsg, Pointer, Rect, Style, PROTOCOL_VERSION};
use serde::de::DeserializeOwned;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;

/// What a plugin says about itself for `--describe`.
pub struct Describe {
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
}

/// Size and look of the slot the plugin draws into.
#[derive(Debug, Clone)]
pub struct Frame {
    pub instance: String,
    pub bar: String,
    pub output: String,
    pub edge: String,
    /// "horizontal" or "vertical"
    pub orientation: String,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub style: Style,
    /// Directory of the instance's TOML file; relative paths in the config are relative to it.
    pub config_dir: PathBuf,
}

impl Frame {
    fn from_init(init: &Init) -> Self {
        Frame {
            instance: init.instance.clone(),
            bar: init.bar.clone(),
            output: init.output.clone(),
            edge: init.edge.clone(),
            orientation: init.orientation.clone(),
            width: init.width,
            height: init.height,
            scale: init.scale,
            style: init.style.clone(),
            config_dir: PathBuf::from(&init.config_dir),
        }
    }

    pub fn is_vertical(&self) -> bool {
        self.orientation == "vertical"
    }

    /// Resolve a path from the config: `~` expands to `$HOME`, relative paths are
    /// relative to the instance's config directory.
    pub fn resolve_path(&self, path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            return dirs_home().join(rest);
        }
        let p = PathBuf::from(path);
        if p.is_absolute() { p } else { self.config_dir.join(p) }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Mouse input on the slot. Coordinates are SVG user units of the frame on screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Mouse {
    Enter(Pointer),
    Move(Pointer),
    Leave,
    Down(Pointer, Button),
    Up(Pointer, Button),
    Click(Pointer, Button),
    Wheel(Pointer, f64, f64),
}

impl Mouse {
    pub fn pointer(&self) -> Option<&Pointer> {
        match self {
            Mouse::Enter(p) | Mouse::Move(p) | Mouse::Down(p, _) | Mouse::Up(p, _) | Mouse::Click(p, _) | Mouse::Wheel(p, _, _) => Some(p),
            Mouse::Leave => None,
        }
    }
}

/// Side effects requested by a mouse handler.
#[derive(Debug, Default)]
pub struct Out {
    tooltip: Option<Option<(String, Option<Rect>)>>,
}

impl Out {
    pub fn tooltip(&mut self, text: impl Into<String>, anchor: Option<Rect>) {
        self.tooltip = Some(Some((text.into(), anchor)));
    }
    pub fn hide_tooltip(&mut self) {
        self.tooltip = Some(None);
    }
}

/// Sender for the plugin's own messages (from timer / IPC threads).
pub struct Tx<M>(mpsc::Sender<Event<M>>);

impl<M> Clone for Tx<M> {
    fn clone(&self) -> Self {
        Tx(self.0.clone())
    }
}

impl<M> Tx<M> {
    /// Returns false once the plugin is shutting down, so threads can stop.
    pub fn send(&self, msg: M) -> bool {
        self.0.send(Event::Own(msg)).is_ok()
    }
}

#[allow(clippy::large_enum_variant)] // one message at a time; boxing buys nothing
enum Event<M> {
    Bar(BarMsg),
    Own(M),
    Eof,
}

pub trait Plugin: Sized {
    /// The plugin's own TOML keys (everything except `plugin`, `enabled`, `[position]`).
    type Config: DeserializeOwned;
    /// Messages from the plugin's own threads.
    type Msg: Send + 'static;

    /// Spawn timers / IPC threads that send `Self::Msg` through `tx`.
    fn start(config: Self::Config, frame: &Frame, tx: Tx<Self::Msg>) -> anyhow::Result<Self>;
    /// Handle an own message. Return true to redraw.
    fn update(&mut self, msg: Self::Msg, frame: &Frame) -> bool;
    /// All mouse input. Return true to redraw (e.g. hover highlight changed).
    fn mouse(&mut self, _ev: &Mouse, _frame: &Frame, _out: &mut Out) -> bool {
        false
    }
    /// Pure: no I/O, no clock reads. Everything it needs is in `self` and `frame`.
    fn render(&self, frame: &Frame) -> String;
}

/// Write one protocol message to stdout (one line, flushed). Used by the logger.
fn send(msg: &PluginMsg) {
    let mut line = serde_json::to_string(msg).expect("serialisable message");
    line.push('\n');
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes()).and_then(|_| out.flush());
}

struct ProtocolLogger;

impl log::Log for ProtocolLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::max_level()
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            let level = match r.level() {
                log::Level::Error => "error",
                log::Level::Warn => "warn",
                log::Level::Info => "info",
                _ => "debug",
            };
            send(&PluginMsg::Log { level: level.into(), message: r.args().to_string() });
        }
    }
    fn flush(&self) {}
}

/// Run the plugin `P`. Never returns.
pub fn run<P: Plugin>(describe: Describe, default_config: &str) -> ! {
    match std::env::args().nth(1).as_deref() {
        Some("--describe") => {
            let d = Description {
                name: describe.name.into(),
                description: describe.description.into(),
                protocol: PROTOCOL_VERSION,
                version: describe.version.into(),
            };
            println!("{}", serde_json::to_string(&d).unwrap());
            std::process::exit(0);
        }
        Some("--default-config") => {
            print!("{default_config}");
            std::process::exit(0);
        }
        _ => {}
    }
    let _ = log::set_logger(&ProtocolLogger);
    log::set_max_level(log::LevelFilter::Info);
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    let stdin = std::io::BufReader::new(std::io::stdin());
    std::process::exit(main_loop::<P, _, _>(stdin, std::io::stdout()))
}

/// The whole runtime, generic over input and output so it can be tested in memory.
fn main_loop<P, R, W>(mut input: R, output: W) -> i32
where
    P: Plugin,
    R: BufRead + Send + 'static,
    W: Write,
{
    let mut first = String::new();
    if input.read_line(&mut first).unwrap_or(0) == 0 {
        return 0;
    }
    let init = match serde_json::from_str::<BarMsg>(&first) {
        Ok(BarMsg::Init(init)) => init,
        other => {
            log::error!("expected an init message first, got {other:?}");
            return 2;
        }
    };
    let config: P::Config = match serde_path_to_error::deserialize(&init.config) {
        Ok(c) => c,
        Err(e) => {
            log::error!("config error: {}: {}", e.path(), e.inner());
            return 2;
        }
    };
    let mut frame = Frame::from_init(&init);
    let (tx, rx) = mpsc::channel::<Event<P::Msg>>();
    let mut plugin = match P::start(config, &frame, Tx(tx.clone())) {
        Ok(p) => p,
        Err(e) => {
            log::error!("{e:#}");
            return 2;
        }
    };

    // Bar messages are read on their own thread, so they share one loop with own messages.
    std::thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<BarMsg>(&line) {
                Ok(msg) => {
                    if tx.send(Event::Bar(msg)).is_err() {
                        return;
                    }
                }
                Err(e) => log::warn!("bad message from bar: {e}"),
            }
        }
        let _ = tx.send(Event::Eof);
    });

    let mut rt = Runtime { out: output, last_svg: None, last_tooltip: None };
    rt.redraw(&plugin, &frame);
    for event in rx {
        let redraw = match event {
            Event::Eof => break,
            Event::Own(msg) => plugin.update(msg, &frame),
            Event::Bar(msg) => rt.handle_bar(&mut plugin, &mut frame, msg),
        };
        if redraw {
            rt.redraw(&plugin, &frame);
        }
    }
    0
}

struct Runtime<W: Write> {
    out: W,
    last_svg: Option<String>,
    last_tooltip: Option<(String, Option<Rect>)>,
}

impl<W: Write> Runtime<W> {
    fn send(&mut self, msg: &PluginMsg) {
        let mut line = serde_json::to_string(msg).expect("serialisable message");
        line.push('\n');
        let _ = self.out.write_all(line.as_bytes()).and_then(|_| self.out.flush());
    }

    fn redraw<P: Plugin>(&mut self, plugin: &P, frame: &Frame) {
        let svg = plugin.render(frame);
        if self.last_svg.as_ref() != Some(&svg) {
            self.send(&PluginMsg::Svg { svg: svg.clone() });
            self.last_svg = Some(svg);
        }
    }

    fn handle_bar<P: Plugin>(&mut self, plugin: &mut P, frame: &mut Frame, msg: BarMsg) -> bool {
        let mouse = match msg {
            BarMsg::Resize { width, height, scale } => {
                (frame.width, frame.height, frame.scale) = (width, height, scale);
                return true;
            }
            BarMsg::Style(style) => {
                frame.style = style;
                return true;
            }
            BarMsg::Init(_) | BarMsg::Unknown => return false,
            BarMsg::MouseEnter(p) => Mouse::Enter(p),
            BarMsg::MouseMove(p) => Mouse::Move(p),
            BarMsg::MouseLeave => Mouse::Leave,
            BarMsg::MouseDown { pointer, button } => Mouse::Down(pointer, button),
            BarMsg::MouseUp { pointer, button } => Mouse::Up(pointer, button),
            BarMsg::Click { pointer, button } => Mouse::Click(pointer, button),
            BarMsg::Wheel { pointer, dx, dy } => Mouse::Wheel(pointer, dx, dy),
        };
        if mouse == Mouse::Leave {
            // The bar hides the tooltip itself on leave.
            self.last_tooltip = None;
        }
        let mut out = Out::default();
        let redraw = plugin.mouse(&mouse, frame, &mut out);
        if let Some(t) = out.tooltip {
            if t != self.last_tooltip {
                let (text, anchor) = match &t {
                    Some((text, anchor)) => (Some(text.clone()), *anchor),
                    None => (None, None),
                };
                self.send(&PluginMsg::Tooltip { text, anchor });
                self.last_tooltip = t;
            }
        }
        redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Counter {
        n: u32,
        hovered: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct Cfg {
        start: u32,
    }

    impl Plugin for Counter {
        type Config = Cfg;
        type Msg = u32;
        fn start(c: Cfg, _: &Frame, tx: Tx<u32>) -> anyhow::Result<Self> {
            tx.send(0); // a no-op increment: must not produce a duplicate frame
            Ok(Counter { n: c.start, hovered: None })
        }
        fn update(&mut self, by: u32, _: &Frame) -> bool {
            self.n += by;
            true
        }
        fn mouse(&mut self, ev: &Mouse, _: &Frame, out: &mut Out) -> bool {
            let t = ev.pointer().and_then(|p| p.target("item-")).map(str::to_owned);
            match &t {
                Some(id) => out.tooltip(format!("item {id}"), None),
                None => out.hide_tooltip(),
            }
            self.hovered = t;
            if let Mouse::Click(..) = ev {
                self.n += 1;
                return true;
            }
            false
        }
        fn render(&self, f: &Frame) -> String {
            format!("<svg n='{}' w='{}'/>", self.n, f.width)
        }
    }

    fn init(config: &str) -> String {
        format!(
            r#"{{"type":"init","protocol":1,"instance":"c","bar":"main","output":"DP-1","edge":"top","orientation":"horizontal","width":40,"height":20,"scale":1,"style":{{}},"config":{config},"config_dir":"/tmp"}}"#
        )
    }

    fn drive(lines: &[String]) -> (i32, Vec<PluginMsg>) {
        let text = if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" };
        let input = Cursor::new(text);
        let mut out = Vec::new();
        let code = main_loop::<Counter, _, _>(input, &mut out);
        let msgs = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        (code, msgs)
    }

    fn svg(s: &str) -> PluginMsg {
        PluginMsg::Svg { svg: s.into() }
    }

    #[test]
    fn frames_are_sent_on_init_resize_and_click_without_duplicates() {
        let mv = r#"{"type":"mousemove","x":1,"y":1,"targets":["item-2"]}"#;
        let (code, msgs) = drive(&[
            init(r#"{"start":5}"#),
            r#"{"type":"resize","width":50,"height":20,"scale":1}"#.into(),
            mv.into(),
            mv.into(),
            r#"{"type":"click","x":1,"y":1,"targets":["item-2"],"button":1}"#.into(),
            r#"{"type":"mousemove","x":30,"y":1,"targets":[]}"#.into(),
        ]);
        assert_eq!(code, 0);
        let tip = |t: Option<&str>| PluginMsg::Tooltip { text: t.map(Into::into), anchor: None };
        // The own `0` increment can arrive anywhere; it never changes the picture.
        assert_eq!(
            msgs,
            vec![
                svg("<svg n='5' w='40'/>"),
                svg("<svg n='5' w='50'/>"),
                tip(Some("item 2")),
                svg("<svg n='6' w='50'/>"),
                tip(None),
            ]
        );
    }

    #[test]
    fn bad_config_exits_with_code_2() {
        let (code, msgs) = drive(&[init(r#"{"start":"five"}"#)]);
        assert_eq!((code, msgs), (2, vec![]));
    }

    #[test]
    fn eof_before_init_exits_cleanly() {
        let (code, _) = drive(&[]);
        assert_eq!(code, 0);
    }
}
