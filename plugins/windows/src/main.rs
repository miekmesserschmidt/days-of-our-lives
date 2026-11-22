//! dool-plugin-windows: an icon for each window on the active workspace of this bar's
//! display, in a `columns × rows` grid. Click to focus; hover for the title.

mod icons;

use anyhow::Result;
use dool_plugin_sdk::hypr::{self, Client, Monitor};
use dool_plugin_sdk::svg::{check_color, esc, num, Svg};
use dool_plugin_sdk::{Frame, Mouse, Out, Plugin, Rect, Tx};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

const DEFAULT_CONFIG: &str = r##"plugin = "windows"
# At most columns*rows icons; with more windows the last cell shows "+N".
columns = 4
rows = 1
# position (left→right, top→bottom on screen) | focus (most recent first) | class
order = "position"
# "" = the default (GTK) icon theme
icon_theme = ""
highlight_focused = true
# "" = the CSS text colour of the slot.
highlight_color = ""
hover_color = "#45475a"
tooltip = true
middle_click_closes = false

[position]
bar = "main"
section = "end"
"##;

const EVENTS: &[&str] = &[
    "workspacev2", "focusedmonv2", "openwindow", "closewindow", "movewindowv2",
    "activewindowv2", "windowtitlev2", "changefloatingmode", "monitoraddedv2", "monitorremovedv2",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    columns: u32,
    rows: u32,
    order: String,
    icon_theme: String,
    highlight_focused: bool,
    highlight_color: String,
    hover_color: String,
    tooltip: bool,
    middle_click_closes: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            columns: 4,
            rows: 1,
            order: "position".into(),
            icon_theme: String::new(),
            highlight_focused: true,
            highlight_color: String::new(),
            hover_color: "#45475a".into(),
            tooltip: true,
            middle_click_closes: false,
        }
    }
}

struct Windows {
    cfg: Config,
    theme: String,
    desktop: Vec<icons::DesktopEntry>,
    icon_cache: HashMap<String, Option<PathBuf>>,
    monitors: Vec<Monitor>,
    clients: Vec<Client>,
    hovered: Option<String>,
    error_logged: bool,
}

/// What a grid cell shows.
enum Cell<'a> {
    Window(&'a Client),
    More(Vec<&'a Client>),
}

impl Windows {
    fn bar_monitor(&self, frame: &Frame) -> Option<&Monitor> {
        let m = &self.monitors;
        m.iter().find(|m| m.name == frame.output).or_else(|| m.iter().find(|m| m.focused)).or(m.first())
    }

    fn windows(&self, frame: &Frame) -> Vec<&Client> {
        let Some(ws) = self.bar_monitor(frame).map(|m| m.active_workspace.id) else { return vec![] };
        let mut v: Vec<&Client> = self.clients.iter().filter(|c| c.mapped && !c.hidden && c.workspace.id == ws).collect();
        match self.cfg.order.as_str() {
            "focus" => v.sort_by_key(|c| c.focus_history_id),
            "class" => v.sort_by(|a, b| (a.class.to_lowercase(), &a.title).cmp(&(b.class.to_lowercase(), &b.title))),
            _ => v.sort_by(|a, b| (a.at[0], a.at[1]).partial_cmp(&(b.at[0], b.at[1])).unwrap_or(std::cmp::Ordering::Equal)),
        }
        v
    }

    fn cells(&self, frame: &Frame) -> Vec<Cell<'_>> {
        let windows = self.windows(frame);
        let max = (self.cfg.columns * self.cfg.rows) as usize;
        if windows.len() <= max {
            return windows.into_iter().map(Cell::Window).collect();
        }
        let mut cells: Vec<Cell> = windows[..max - 1].iter().map(|c| Cell::Window(c)).collect();
        cells.push(Cell::More(windows[max - 1..].to_vec()));
        cells
    }

    fn cell_rect(&self, frame: &Frame, i: usize) -> Rect {
        let (cols, rows) = (self.cfg.columns as f64, self.cfg.rows as f64);
        let (cw, ch) = (frame.width / cols, frame.height / rows);
        let (c, r) = ((i as u32 % self.cfg.columns) as f64, (i as u32 / self.cfg.columns) as f64);
        Rect { x: c * cw, y: r * ch, w: cw, h: ch }
    }

    fn icon_size(&self, frame: &Frame) -> f64 {
        let r = self.cell_rect(frame, 0);
        (r.w.min(r.h) - 4.0).max(4.0)
    }

    fn refresh(&mut self) -> Result<bool> {
        let monitors: Vec<Monitor> = hypr::query("monitors")?;
        let clients: Vec<Client> = hypr::query("clients")?;
        let changed = monitors != self.monitors || clients != self.clients;
        (self.monitors, self.clients) = (monitors, clients);
        Ok(changed)
    }

    fn resolve_icons(&mut self, size: u16) {
        for c in &self.clients {
            if !self.icon_cache.contains_key(&c.class) {
                let icon = icons::resolve(&self.desktop, &c.class, &self.theme, size);
                self.icon_cache.insert(c.class.clone(), icon);
            }
        }
    }
}

