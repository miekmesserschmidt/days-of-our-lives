//! dool-plugin-workspaces: a `columns × rows` grid of Hyprland workspaces. Each cell
//! shows that workspace's windows as tiny rectangles at their real relative positions.
//! Clicking a cell switches to it; the wheel steps through workspaces.

use anyhow::Result;
use dool_plugin_sdk::hypr::{self, Client, Monitor, Workspace};
use dool_plugin_sdk::svg::{check_color, esc, num, Svg};
use dool_plugin_sdk::{tick, Frame, Mouse, Out, Plugin, Rect, Tx};
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_CONFIG: &str = r##"plugin = "workspaces"
# Grid size (columns × rows cells).
columns = 3
rows = 2
# true: the grid lists the workspaces on this bar's display (in id order), with
# that display's active workspace and windows.
# false: the grid shows workspaces first .. first + columns*rows - 1 of all displays.
this_output_only = true
first = 1
show_windows = true
show_numbers = false
active_color = "#cba6f7"
workspace_color = "#313244"
# "" = the CSS text colour of the slot.
window_color = ""
focused_window_color = "#f5c2e7"
hover_color = "#45475a"
tooltip = true
# Safety-net refresh in seconds (Hyprland sends no event for tiled resizes); 0 = off.
poll_seconds = 2

[position]
bar = "main"
section = "end"
"##;

/// Hyprland events after which the picture may change.
const EVENTS: &[&str] = &[
    "workspacev2", "createworkspacev2", "destroyworkspacev2", "moveworkspacev2", "focusedmonv2",
    "openwindow", "closewindow", "movewindowv2", "changefloatingmode", "fullscreen",
    "activewindowv2", "monitoraddedv2", "monitorremovedv2",
];

const GAP: f64 = 2.0;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    columns: u32,
    rows: u32,
    first: i64,
    this_output_only: bool,
    show_windows: bool,
    show_numbers: bool,
    active_color: String,
    workspace_color: String,
    window_color: String,
    focused_window_color: String,
    hover_color: String,
    tooltip: bool,
    poll_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            columns: 3,
            rows: 2,
            first: 1,
            this_output_only: true,
            show_windows: true,
            show_numbers: false,
            active_color: "#cba6f7".into(),
            workspace_color: "#313244".into(),
            window_color: String::new(),
            focused_window_color: "#f5c2e7".into(),
            hover_color: "#45475a".into(),
            tooltip: true,
            poll_seconds: 2,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
struct HyprState {
    monitors: Vec<Monitor>,
    workspaces: Vec<Workspace>,
    clients: Vec<Client>,
}

fn query_state() -> Result<HyprState> {
    Ok(HyprState { monitors: hypr::query("monitors")?, workspaces: hypr::query("workspaces")?, clients: hypr::query("clients")? })
}

struct Workspaces {
    cfg: Config,
    hypr: HyprState,
    hovered: Option<i64>,
    error_logged: bool,
}

struct Grid {
    x0: f64,
    y0: f64,
    cw: f64,
    ch: f64,
}

impl Workspaces {
    /// The monitor this bar copy is on (or the focused one when unknown).
    fn bar_monitor(&self, frame: &Frame) -> Option<&Monitor> {
        let m = &self.hypr.monitors;
        m.iter().find(|m| m.name == frame.output).or_else(|| m.iter().find(|m| m.focused)).or(m.first())
    }

    /// The workspace id of each grid cell; `None` for unused cells.
    fn shown(&self, frame: &Frame) -> Vec<Option<i64>> {
        let n = (self.cfg.columns.max(1) * self.cfg.rows.max(1)) as usize;
        let mon = if self.cfg.this_output_only { self.bar_monitor(frame) } else { None };
        let mut ids: Vec<Option<i64>> = match mon {
            Some(m) => {
                let mut ids: Vec<i64> = self.hypr.workspaces.iter().filter(|w| w.monitor == m.name && w.id > 0).map(|w| w.id).collect();
                ids.sort_unstable();
                ids.into_iter().take(n).map(Some).collect()
            }
            None => (0..n as i64).map(|i| Some(self.cfg.first + i)).collect(),
        };
        ids.resize(n, None);
        ids
    }

    fn active_workspace(&self, frame: &Frame) -> Option<i64> {
        let m = if self.cfg.this_output_only {
            self.bar_monitor(frame)
        } else {
            self.hypr.monitors.iter().find(|m| m.focused).or_else(|| self.bar_monitor(frame))
        };
        m.map(|m| m.active_workspace.id)
    }

