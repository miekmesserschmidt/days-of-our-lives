//! dool-plugin-calendar: today's events from one or more ICS feeds on a 24 h timeline,
//! each feed in its own colour. The elapsed part of the day is shaded like a progress
//! bar and a "now" line runs across the events. Hovering an event shows its name.

mod ics;
mod layout;

use anyhow::{bail, Result};
use chrono::{Local, NaiveDate, NaiveDateTime};
use dool_plugin_sdk::svg::{check_color, esc, num, Svg};
use dool_plugin_sdk::{tick, Frame, Mouse, Out, Plugin, Tx};
use ics::Event;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_CONFIG: &str = r##"plugin = "calendar"
# horizontal | vertical | auto (= follow the bar)
orientation = "auto"
refresh_minutes = 10
show_all_day = true
hour_labels = true
now_line_color = "#ff5555"
now_line_width = 2
# Shade over the elapsed part of the day (the "progress bar").
past_shade = "#00000066"
# "" = the CSS text colour of the slot.
text_color = ""
event_text_color = "#11111b"
tooltip = true
tooltip_details = true
hover_outline = ""

# One table per feed: `url` (https://, webcal:// or file://) or `url_file`
# (a file that contains a secret URL), plus a colour.
[[calendars]]
name = "work"
url = "https://example.com/work.ics"
color = "#89b4fa"

[position]
bar = "main"
section = "center"
expand = true
"##;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Feed {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    url_file: String,
    color: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    orientation: String,
    refresh_minutes: u64,
    show_all_day: bool,
    hour_labels: bool,
    now_line_color: String,
    now_line_width: f64,
    past_shade: String,
    text_color: String,
    event_text_color: String,
    tooltip: bool,
    tooltip_details: bool,
    hover_outline: String,
    calendars: Vec<Feed>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            orientation: "auto".into(),
            refresh_minutes: 10,
            show_all_day: true,
            hour_labels: true,
            now_line_color: "#ff5555".into(),
            now_line_width: 2.0,
            past_shade: "#00000066".into(),
            text_color: String::new(),
            event_text_color: "#11111b".into(),
            tooltip: true,
            tooltip_details: true,
            hover_outline: String::new(),
            calendars: vec![],
        }
    }
}

enum Msg {
    Fetched(usize, Result<String, String>),
    Tick,
}

struct Calendar {
    cfg: Config,
    feeds: Vec<Option<String>>,
    events: Vec<Event>,
    day: NaiveDate,
    now: NaiveDateTime,
    hovered: Option<usize>,
    last_error: Vec<Option<String>>,
}

fn cache_path(instance: &str, i: usize) -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("days-of-our-lives").join("calendar").join(format!("{instance}-{i}.ics"))
}

fn fetch(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"));
    }
    let url = match url.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_owned(),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
    let mut resp = agent.get(&url).call().map_err(|e| e.to_string())?;
    resp.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())
}

/// Never log secret URLs: only scheme and host.
fn redact(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}/…", rest.split('/').next().unwrap_or("")),
        None => "…".into(),
    }
}

impl Calendar {
    /// Validate the config, with error messages that name the key.
    fn check(cfg: &Config) -> Result<()> {
        for (key, c) in [("now_line_color", &cfg.now_line_color), ("past_shade", &cfg.past_shade), ("event_text_color", &cfg.event_text_color)] {
            check_color(c).map_err(|e| anyhow::anyhow!("{key}: {e}"))?;
        }
        for (key, c) in [("text_color", &cfg.text_color), ("hover_outline", &cfg.hover_outline)] {
            if !c.is_empty() {
                check_color(c).map_err(|e| anyhow::anyhow!("{key}: {e}"))?;
            }
        }
        if !["auto", "horizontal", "vertical"].contains(&cfg.orientation.as_str()) {
            bail!("orientation: expected horizontal | vertical | auto, got {:?}", cfg.orientation);
        }
        for (i, f) in cfg.calendars.iter().enumerate() {
            check_color(&f.color).map_err(|e| anyhow::anyhow!("calendars[{i}].color: {e}"))?;
            if f.url.is_empty() == f.url_file.is_empty() {
                bail!("calendars[{i}]: set exactly one of url and url_file");
            }
        }
        Ok(())
    }

