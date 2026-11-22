//! Bar copies: one layer-shell window per (bar config × matching display), created and
//! destroyed as displays come and go.

use crate::config::{BarConfig, Config, Edge, Layer, Section};
use crate::slot::{Slot, SlotSpec};
use gtk4::{gdk, glib, prelude::*};
use gtk4_layer_shell::LayerShell;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

pub struct Bars {
    app: gtk4::Application,
    config: Config,
    plugins: BTreeMap<String, PathBuf>,
    /// Keyed by (bar name, connector); connector is "" for "compositor's choice".
    copies: RefCell<BTreeMap<(String, String), BarCopy>>,
}

struct BarCopy {
    window: gtk4::Window,
    slots: Vec<Slot>,
}

impl Drop for BarCopy {
    fn drop(&mut self) {
        // Dropping the slots stops their plugins.
        self.window.destroy();
    }
}

fn monitors() -> Vec<(String, gdk::Monitor)> {
    let Some(display) = gdk::Display::default() else { return vec![] };
    let list = display.monitors();
    (0..list.n_items())
        .filter_map(|i| list.item(i).and_downcast::<gdk::Monitor>())
        .filter_map(|m| m.connector().map(|c| (c.to_string(), m)))
        .collect()
}

/// Which bar copies should exist for the given connectors.
pub fn wanted(bars: &[BarConfig], connectors: &[String]) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for bar in bars {
        match bar.output.as_str() {
            "*" => out.extend(connectors.iter().map(|c| (bar.name.clone(), c.clone()))),
            "" => {
                out.insert((bar.name.clone(), String::new()));
            }
            name if connectors.iter().any(|c| c == name) => {
                out.insert((bar.name.clone(), name.to_owned()));
            }
            name => log::info!("bars.{}: display {name} is not connected", bar.name),
        }
    }
    out
}