    fn grid(&self, frame: &Frame) -> Grid {
        let aspect = self
            .bar_monitor(frame)
            .map(|m| m.logical_bounds())
            .filter(|b| b.3 > 0.0)
            .map(|b| b.2 / b.3)
            .unwrap_or(16.0 / 9.0);
        let (cols, rows) = (self.cfg.columns.max(1) as f64, self.cfg.rows.max(1) as f64);
        let max_w = (frame.width - (cols - 1.0) * GAP) / cols;
        let max_h = (frame.height - (rows - 1.0) * GAP) / rows;
        let cw = max_w.min(max_h * aspect).max(1.0);
        let ch = cw / aspect;
        let (gw, gh) = (cols * cw + (cols - 1.0) * GAP, rows * ch + (rows - 1.0) * GAP);
        Grid { x0: (frame.width - gw) / 2.0, y0: (frame.height - gh) / 2.0, cw, ch }
    }

    fn cell(&self, g: &Grid, i: u32) -> Rect {
        let (c, r) = ((i % self.cfg.columns.max(1)) as f64, (i / self.cfg.columns.max(1)) as f64);
        Rect { x: g.x0 + c * (g.cw + GAP), y: g.y0 + r * (g.ch + GAP), w: g.cw, h: g.ch }
    }

    fn windows_on(&self, ws: i64, frame: &Frame) -> Vec<&Client> {
        let bar_mon = self.bar_monitor(frame).map(|m| m.id);
        let mut v: Vec<&Client> = self
            .hypr
            .clients
            .iter()
            .filter(|c| c.mapped && !c.hidden && c.workspace.id == ws)
            .filter(|c| !self.cfg.this_output_only || Some(c.monitor) == bar_mon)
            .collect();
        v.sort_by_key(|c| c.floating); // floating windows on top
        v
    }

    fn tooltip_text(&self, ws: i64, window: Option<&str>, frame: &Frame) -> String {
        if let Some(c) = window.and_then(|a| self.hypr.clients.iter().find(|c| c.address == a)) {
            return format!("{} — {}", c.class, c.title);
        }
        let n = self.windows_on(ws, frame).len();
        format!("Workspace {ws} · {n} window{}", if n == 1 { "" } else { "s" })
    }
}

impl Plugin for Workspaces {
    type Config = Config;
    type Msg = ();

    fn start(cfg: Config, _frame: &Frame, tx: Tx<()>) -> Result<Self> {
        for (key, c) in [("active_color", &cfg.active_color), ("workspace_color", &cfg.workspace_color), ("focused_window_color", &cfg.focused_window_color), ("hover_color", &cfg.hover_color)] {
            check_color(c).map_err(|e| anyhow::anyhow!("{key}: {e}"))?;
        }
        if !cfg.window_color.is_empty() {
            check_color(&cfg.window_color).map_err(|e| anyhow::anyhow!("window_color: {e}"))?;
        }
        anyhow::ensure!(cfg.columns > 0 && cfg.rows > 0, "columns and rows must be at least 1");
        anyhow::ensure!(cfg.first >= 1, "first: workspace ids start at 1");
        hypr::watch(EVENTS, tx.clone(), || ());
        if cfg.poll_seconds > 0 {
            tick::every(Duration::from_secs(cfg.poll_seconds), tx, || ());
        }
        let mut w = Workspaces { cfg, hypr: HyprState::default(), hovered: None, error_logged: false };
        w.update((), _frame);
        Ok(w)
    }

    fn update(&mut self, _: (), _frame: &Frame) -> bool {
        match query_state() {
            Ok(state) => {
                self.error_logged = false;
                let changed = state != self.hypr;
                self.hypr = state;
                changed
            }
            Err(e) => {
                if !std::mem::replace(&mut self.error_logged, true) {
                    log::warn!("hyprland query failed: {e:#}");
                }
                false
            }
        }
    }

