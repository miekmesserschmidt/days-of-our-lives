//! A slot: the `DrawingArea` one plugin instance draws into on one bar copy.
//! It owns the plugin process (restarting it after crashes), paints the latest SVG
//! frame, forwards all mouse input in SVG units, and shows the plugin's tooltip.

use crate::config::{BarConfig, Instance};
use crate::plugin_host::{HostEvent, PluginProcess};
use crate::render::Frame;
use crate::style::widget_style;
use dool_plugin_sdk::protocol::{BarMsg, Init, PluginMsg, Pointer, Rect, PROTOCOL_VERSION};
use gtk4::{gdk, glib, prelude::*};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Max pointer travel (px) between press and release that still counts as a click.
const CLICK_SLOP: f64 = 4.0;

pub struct SlotSpec {
    pub instance: Instance,
    /// `None` when the plugin is not installed.
    pub plugin_path: Option<PathBuf>,
    pub bar: BarConfig,
    pub output: String,
}

pub struct Slot(Rc<Inner>);

struct Inner {
    da: gtk4::DrawingArea,
    spec: SlotSpec,
    label: String,
    st: RefCell<State>,
}

#[derive(Default)]
struct State {
    frame: Option<Frame>,
    process: Option<PluginProcess>,
    /// Bumped per process, so events of an old process are ignored.
    generation: u64,
    started_at: Option<Instant>,
    backoff_secs: u64,
    tooltip: Option<(String, Option<Rect>)>,
    /// Last pointer position in slot coordinates, while inside or dragging.
    pointer: Option<(f64, f64)>,
    last_move: Option<(f64, f64, Vec<String>)>,
    move_pending: bool,
    /// (button, x, y) of the press that started a drag.
    pressed: Option<(u32, f64, f64)>,
    left_while_pressed: bool,
    sent_size: (i32, i32, f64),
}

fn dom_bit(button: u32) -> u32 {
    match button {
        1 => 1,
        3 => 2,
        2 => 4,
        8 => 8,
        9 => 16,
        _ => 0,
    }
}

fn mods(state: gdk::ModifierType) -> Vec<String> {
    let mut m = Vec::new();
    for (flag, name) in [
        (gdk::ModifierType::SHIFT_MASK, "shift"),
        (gdk::ModifierType::CONTROL_MASK, "ctrl"),
        (gdk::ModifierType::ALT_MASK, "alt"),
        (gdk::ModifierType::SUPER_MASK, "super"),
    ] {
        if state.contains(flag) {
            m.push(name.to_owned());
        }
    }
    m
}

impl Slot {
    pub fn new(spec: SlotSpec) -> Slot {
        let da = gtk4::DrawingArea::new();
        da.set_widget_name(&spec.instance.name);
        da.add_css_class("slot");
        da.add_css_class(&format!("plugin-{}", spec.instance.plugin));
        if spec.instance.expand {
            if spec.bar.edge.is_vertical() { da.set_vexpand(true) } else { da.set_hexpand(true) }
        }
        da.set_has_tooltip(true);
        let label = format!("{}@{}/{}", spec.instance.name, spec.bar.name, if spec.output.is_empty() { "default" } else { &spec.output });
        let inner = Rc::new(Inner { da: da.clone(), spec, label, st: RefCell::default() });
        if inner.spec.plugin_path.is_none() {
            log::warn!("[{}] plugin {:?} is not installed (no dool-plugin-{} on the plugin path)", inner.label, inner.spec.instance.plugin, inner.spec.instance.plugin);
            da.add_css_class("missing");
        }
        Inner::connect(&inner);
        Slot(inner)
    }

    pub fn widget(&self) -> &gtk4::DrawingArea {
        &self.0.da
    }

    /// Send the current computed CSS text style (after a stylesheet reload).
    pub fn send_style(&self) {
        let style = widget_style(&self.0.da);
        self.0.send(&BarMsg::Style(style));
        self.0.da.queue_draw();
    }
}