impl Bars {
    pub fn start(app: &gtk4::Application, config: Config, plugins: BTreeMap<String, PathBuf>) -> Rc<Bars> {
        let bars = Rc::new(Bars { app: app.clone(), config, plugins, copies: RefCell::default() });
        bars.reconcile();
        if let Some(display) = gdk::Display::default() {
            let weak = Rc::downgrade(&bars);
            display.monitors().connect_items_changed(move |_, _, _, _| {
                // Connector names can arrive slightly after the monitor itself.
                for delay in [0, 1000] {
                    let weak = weak.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
                        if let Some(b) = weak.upgrade() {
                            b.reconcile();
                        }
                    });
                }
            });
        }
        bars
    }

    /// Create missing bar copies and remove those whose display went away.
    fn reconcile(&self) {
        let monitors = monitors();
        let connectors: Vec<String> = monitors.iter().map(|(c, _)| c.clone()).collect();
        let wanted = wanted(&self.config.bars, &connectors);
        let mut copies = self.copies.borrow_mut();
        copies.retain(|key, _| {
            let keep = wanted.contains(key);
            if !keep {
                log::info!("removing bar {} on {}", key.0, key.1);
            }
            keep
        });
        for key in wanted {
            if copies.contains_key(&key) {
                continue;
            }
            let bar = self.config.bars.iter().find(|b| b.name == key.0).unwrap();
            let monitor = monitors.iter().find(|(c, _)| *c == key.1).map(|(_, m)| m.clone());
            log::info!("creating bar {} on {}", key.0, if key.1.is_empty() { "the default display" } else { &key.1 });
            copies.insert(key.clone(), self.build(bar, &key.1, monitor.as_ref()));
        }
    }

    /// Tell every plugin about the new computed CSS style.
    pub fn restyle(&self) {
        for copy in self.copies.borrow().values() {
            for slot in &copy.slots {
                slot.send_style();
            }
        }
    }

    fn build(&self, bar: &BarConfig, connector: &str, monitor: Option<&gdk::Monitor>) -> BarCopy {
        let window = gtk4::Window::new();
        self.app.add_window(&window);
        window.add_css_class("dool-window");
        window.init_layer_shell();
        window.set_namespace(Some("days-of-our-lives"));
        window.set_layer(match bar.layer {
            Layer::Background => gtk4_layer_shell::Layer::Background,
            Layer::Bottom => gtk4_layer_shell::Layer::Bottom,
            Layer::Top => gtk4_layer_shell::Layer::Top,
            Layer::Overlay => gtk4_layer_shell::Layer::Overlay,
        });
        window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::None);
        if let Some(m) = monitor {
            window.set_monitor(Some(m));
        }
        use gtk4_layer_shell::Edge as E;
        let (main, sides) = match bar.edge {
            Edge::Top => (E::Top, [E::Left, E::Right]),
            Edge::Bottom => (E::Bottom, [E::Left, E::Right]),
            Edge::Left => (E::Left, [E::Top, E::Bottom]),
            Edge::Right => (E::Right, [E::Top, E::Bottom]),
        };
        window.set_anchor(main, true);
        for e in sides {
            window.set_anchor(e, true);
        }
        for (edge, m) in [E::Top, E::Right, E::Bottom, E::Left].into_iter().zip(bar.margin) {
            window.set_margin(edge, m);
        }

        let vertical = bar.edge.is_vertical();
        let orientation = if vertical { gtk4::Orientation::Vertical } else { gtk4::Orientation::Horizontal };
        let content = gtk4::CenterBox::new();
        content.set_orientation(orientation);
        content.set_widget_name(&bar.name);
        content.add_css_class("bar");
        content.add_css_class(bar.edge.name());
        content.add_css_class(if vertical { "vertical" } else { "horizontal" });
        if !connector.is_empty() {
            content.add_css_class(&format!("output-{connector}"));
        }

        let mut instances: Vec<_> = self.config.instances.iter().filter(|i| i.bars.contains(&bar.name)).collect();
        instances.sort_by(|a, b| (a.section, a.order, &a.name).cmp(&(b.section, b.order, &b.name)));
        let mut slots = Vec::new();
        for (section, class) in [(Section::Start, "start"), (Section::Center, "center"), (Section::End, "end")] {
            let b = gtk4::Box::new(orientation, 0);
            b.add_css_class("section");
            b.add_css_class(class);
            for inst in instances.iter().filter(|i| i.section == section) {
                let slot = Slot::new(SlotSpec {
                    instance: (*inst).clone(),
                    plugin_path: self.plugins.get(&inst.plugin).cloned(),
                    bar: bar.clone(),
                    output: connector.to_owned(),
                });
                if inst.expand {
                    if vertical { b.set_vexpand(true) } else { b.set_hexpand(true) }
                }
                b.append(slot.widget());
                slots.push(slot);
            }
            match section {
                Section::Start => content.set_start_widget(Some(&b)),
                Section::Center => content.set_center_widget(Some(&b)),
                Section::End => content.set_end_widget(Some(&b)),
            }
        }

        // An invisible overlay child always has the window's size, so its resize signal
        // tells us the bar's CSS thickness → reserved space = thickness + gap.
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&content));
        let sizer = gtk4::DrawingArea::new();
        sizer.set_can_target(false);
        overlay.add_overlay(&sizer);
        overlay.set_measure_overlay(&sizer, false);
        let (exclusive, gap) = (bar.exclusive, bar.gap);
        let win = window.downgrade();
        sizer.connect_resize(move |_, w, h| {
            if let Some(win) = win.upgrade() {
                let thickness = if vertical { w } else { h };
                win.set_exclusive_zone(if exclusive { thickness + gap } else { 0 });
            }
        });
        if !exclusive {
            window.set_exclusive_zone(0);
        }
        window.set_child(Some(&overlay));
        window.present();
        BarCopy { window, slots }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn bar(name: &str, output: &str) -> BarConfig {
        BarConfig { name: name.into(), output: output.into(), edge: Edge::Top, layer: Layer::Top, exclusive: true, margin: [0; 4], gap: 0 }
    }

    #[test]
    fn expands_outputs_into_bar_copies() {
        let bars = [bar("all", "*"), bar("one", "DP-1"), bar("gone", "HDMI-A-1"), bar("any", "")];
        let w = wanted(&bars, &["DP-1".into(), "eDP-1".into()]);
        let pairs: Vec<_> = w.iter().map(|(b, c)| format!("{b}@{c}")).collect();
        assert_eq!(pairs, ["all@DP-1", "all@eDP-1", "any@", "one@DP-1"]);
    }
}