    fn vertical(&self, frame: &Frame) -> bool {
        match self.cfg.orientation.as_str() {
            "vertical" => true,
            "horizontal" => false,
            _ => frame.is_vertical(),
        }
    }

    fn reexpand(&mut self) {
        let mut events = Vec::new();
        for (feed, text) in self.cfg.calendars.iter().zip(&self.feeds) {
            if let Some(text) = text {
                events.extend(ics::expand(text, self.day, &feed.name, &feed.color));
            }
        }
        events.sort_by_key(|e| (e.start, e.end));
        self.events = events;
        self.hovered = None;
    }

    fn layout(&self, frame: &Frame) -> layout::Layout {
        let geo = layout::Geo::new(self.vertical(frame), frame.width, frame.height);
        let font = if frame.style.font_size > 0.0 { frame.style.font_size } else { 11.0 };
        layout::layout(&self.events, self.day, geo, self.cfg.show_all_day, self.cfg.hour_labels, font.min(10.0))
    }

    fn tooltip_text(&self, e: &Event) -> String {
        let title = if e.title.trim().is_empty() { "(no title)" } else { e.title.trim() };
        if !self.cfg.tooltip_details {
            return title.to_owned();
        }
        let when = if e.all_day { "all day".to_owned() } else { format!("{}–{}", e.start.format("%H:%M"), e.end.format("%H:%M")) };
        let mut text = format!("{title}\n{when}");
        if !e.calendar.is_empty() {
            text.push_str(&format!(" · {}", e.calendar));
        }
        if !e.location.trim().is_empty() {
            text.push_str(&format!("\n{}", e.location.trim()));
        }
        text
    }
}

impl Plugin for Calendar {
    type Config = Config;
    type Msg = Msg;