impl Plugin for Windows {
    type Config = Config;
    type Msg = ();

    fn start(cfg: Config, frame: &Frame, tx: Tx<()>) -> Result<Self> {
        for (key, c) in [("hover_color", &cfg.hover_color), ("highlight_color", &cfg.highlight_color)] {
            if !c.is_empty() {
                check_color(c).map_err(|e| anyhow::anyhow!("{key}: {e}"))?;
            }
        }
        anyhow::ensure!(cfg.columns > 0 && cfg.rows > 0, "columns and rows must be at least 1");
        anyhow::ensure!(["position", "focus", "class"].contains(&cfg.order.as_str()), "order: expected position | focus | class, got {:?}", cfg.order);
        let theme = if cfg.icon_theme.is_empty() { freedesktop_icons::default_theme_gtk().unwrap_or_else(|| "hicolor".into()) } else { cfg.icon_theme.clone() };
        hypr::watch(EVENTS, tx, || ());
        let mut w = Windows {
            cfg,
            theme,
            desktop: icons::index(&icons::application_dirs()),
            icon_cache: HashMap::new(),
            monitors: vec![],
            clients: vec![],
            hovered: None,
            error_logged: false,
        };
        w.update((), frame);
        Ok(w)
    }

    fn update(&mut self, _: (), frame: &Frame) -> bool {
        match self.refresh() {
            Ok(changed) => {
                self.error_logged = false;
                let size = (self.icon_size(frame) * frame.scale.max(1.0)).round() as u16;
                self.resolve_icons(size.max(16));
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
        let target = ev.pointer().and_then(|p| p.targets.first().cloned()).filter(|t| t.starts_with("win-") || t == "more");
        match ev {
            Mouse::Click(_, button) => {
                let Some(addr) = target.as_deref().and_then(|t| t.strip_prefix("win-")) else { return false };
                let sel = hypr::lua_str(&format!("address:{addr}"));
                let lua = match button {
                    1 => format!("hl.dsp.focus({{ window = {sel} }})"),
                    2 if self.cfg.middle_click_closes => format!("hl.dsp.window.close({sel})"),
                    _ => return false,
                };
                if let Err(e) = hypr::dispatch(&lua) {
                    log::warn!("{e:#}");
                }
                false
            }
            Mouse::Leave => self.hovered.take().is_some(),
            Mouse::Enter(_) | Mouse::Move(_) => {
                if self.cfg.tooltip {
                    let cells = self.cells(frame);
                    let found = cells.iter().enumerate().find(|(_, cell)| match cell {
                        Cell::Window(c) => target.as_deref() == Some(&format!("win-{}", c.address)),
                        Cell::More(_) => target.as_deref() == Some("more"),
                    });
                    match found {
                        Some((i, Cell::Window(c))) => out.tooltip(format!("{}\n{}", c.title, c.class), Some(self.cell_rect(frame, i))),
                        Some((i, Cell::More(rest))) => {
                            let titles: Vec<String> = rest.iter().map(|c| format!("{} — {}", c.class, c.title)).collect();
                            out.tooltip(titles.join("\n"), Some(self.cell_rect(frame, i)))
                        }
                        None => out.hide_tooltip(),
                    }
                }
                let changed = target != self.hovered;
                self.hovered = target;
                changed
            }
            _ => false,
        }
    }

    fn render(&self, frame: &Frame) -> String {
        let mut s = Svg::new(frame.width, frame.height);
        let fg = if frame.style.color.is_empty() { "#cdd6f4".to_owned() } else { frame.style.color.clone() };
        let highlight = if self.cfg.highlight_color.is_empty() { fg.clone() } else { self.cfg.highlight_color.clone() };
        let size = self.icon_size(frame);
        for (i, cell) in self.cells(frame).iter().enumerate() {
            let r = self.cell_rect(frame, i);
            let id = match cell {
                Cell::Window(c) => format!("win-{}", c.address),
                Cell::More(_) => "more".to_owned(),
            };
            s.open_group(&id, "");
            let bg = if self.hovered.as_deref() == Some(id.as_str()) { self.cfg.hover_color.as_str() } else { "#000" };
            let opacity = if bg == "#000" { "0.01" } else { "1" };
            s.rect(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0, &format!(r#"rx="4" fill="{bg}" fill-opacity="{opacity}""#));
            let (ix, iy) = (r.x + (r.w - size) / 2.0, r.y + (r.h - size) / 2.0);
            match cell {
                Cell::Window(c) => {
                    match self.icon_cache.get(&c.class).cloned().flatten() {
                        Some(path) => {
                            s.image(ix, iy, size, size, &path.to_string_lossy(), "");
                        }
                        None => {
                            let letter: String = c.class.chars().next().unwrap_or('?').to_uppercase().collect();
                            s.rect(ix, iy, size, size, r##"rx="4" fill="#585b70""##);
                            s.text(ix + size / 2.0, iy + size * 0.72, &letter, &format!(r#"font-size="{}" text-anchor="middle" fill="{fg}""#, num(size * 0.65)));
                        }
                    }
                    if self.cfg.highlight_focused && c.focus_history_id == 0 {
                        s.rect(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0, &format!(r#"rx="4" fill="none" stroke="{}" stroke-width="1.5""#, esc(&highlight)));
                    }
                }
                Cell::More(rest) => {
                    s.text(r.x + r.w / 2.0, r.y + (r.h + size * 0.5) / 2.0, &format!("+{}", rest.len()), &format!(r#"font-size="{}" text-anchor="middle" fill="{fg}""#, num(size * 0.5)));
                }
            }
            s.close_group();
        }
        s.finish()
    }
}

fn main() {
    dool_plugin_sdk::run::<Windows>(
        dool_plugin_sdk::Describe { name: "windows", description: "an icon per window on the active workspace", version: env!("CARGO_PKG_VERSION") },
        DEFAULT_CONFIG,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dool_plugin_sdk::hypr::WorkspaceRef;
    use dool_plugin_sdk::{Pointer, Style};

    fn frame() -> Frame {
        Frame {
            instance: "win".into(),
            bar: "main".into(),
            output: "DP-1".into(),
            edge: "top".into(),
            orientation: "horizontal".into(),
            width: 96.0,
            height: 32.0,
            scale: 1.0,
            style: Style { color: "#ffffff".into(), ..Default::default() },
            config_dir: "/tmp".into(),
        }
    }

    fn plugin(n: usize) -> Windows {
        let mon = Monitor { name: "DP-1".into(), focused: true, active_workspace: WorkspaceRef { id: 1, name: "1".into() }, ..Default::default() };
        let clients = (0..n)
            .map(|i| Client {
                address: format!("0x{i}"),
                mapped: true,
                at: [(n - i) as f64 * 100.0, 0.0],
                workspace: WorkspaceRef { id: 1, name: "1".into() },
                class: format!("app{i}"),
                title: format!("title {i}"),
                focus_history_id: i as i64,
                ..Default::default()
            })
            .collect();
        Windows {
            cfg: Config { columns: 3, ..Default::default() },
            theme: "hicolor".into(),
            desktop: vec![],
            icon_cache: HashMap::new(),
            monitors: vec![mon],
            clients,
            hovered: None,
            error_logged: false,
        }
    }

    #[test]
    fn orders_by_screen_position_and_overflows_into_plus_n() {
        let p = plugin(5);
        let svg = p.render(&frame());
        let order: Vec<&str> = svg.match_indices("<g id=\"").map(|(i, _)| &svg[i + 7..i + 11]).collect();
        assert_eq!(order, ["win-", "win-", "more"]);
        assert!(svg.find("win-0x4").unwrap() < svg.find("win-0x3").unwrap(), "leftmost window first");
        assert!(svg.contains(">+3<"), "{svg}");
        assert!(svg.contains(">A<"), "letter fallback without an icon: {svg}");
    }

    #[test]
    fn hover_tooltips_list_titles() {
        let mut p = plugin(5);
        let f = frame();
        let mv = |t: &str| Mouse::Move(Pointer { targets: vec![t.into()], inside: true, ..Default::default() });
        let mut out = Out::default();
        assert!(p.mouse(&mv("win-0x4"), &f, &mut out));
        assert!(format!("{out:?}").contains("title 4\\napp4"));
        let mut out = Out::default();
        p.mouse(&mv("more"), &f, &mut out);
        let dbg = format!("{out:?}");
        assert!(dbg.contains("app2 — title 2") && dbg.contains("app0 — title 0"), "{dbg}");
    }
}