    fn mouse(&mut self, ev: &Mouse, frame: &Frame, out: &mut Out) -> bool {
        let ws = ev.pointer().and_then(|p| p.target("ws-")).and_then(|s| s.parse::<i64>().ok());
        match ev {
            Mouse::Click(_, 1) => {
                if let Some(ws) = ws {
                    if let Err(e) = hypr::dispatch(&format!("hl.dsp.focus({{ workspace = {} }})", hypr::lua_str(&ws.to_string()))) {
                        log::warn!("{e:#}");
                    }
                }
                false
            }
            Mouse::Wheel(_, _, dy) => {
                let target = if self.cfg.this_output_only {
                    // Step through this display's workspaces (`m+1` would use the focused display).
                    let ids: Vec<i64> = self.shown(frame).into_iter().flatten().collect();
                    let active = self.active_workspace(frame);
                    match ids.iter().position(|&id| Some(id) == active) {
                        Some(i) if !ids.is_empty() => {
                            let j = if *dy > 0.0 { (i + 1) % ids.len() } else { (i + ids.len() - 1) % ids.len() };
                            Some(ids[j].to_string())
                        }
                        _ => ids.first().map(|id| id.to_string()),
                    }
                } else {
                    Some(if *dy > 0.0 { "m+1" } else { "m-1" }.to_owned())
                };
                if let Some(t) = target {
                    if let Err(e) = hypr::dispatch(&format!("hl.dsp.focus({{ workspace = {} }})", hypr::lua_str(&t))) {
                        log::warn!("{e:#}");
                    }
                }
                false
            }
            Mouse::Leave => self.hovered.take().is_some(),
            Mouse::Enter(p) | Mouse::Move(p) => {
                if self.cfg.tooltip {
                    match ws.and_then(|ws| Some((ws, self.shown(frame).iter().position(|&s| s == Some(ws))?))) {
                        Some((ws, i)) => {
                            let g = self.grid(frame);
                            let cell = self.cell(&g, i as u32);
                            out.tooltip(self.tooltip_text(ws, p.target("win-"), frame), Some(cell));
                        }
                        None => out.hide_tooltip(),
                    }
                }
                let changed = ws != self.hovered;
                self.hovered = ws;
                changed
            }
            _ => false,
        }
    }