    fn start(cfg: Config, frame: &Frame, tx: Tx<Msg>) -> Result<Self> {
        Calendar::check(&cfg)?;
        let mut urls = Vec::new();
        for (i, f) in cfg.calendars.iter().enumerate() {
            let url = if f.url.is_empty() {
                let path = frame.resolve_path(&f.url_file);
                std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("calendars[{i}].url_file {}: {e}", path.display()))?.trim().to_owned()
            } else {
                f.url.clone()
            };
            urls.push(url);
        }
        // Cached copies make events appear instantly (and offline).
        let feeds: Vec<Option<String>> = (0..urls.len()).map(|i| std::fs::read_to_string(cache_path(&frame.instance, i)).ok()).collect();
        let refresh = Duration::from_secs(cfg.refresh_minutes.max(1) * 60);
        for (i, url) in urls.into_iter().enumerate() {
            let tx = tx.clone();
            std::thread::spawn(move || loop {
                if !tx.send(Msg::Fetched(i, fetch(&url).map_err(|e| format!("{} ({})", e, redact(&url))))) {
                    return;
                }
                std::thread::sleep(refresh);
            });
        }
        tick::every_minute(tx, || Msg::Tick);
        let now = Local::now().naive_local();
        let mut cal = Calendar { last_error: vec![None; feeds.len()], feeds, events: vec![], day: now.date(), now, hovered: None, cfg };
        cal.reexpand();
        Ok(cal)
    }

    fn update(&mut self, msg: Msg, frame: &Frame) -> bool {
        match msg {
            Msg::Tick => {
                self.now = Local::now().naive_local();
                if self.now.date() != self.day {
                    self.day = self.now.date();
                    self.reexpand();
                }
            }
            Msg::Fetched(i, Ok(text)) => {
                self.last_error[i] = None;
                if self.feeds[i].as_ref() == Some(&text) {
                    return false;
                }
                let path = cache_path(&frame.instance, i);
                let _ = std::fs::create_dir_all(path.parent().unwrap()).and_then(|_| std::fs::write(&path, &text));
                self.feeds[i] = Some(text);
                self.reexpand();
            }
            Msg::Fetched(i, Err(e)) => {
                // Keep the last good data; log each distinct error once.
                if self.last_error[i].as_ref() != Some(&e) {
                    log::warn!("calendars[{i}]: fetch failed: {e}");
                    self.last_error[i] = Some(e);
                }
                return false;
            }
        }
        true
    }

    fn mouse(&mut self, ev: &Mouse, frame: &Frame, out: &mut Out) -> bool {
        let hovered = ev.pointer().and_then(|p| p.target("ev-")).and_then(|n| n.parse::<usize>().ok()).filter(|&n| n < self.events.len());
        if matches!(ev, Mouse::Leave) {
            return self.hovered.take().is_some();
        }
        if !matches!(ev, Mouse::Enter(_) | Mouse::Move(_)) || hovered == self.hovered {
            return false;
        }
        self.hovered = hovered;
        if self.cfg.tooltip {
            match hovered {
                Some(n) => out.tooltip(self.tooltip_text(&self.events[n]), Some(self.layout(frame).rects[n])),
                None => out.hide_tooltip(),
            }
        }
        true
    }

    fn render(&self, frame: &Frame) -> String {
        let l = self.layout(frame);
        let geo = l.geo;
        let mut s = Svg::new(frame.width, frame.height);
        let text_color = if self.cfg.text_color.is_empty() { frame.style.color.clone() } else { self.cfg.text_color.clone() };
        let text_color = if text_color.is_empty() { "#cdd6f4".to_owned() } else { text_color };
        let font = (if frame.style.font_size > 0.0 { frame.style.font_size } else { 11.0 }).min(10.0);
        let family = esc(&frame.style.font_family);

        // Hour ticks and labels.
        for h in (0..=24).step_by(l.hour_step as usize) {
            let t = geo.t(h as f64 * 60.0);
            let r = geo.rect(t, t, l.events.0, l.events.1);
            s.line(r.x, r.y, r.x + r.w, r.y + r.h, &format!(r#"stroke="{text_color}" stroke-opacity="0.2" stroke-width="1""#));
            if let (Some((c0, c1)), true) = (l.labels, h < 24) {
                let anchor = if geo.vertical || h == 0 { "start" } else { "middle" };
                let (x, y) = if geo.vertical { (c0 + 1.0, t + font) } else { (t + if h == 0 { 1.0 } else { 0.0 }, c1 - 2.0) };
                s.text(x, y, &format!("{h:02}"), &format!(r#"font-size="{}" font-family="{family}" fill="{text_color}" fill-opacity="0.7" text-anchor="{anchor}""#, num(font)));
            }
        }

        // Events.
        for (i, (e, r)) in self.events.iter().zip(&l.rects).enumerate() {
            if r.w <= 0.0 || r.h <= 0.0 {
                continue;
            }
            let outline = if self.hovered == Some(i) {
                let c = if self.cfg.hover_outline.is_empty() { &text_color } else { &self.cfg.hover_outline };
                format!(r#" stroke="{c}" stroke-width="1.5""#)
            } else {
                String::new()
            };
            s.rect(r.x, r.y, r.w, r.h, &format!(r#"id="ev-{i}" rx="2" fill="{}" fill-opacity="0.85"{outline}"#, e.color));
            let size = font.min(r.h * 0.8);
            if !e.all_day && r.w >= 14.0 && r.h >= 7.0 {
                // A nested <svg> clips the title to its event.
                s.raw(&format!(
                    r#"<svg x="{}" y="{}" width="{}" height="{}"><text x="2" y="{}" font-size="{}" font-family="{family}" fill="{}">{}</text></svg>"#,
                    num(r.x), num(r.y), num(r.w), num(r.h), num((r.h + size) / 2.0 - 1.0), num(size), self.cfg.event_text_color, esc(&e.title)
                ));
            }
        }

        // Progress: shade the elapsed part of the day, then the now line on top.
        let now_t = geo.t(layout::minutes(self.now, self.day));
        let past = geo.rect(0.0, now_t, 0.0, geo.breadth);
        s.rect(past.x, past.y, past.w, past.h, &format!(r#"fill="{}""#, self.cfg.past_shade));
        let line = geo.rect(now_t, now_t, 0.0, geo.breadth);
        s.line(line.x, line.y, line.x + line.w, line.y + line.h, &format!(r#"stroke="{}" stroke-width="{}""#, self.cfg.now_line_color, num(self.cfg.now_line_width)));
        s.finish()
    }
}

fn main() {
    dool_plugin_sdk::run::<Calendar>(
        dool_plugin_sdk::Describe { name: "calendar", description: "today's events from ICS feeds on a timeline", version: env!("CARGO_PKG_VERSION") },
        DEFAULT_CONFIG,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dool_plugin_sdk::{Pointer, Style};

    fn frame(w: f64, h: f64, orientation: &str) -> Frame {
        Frame {
            instance: "cal".into(),
            bar: "main".into(),
            output: "".into(),
            edge: "top".into(),
            orientation: orientation.into(),
            width: w,
            height: h,
            scale: 1.0,
            style: Style { color: "#ffffff".into(), font_family: "Inter".into(), font_size: 10.0, font_weight: 400 },
            config_dir: "/tmp".into(),
        }
    }

    fn cal() -> Calendar {
        let day = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let ev = |h0: u32, h1: u32, title: &str| Event {
            start: day.and_hms_opt(h0, 0, 0).unwrap(),
            end: day.and_hms_opt(h1, 0, 0).unwrap(),
            title: title.into(),
            location: "Room 1".into(),
            calendar: "work".into(),
            color: "#89b4fa".into(),
            all_day: false,
        };
        Calendar {
            cfg: Config::default(),
            feeds: vec![],
            events: vec![ev(9, 10, "Standup"), ev(12, 13, "Lunch")],
            day,
            now: day.and_hms_opt(12, 0, 0).unwrap(),
            hovered: None,
            last_error: vec![],
        }
    }

    fn mv(targets: &[&str]) -> Mouse {
        Mouse::Move(Pointer { targets: targets.iter().map(|t| t.to_string()).collect(), inside: true, ..Default::default() })
    }

    #[test]
    fn renders_ids_now_line_and_shade_in_both_orientations() {
        let c = cal();
        let h = c.render(&frame(480.0, 30.0, "horizontal"));
        assert!(h.contains(r#"id="ev-0""#) && h.contains(r#"id="ev-1""#) && h.contains(">Standup<"));
        // 12:00 is halfway along 480 px: shade 0..240 and the now line at x = 240.
        assert!(h.contains(r##"<rect x="0" y="0" width="240" height="30" fill="#00000066"/>"##), "{h}");
        assert!(h.contains(r#"<line x1="240" y1="0" x2="240" y2="30""#), "{h}");
        let v = c.render(&frame(30.0, 480.0, "vertical"));
        assert!(v.contains(r#"<line x1="0" y1="240" x2="30" y2="240""#), "{v}");
    }

    #[test]
    fn hover_shows_name_tooltip_anchored_at_the_event() {
        let mut c = cal();
        let f = frame(480.0, 30.0, "horizontal");
        let mut out = Out::default();
        assert!(c.mouse(&mv(&["ev-1"]), &f, &mut out));
        let dbg = format!("{out:?}");
        assert!(dbg.contains("Lunch\\n12:00–13:00 · work\\nRoom 1"), "{dbg}");
        assert!(dbg.contains("x: 240.0"), "anchored at the 12:00 event: {dbg}");
        assert!(c.render(&f).contains(r##"id="ev-1" rx="2" fill="#89b4fa" fill-opacity="0.85" stroke="#ffffff""##));

        let mut out = Out::default();
        assert!(!c.mouse(&mv(&["ev-1"]), &f, &mut out), "same event: nothing to do");
        assert_eq!(format!("{out:?}"), "Out { tooltip: None }");

        let mut out = Out::default();
        assert!(c.mouse(&mv(&[]), &f, &mut out));
        assert_eq!(format!("{out:?}"), "Out { tooltip: Some(None) }");
    }

    #[test]
    fn config_errors_are_precise() {
        let cfg: Config = serde_json::from_value(serde_json::json!({"calendars": [{"url": "file:///x.ics", "color": "#12"}]})).unwrap();
        let err = Calendar::check(&cfg).unwrap_err().to_string();
        assert!(err.contains("calendars[0].color") && err.contains("#12"), "{err}");
    }
}
