//! Styling with real GTK4 CSS: a built-in default, then the global and the user
//! `style.css`. Later files win. Both files are watched and reloaded on save.

use dool_plugin_sdk::Style;
use gtk4::{gdk, gio, glib, glib::translate::IntoGlib, pango, prelude::*};
use std::path::PathBuf;

const DEFAULT_CSS: &str = include_str!("default.css");

/// Keeps providers and file monitors alive.
pub struct Styles {
    _providers: Vec<gtk4::CssProvider>,
    _monitors: Vec<gio::FileMonitor>,
}

fn load(provider: &gtk4::CssProvider, path: &PathBuf) {
    if path.exists() {
        provider.load_from_path(path);
    } else {
        provider.load_from_string("");
    }
}

/// `on_reload` runs (once per change burst) after a stylesheet changed.
pub fn install(files: &[PathBuf], on_reload: impl Fn() + 'static) -> Styles {
    let display = gdk::Display::default().expect("a display");
    let default = gtk4::CssProvider::new();
    default.load_from_string(DEFAULT_CSS);
    gtk4::style_context_add_provider_for_display(&display, &default, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);

    let on_reload = std::rc::Rc::new(on_reload);
    let mut providers = vec![default];
    let mut monitors = Vec::new();
    for (i, path) in files.iter().enumerate() {
        let provider = gtk4::CssProvider::new();
        let name = path.display().to_string();
        provider.connect_parsing_error(move |_, section, error| {
            let line = section.start_location().lines() + 1;
            log::warn!("{name}:{line}: {}", error.message());
        });
        load(&provider, path);
        // Later files get a higher priority, all above the application default.
        gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_USER + i as u32);
        match gio::File::for_path(path).monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            Ok(monitor) => {
                let (provider, path, on_reload) = (provider.clone(), path.clone(), on_reload.clone());
                let pending = std::rc::Rc::new(std::cell::Cell::new(false));
                monitor.connect_changed(move |_, _, _, _| {
                    if pending.replace(true) {
                        return;
                    }
                    let (provider, path, on_reload, pending) = (provider.clone(), path.clone(), on_reload.clone(), pending.clone());
                    // Editors write files in several steps; reload once things settle.
                    glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                        pending.set(false);
                        log::info!("reloading {}", path.display());
                        load(&provider, &path);
                        on_reload();
                    });
                });
                monitors.push(monitor);
            }
            Err(e) => log::warn!("cannot watch {}: {e}", path.display()),
        }
        providers.push(provider);
    }
    Styles { _providers: providers, _monitors: monitors }
}

/// The computed text style of a widget, as sent to plugins.
pub fn widget_style(w: &impl IsA<gtk4::Widget>) -> Style {
    let c = w.color();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let color = format!("#{:02x}{:02x}{:02x}{:02x}", byte(c.red()), byte(c.green()), byte(c.blue()), byte(c.alpha()));
    let font = w.pango_context().font_description().unwrap_or_default();
    let size = font.size() as f64 / pango::SCALE as f64;
    let font_size = if font.is_size_absolute() { size } else { size * 96.0 / 72.0 };
    Style {
        color,
        font_family: font.family().map(|f| f.to_string()).unwrap_or_default(),
        font_size: (font_size * 100.0).round() / 100.0,
        font_weight: font.weight().into_glib() as u32,
    }
}