    fn render(&self, frame: &Frame) -> String {
        let g = self.grid(frame);
        let mut s = Svg::new(frame.width, frame.height);
        let active = self.active_workspace(frame);
        let fg = if self.cfg.window_color.is_empty() {
            if frame.style.color.is_empty() { "#cdd6f4".to_owned() } else { frame.style.color.clone() }
        } else {
            self.cfg.window_color.clone()
        };
        for (i, ws) in self.shown(frame).into_iter().enumerate() {
            let cell = self.cell(&g, i as u32);
            let Some(ws) = ws else {
                s.rect(cell.x, cell.y, cell.w, cell.h, &format!(r#"rx="2" fill="{}" fill-opacity="0.4""#, self.cfg.workspace_color));
                continue;
            };
            let fill = if Some(ws) == self.hovered { &self.cfg.hover_color } else { &self.cfg.workspace_color };
            s.open_group(&format!("ws-{ws}"), "");
            s.rect(cell.x, cell.y, cell.w, cell.h, &format!(r#"rx="2" fill="{fill}""#));
            if Some(ws) == active {
                s.rect(cell.x + 0.5, cell.y + 0.5, cell.w - 1.0, cell.h - 1.0, &format!(r#"rx="2" fill="none" stroke="{}" stroke-width="1.5""#, self.cfg.active_color));
            }
            if self.cfg.show_windows {
                for c in self.windows_on(ws, frame) {
                    let Some(m) = self.hypr.monitors.iter().find(|m| m.id == c.monitor) else { continue };
                    let (mx, my, mw, mh) = m.logical_bounds();
                    if mw <= 0.0 || mh <= 0.0 {
                        continue;
                    }
                    let (sx, sy) = (cell.w / mw, cell.h / mh);
                    let x = cell.x + ((c.at[0] - mx) * sx).clamp(0.0, cell.w);
                    let y = cell.y + ((c.at[1] - my) * sy).clamp(0.0, cell.h);
                    let w = (c.size[0] * sx).min(cell.x + cell.w - x).max(1.0);
                    let h = (c.size[1] * sy).min(cell.y + cell.h - y).max(1.0);
                    let color = if c.focus_history_id == 0 { &self.cfg.focused_window_color } else { &fg };
                    s.rect(x, y, w, h, &format!(r#"id="win-{}" fill="{color}" fill-opacity="0.75" stroke="{}" stroke-width="0.5""#, esc(&c.address), self.cfg.workspace_color));
                }
            }
            if self.cfg.show_numbers {
                let size = (cell.h * 0.6).min(10.0);
                s.text(cell.x + cell.w / 2.0, cell.y + (cell.h + size) / 2.0 - 1.0, &ws.to_string(), &format!(r#"font-size="{}" text-anchor="middle" fill="{fg}""#, num(size)));
            }
            s.close_group();
        }
        s.finish()
    }
}

fn main() {
    dool_plugin_sdk::run::<Workspaces>(
        dool_plugin_sdk::Describe { name: "workspaces", description: "grid of Hyprland workspaces with their windows", version: env!("CARGO_PKG_VERSION") },
        DEFAULT_CONFIG,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dool_plugin_sdk::hypr::{Workspace, WorkspaceRef};
    use dool_plugin_sdk::{Pointer, Style};

    fn frame() -> Frame {
        Frame {
            instance: "ws".into(),
            bar: "main".into(),
            output: "DP-1".into(),
            edge: "top".into(),
            orientation: "horizontal".into(),
            width: 104.0,
            height: 30.0,
            scale: 1.0,
            style: Style { color: "#ffffff".into(), ..Default::default() },
            config_dir: "/tmp".into(),
        }
    }

    fn plugin() -> Workspaces {
        let mon = Monitor { id: 0, name: "DP-1".into(), width: 1600.0, height: 900.0, x: 0.0, y: 0.0, scale: 1.0, focused: true, active_workspace: WorkspaceRef { id: 2, name: "2".into() }, ..Default::default() };
        let hdmi = Monitor { id: 1, name: "HDMI-A-2".into(), width: 1920.0, height: 1080.0, x: 1600.0, y: 0.0, scale: 1.0, active_workspace: WorkspaceRef { id: 24, name: "24".into() }, ..Default::default() };
        let ws = |id: i64, monitor: &str| Workspace { id, name: id.to_string(), monitor: monitor.into(), windows: 0 };
        let mut workspaces: Vec<Workspace> = (1..=7).map(|id| ws(id, "DP-1")).collect();
        workspaces.extend([ws(24, "HDMI-A-2"), ws(18, "HDMI-A-2"), ws(-98, "HDMI-A-2")]);
        let client = |addr: &str, ws: i64, at: [f64; 2], size: [f64; 2], focus: i64| Client {
            address: addr.into(),
            mapped: true,
            at,
            size,
            workspace: WorkspaceRef { id: ws, name: ws.to_string() },
            class: "kitty".into(),
            title: format!("term {addr}"),
            focus_history_id: focus,
            ..Default::default()
        };
        Workspaces {
            cfg: Config::default(),
            hypr: HyprState {
                monitors: vec![mon, hdmi],
                workspaces,
                clients: vec![
                    client("0xa", 2, [0.0, 0.0], [800.0, 900.0], 0),
                    client("0xb", 2, [800.0, 0.0], [800.0, 900.0], 1),
                    Client { monitor: 1, ..client("0xc", 18, [1600.0, 0.0], [1920.0, 1080.0], 2) },
                ],
            },
            hovered: None,
            error_logged: false,
        }
    }

    #[test]
    fn grid_keeps_monitor_aspect_and_scales_windows_into_cells() {
        let p = plugin();
        let f = frame();
        // 3 × 2 cells in 104 × 30 are height-limited: (30 - 2) / 2 = 14 high, 24.89 wide (16:9).
        let svg = p.render(&f);
        assert_eq!(svg.matches("<g id=\"ws-").count(), 6);
        assert!(svg.contains(r#"<g id="ws-2" >"#));
        // Two side-by-side windows on workspace 2 each take half a cell.
        assert!(svg.contains(r##"id="win-0xa" fill="#f5c2e7""##), "focused window highlighted: {svg}");
        assert!(svg.contains(r#"width="12.44" height="14""#), "{svg}");
    }

    #[test]
    fn hover_highlights_the_cell_and_names_the_window() {
        let mut p = plugin();
        let f = frame();
        let mut out = Out::default();
        let ev = Mouse::Move(Pointer { targets: vec!["win-0xb".into(), "ws-2".into()], inside: true, ..Default::default() });
        assert!(p.mouse(&ev, &f, &mut out));
        assert!(format!("{out:?}").contains("kitty — term 0xb"));
        assert!(p.render(&f).contains(r##"fill="#45475a""##));
        let mut out = Out::default();
        let ev = Mouse::Move(Pointer { targets: vec!["ws-5".into()], inside: true, ..Default::default() });
        p.mouse(&ev, &f, &mut out);
        assert!(format!("{out:?}").contains("Workspace 5 · 0 windows"));
    }

    #[test]
    fn each_display_shows_its_own_workspaces() {
        let p = plugin();
        let f = Frame { output: "HDMI-A-2".into(), ..frame() };
        let svg = p.render(&f);
        // Only HDMI-A-2's workspaces, in id order, without the special one; the rest are empty cells.
        let groups: Vec<&str> = svg.match_indices("<g id=\"ws-").map(|(i, _)| &svg[i + 10..i + 12]).collect();
        assert_eq!(groups, ["18", "24"], "{svg}");
        assert_eq!(svg.matches(r#"fill-opacity="0.4""#).count(), 4);
        assert!(svg.contains("win-0xc") && !svg.contains("win-0xa"), "{svg}");
        assert_eq!(p.active_workspace(&f), Some(24));
        // On DP-1 the grid is that display's first six workspaces.
        let svg = p.render(&frame());
        assert!(svg.contains(r#"<g id="ws-6" >"#) && !svg.contains("ws-7") && !svg.contains("ws-18"), "{svg}");
    }
}