impl Inner {
    fn connect(this: &Rc<Inner>) {
        let da = &this.da;
        let w = Rc::downgrade(this);
        da.set_draw_func(move |da, cr, width, height| {
            let Some(s) = w.upgrade() else { return };
            let scale = surface_scale(da);
            s.check_size(width, height, scale);
            let mut st = s.st.borrow_mut();
            if let Some(frame) = st.frame.as_mut() {
                frame.paint(cr, width, height, scale);
            }
        });

        let w = Rc::downgrade(this);
        da.connect_resize(move |da, width, height| {
            if let Some(s) = w.upgrade() {
                s.check_size(width, height, surface_scale(da));
            }
        });

        let w = Rc::downgrade(this);
        da.connect_query_tooltip(move |da, _x, _y, _kb, tooltip| {
            let Some(s) = w.upgrade() else { return false };
            let st = s.st.borrow();
            let Some((text, anchor)) = st.tooltip.as_ref() else { return false };
            tooltip.set_text(Some(text));
            if let (Some(a), Some(frame)) = (anchor, st.frame.as_ref()) {
                let fit = frame.fit(da.width() as f64, da.height() as f64);
                let (x0, y0) = fit.to_slot(a.x, a.y);
                let (x1, y1) = fit.to_slot(a.x + a.w, a.y + a.h);
                tooltip.set_tip_area(&gdk::Rectangle::new(x0 as i32, y0 as i32, (x1 - x0).max(1.0) as i32, (y1 - y0).max(1.0) as i32));
            }
            true
        });

        // Hover: enter / move / leave.
        let motion = gtk4::EventControllerMotion::new();
        let w = Rc::downgrade(this);
        motion.connect_enter(move |c, x, y| {
            let Some(s) = w.upgrade() else { return };
            s.st.borrow_mut().left_while_pressed = false;
            s.st.borrow_mut().pointer = Some((x, y));
            let p = s.pointer(x, y, c.current_event_state());
            s.st.borrow_mut().last_move = Some((p.x, p.y, p.targets.clone()));
            s.send(&BarMsg::MouseEnter(p));
        });
        let w = Rc::downgrade(this);
        motion.connect_motion(move |c, x, y| {
            let Some(s) = w.upgrade() else { return };
            if s.st.borrow().pressed.is_some() {
                return; // drag-update reports moves while a button is held
            }
            s.st.borrow_mut().pointer = Some((x, y));
            s.queue_move(c.current_event_state());
        });
        let w = Rc::downgrade(this);
        motion.connect_leave(move |_| {
            let Some(s) = w.upgrade() else { return };
            if s.st.borrow().pressed.is_some() {
                s.st.borrow_mut().left_while_pressed = true; // reported after mouseup
                return;
            }
            s.leave();
        });
        da.add_controller(motion);

        // Buttons: a drag gesture always reports the release, even outside the slot.
        let drag = gtk4::GestureDrag::new();
        drag.set_button(0);
        let w = Rc::downgrade(this);
        drag.connect_drag_begin(move |g, x, y| {
            let Some(s) = w.upgrade() else { return };
            let button = g.current_button();
            s.st.borrow_mut().pressed = Some((button, x, y));
            s.st.borrow_mut().pointer = Some((x, y));
            let pointer = s.pointer(x, y, g.current_event_state());
            s.send(&BarMsg::MouseDown { pointer, button });
        });
        let w = Rc::downgrade(this);
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(s) = w.upgrade() else { return };
            let Some((_, x0, y0)) = s.st.borrow().pressed else { return };
            s.st.borrow_mut().pointer = Some((x0 + dx, y0 + dy));
            s.queue_move(g.current_event_state());
        });
        let w = Rc::downgrade(this);
        drag.connect_drag_end(move |g, dx, dy| {
            let Some(s) = w.upgrade() else { return };
            let Some((button, x0, y0)) = s.st.borrow_mut().pressed.take() else { return };
            let (x, y) = (x0 + dx, y0 + dy);
            let mods = g.current_event_state();
            // `buttons` still shows the released button on mouseup, like the DOM's event order.
            let mut pointer = s.pointer(x, y, mods);
            pointer.buttons |= dom_bit(button);
            s.send(&BarMsg::MouseUp { pointer: pointer.clone(), button });
            let in_slot = x >= 0.0 && y >= 0.0 && x <= s.da.width() as f64 && y <= s.da.height() as f64;
            if in_slot && dx.hypot(dy) < CLICK_SLOP {
                pointer.buttons &= !dom_bit(button);
                s.send(&BarMsg::Click { pointer, button });
            }
            if std::mem::take(&mut s.st.borrow_mut().left_while_pressed) {
                s.leave();
            }
        });
        da.add_controller(drag);

        // Wheel / touchpad, in whole lines.
        let scroll = gtk4::EventControllerScroll::new(
            gtk4::EventControllerScrollFlags::BOTH_AXES | gtk4::EventControllerScrollFlags::DISCRETE,
        );
        let w = Rc::downgrade(this);
        scroll.connect_scroll(move |c, dx, dy| {
            let Some(s) = w.upgrade() else { return glib::Propagation::Proceed };
            let (x, y) = s.st.borrow().pointer.unwrap_or_default();
            let pointer = s.pointer(x, y, c.current_event_state());
            s.send(&BarMsg::Wheel { pointer, dx, dy });
            glib::Propagation::Stop
        });
        da.add_controller(scroll);
    }

    fn send(&self, msg: &BarMsg) {
        if let Some(p) = self.st.borrow_mut().process.as_mut() {
            p.send(msg);
        }
    }

    /// Build the pointer fields for slot coordinates (x, y).
    fn pointer(&self, x: f64, y: f64, state: gdk::ModifierType) -> Pointer {
        let st = self.st.borrow();
        let (sx, sy, inside, targets) = match st.frame.as_ref() {
            Some(f) => {
                let (sx, sy, inside) = f.fit(self.da.width() as f64, self.da.height() as f64).to_svg(x, y);
                (sx, sy, inside, if inside { f.hit_test(sx, sy) } else { vec![] })
            }
            None => (x, y, false, vec![]),
        };
        let buttons = st.pressed.map(|(b, _, _)| dom_bit(b)).unwrap_or(0);
        let round = |v: f64| (v * 100.0).round() / 100.0;
        Pointer { x: round(sx), y: round(sy), inside, targets, buttons, mods: mods(state) }
    }

    /// Coalesce moves to at most one per frame, sent only when something changed.
    fn queue_move(self: &Rc<Self>, state: gdk::ModifierType) {
        if std::mem::replace(&mut self.st.borrow_mut().move_pending, true) {
            return;
        }
        let w = Rc::downgrade(self);
        self.da.add_tick_callback(move |_, _| {
            if let Some(s) = w.upgrade() {
                s.st.borrow_mut().move_pending = false;
                s.flush_move(state);
            }
            glib::ControlFlow::Break
        });
    }

    fn flush_move(&self, state: gdk::ModifierType) {
        let Some((x, y)) = self.st.borrow().pointer else { return };
        let p = self.pointer(x, y, state);
        let key = Some((p.x, p.y, p.targets.clone()));
        if self.st.borrow().last_move != key {
            self.st.borrow_mut().last_move = key;
            self.send(&BarMsg::MouseMove(p));
        }
    }

    fn leave(&self) {
        {
            let mut st = self.st.borrow_mut();
            st.pointer = None;
            st.last_move = None;
        }
        self.send(&BarMsg::MouseLeave);
    }

    fn check_size(self: &Rc<Self>, width: i32, height: i32, scale: f64) {
        if width <= 0 || height <= 0 {
            return;
        }
        let size = (width, height, scale);
        let (started, changed) = {
            let st = self.st.borrow();
            (st.started_at.is_some() || st.process.is_some(), st.sent_size != size)
        };
        if !changed {
            return;
        }
        self.st.borrow_mut().sent_size = size;
        if started {
            self.send(&BarMsg::Resize { width: width as f64, height: height as f64, scale });
        } else {
            // First allocation: now the plugin can be started with real sizes. Deferred to
            // idle so it never runs inside a draw or allocation.
            self.st.borrow_mut().started_at = Some(Instant::now());
            let w = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(s) = w.upgrade() {
                    s.start();
                }
            });
        }
    }

    fn start(self: &Rc<Self>) {
        let Some(path) = self.spec.plugin_path.clone() else { return };
        let (width, height, scale) = self.st.borrow().sent_size;
        let i = &self.spec.instance;
        let init = BarMsg::Init(Init {
            protocol: PROTOCOL_VERSION,
            instance: i.name.clone(),
            bar: self.spec.bar.name.clone(),
            output: self.spec.output.clone(),
            edge: self.spec.bar.edge.name().into(),
            orientation: if self.spec.bar.edge.is_vertical() { "vertical" } else { "horizontal" }.into(),
            width: width as f64,
            height: height as f64,
            scale,
            style: widget_style(&self.da),
            config: i.config.clone(),
            config_dir: i.config_dir.display().to_string(),
        });
        let env = [
            ("DOOL_PROTOCOL", PROTOCOL_VERSION.to_string()),
            ("DOOL_INSTANCE", i.name.clone()),
            ("DOOL_BAR", self.spec.bar.name.clone()),
            ("DOOL_OUTPUT", self.spec.output.clone()),
        ];
        match PluginProcess::spawn(&path, &self.label, &env, &init) {
            Ok((process, rx)) => {
                let generation = {
                    let mut st = self.st.borrow_mut();
                    st.process = Some(process);
                    st.generation += 1;
                    st.started_at = Some(Instant::now());
                    st.generation
                };
                log::info!("[{}] started {}", self.label, path.display());
                let w = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    while let Ok(first) = rx.recv().await {
                        let mut events = vec![first];
                        while let Ok(more) = rx.try_recv() {
                            events.push(more);
                        }
                        let Some(s) = w.upgrade() else { return };
                        if s.st.borrow().generation != generation {
                            return;
                        }
                        s.handle(events);
                    }
                });
            }
            Err(e) => {
                log::error!("[{}] cannot start {}: {e}", self.label, path.display());
                self.crashed();
            }
        }
    }

    fn handle(self: &Rc<Self>, events: Vec<HostEvent>) {
        // Only the newest frame of a batch is worth parsing ("latest frame wins").
        let last_svg = events.iter().rposition(|e| matches!(e, HostEvent::Msg(PluginMsg::Svg { .. })));
        for (n, event) in events.into_iter().enumerate() {
            match event {
                HostEvent::Msg(PluginMsg::Svg { svg }) if Some(n) == last_svg => self.set_svg(&svg),
                HostEvent::Msg(PluginMsg::Svg { .. }) | HostEvent::Msg(PluginMsg::Unknown) => {}
                HostEvent::Msg(PluginMsg::Tooltip { text, anchor }) => {
                    self.st.borrow_mut().tooltip = text.map(|t| (t, anchor));
                    self.da.trigger_tooltip_query();
                }
                HostEvent::Msg(PluginMsg::Log { level, message }) => {
                    let level = match level.as_str() {
                        "error" => log::Level::Error,
                        "warn" => log::Level::Warn,
                        "debug" => log::Level::Debug,
                        _ => log::Level::Info,
                    };
                    log::log!(level, "[{}] {message}", self.label);
                }
                HostEvent::Exited(status) => {
                    log::warn!("[{}] plugin exited: {status}", self.label);
                    self.crashed();
                }
            }
        }
    }

    fn set_svg(&self, svg: &str) {
        match Frame::parse(svg) {
            Ok(frame) => {
                self.st.borrow_mut().frame = Some(frame);
                self.da.remove_css_class("crashed");
                self.da.queue_draw();
                // Content may have moved under a still pointer: keep hover state right.
                if self.st.borrow().pointer.is_some() && self.st.borrow().pressed.is_none() {
                    self.flush_move(gdk::ModifierType::empty());
                }
            }
            Err(e) => log::warn!("[{}] invalid SVG ignored: {e}", self.label),
        }
    }

    /// The process is gone: mark the slot, then restart with backoff 1, 2, 4 … 30 s.
    fn crashed(self: &Rc<Self>) {
        let delay = {
            let mut st = self.st.borrow_mut();
            st.process = None;
            st.generation += 1;
            st.tooltip = None;
            let healthy = st.started_at.is_some_and(|t| t.elapsed() > Duration::from_secs(60));
            st.backoff_secs = if healthy || st.backoff_secs == 0 { 1 } else { (st.backoff_secs * 2).min(30) };
            st.backoff_secs
        };
        self.da.add_css_class("crashed");
        self.da.trigger_tooltip_query();
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(delay), move || {
            if let Some(s) = w.upgrade() {
                s.start();
            }
        });
    }
}

fn surface_scale(w: &impl IsA<gtk4::Widget>) -> f64 {
    w.native().and_then(|n| n.surface()).map(|s| s.scale()).unwrap_or_else(|| w.scale_factor() as f64)
}

