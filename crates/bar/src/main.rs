//! days-of-our-lives: a simple, plugin based desktop bar for Hyprland.
//! See design/plan.md for the architecture.

mod bars;
mod config;
mod discovery;
mod logging;
mod plugin_host;
mod render;
mod slot;
mod style;

use anyhow::{bail, Context, Result};
use dool_plugin_sdk::protocol::PROTOCOL_VERSION;
use gtk4::{glib, prelude::*};
use std::collections::BTreeMap;
use std::path::PathBuf;

const USAGE: &str = "\
days-of-our-lives — a plugin based desktop bar for Hyprland

USAGE:
    days-of-our-lives [run] [OPTIONS]          start the bar (default)
    days-of-our-lives check-config [OPTIONS]   validate the configuration
    days-of-our-lives plugins list [OPTIONS]   list installed plugins
    days-of-our-lives plugins init <plugin> [instance] [OPTIONS]
                                               write a plugin's default config to the user dir

OPTIONS:
    --config-dir <dir>   user config dir (default: ~/.config/days-of-our-lives)
    --log-level <level>  error | warn | info | debug
    -h, --help           show this help
    -V, --version        show the version
";

struct Args {
    command: Vec<String>,
    config_dir: Option<PathBuf>,
    log_level: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args { command: vec![], config_dir: None, log_level: None };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("days-of-our-lives {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--config-dir" => args.config_dir = Some(it.next().context("--config-dir needs a directory")?.into()),
            "--log-level" => args.log_level = Some(it.next().context("--log-level needs a level")?),
            s if s.starts_with('-') => bail!("unknown option {s}\n\n{USAGE}"),
            _ => args.command.push(a),
        }
    }
    Ok(args)
}

/// SIGTERM/SIGINT/SIGHUP are blocked in every thread and picked up by one `sigwait`
/// thread, which hands them to the GTK main loop.
pub fn block_signals() -> libc::sigset_t {
    // SAFETY: plain libc calls on a locally owned signal set, before any thread is started.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

fn wait_for_signals() -> async_channel::Receiver<i32> {
    let set = SIGNALS.get().copied();
    let (tx, rx) = async_channel::bounded(1);
    if let Some(set) = set {
        std::thread::spawn(move || {
            let mut sig = 0;
            // SAFETY: waits for one of the blocked signals in `set`.
            if unsafe { libc::sigwait(&set, &mut sig) } == 0 {
                let _ = tx.send_blocking(sig);
            }
        });
    }
    rx
}

static SIGNALS: std::sync::OnceLock<libc::sigset_t> = std::sync::OnceLock::new();

fn main() {
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let args = parse_args()?;
    // Absolute, because plugins run in $HOME and resolve relative paths against these.
    let user_dir = std::path::absolute(args.config_dir.clone().unwrap_or_else(config::default_user_dir))?;
    let global_dir = std::path::absolute(config::default_global_dir())?;
    let cmd: Vec<&str> = args.command.iter().map(String::as_str).collect();
    match cmd.as_slice() {
        [] | ["run"] => {
            let config = config::load(&global_dir, &user_dir)?;
            logging::init(args.log_level.as_deref().unwrap_or(&config.log_level));
            run(config)
        }
        ["check-config"] => check_config(&global_dir, &user_dir),
        ["plugins", "list"] => plugins_list(&global_dir, &user_dir),
        ["plugins", "init", plugin] => plugins_init(&global_dir, &user_dir, plugin, plugin),
        ["plugins", "init", plugin, instance] => plugins_init(&global_dir, &user_dir, plugin, instance),
        _ => bail!("unknown command {:?}\n\n{USAGE}", args.command.join(" ")),
    }
}

/// Only one bar per Wayland display (so starting via systemd *and* exec-once is harmless).
fn take_lock() -> Result<std::fs::File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    let path = dir.join(format!("days-of-our-lives-{display}.lock"));
    let file = std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
    use std::os::fd::AsRawFd;
    // SAFETY: flock on a file descriptor we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("days-of-our-lives is already running on {display} ({} is locked)", path.display());
    }
    Ok(file)
}

/// Installed plugins that the config uses, minus those speaking a newer protocol.
fn usable_plugins(config: &config::Config) -> BTreeMap<String, PathBuf> {
    let mut found = discovery::discover(&discovery::search_dirs(&config.plugin_path));
    let used: std::collections::BTreeSet<_> = config.instances.iter().map(|i| i.plugin.clone()).collect();
    found.retain(|name, path| {
        if !used.contains(name) {
            return false;
        }
        match discovery::describe(path) {
            Ok(d) if d.protocol > PROTOCOL_VERSION => {
                log::error!("plugin {name} ({}) needs protocol {}, this bar speaks {PROTOCOL_VERSION}", path.display(), d.protocol);
                false
            }
            Ok(_) => true,
            Err(e) => {
                log::warn!("plugin {name}: --describe failed ({e:#}); starting it anyway");
                true
            }
        }
    });
    found
}

fn run(config: config::Config) -> Result<()> {
    let _lock = take_lock()?;
    SIGNALS.get_or_init(block_signals);
    let plugins = usable_plugins(&config);
    log::info!("days-of-our-lives {} starting: {} bar(s), {} plugin instance(s)", env!("CARGO_PKG_VERSION"), config.bars.len(), config.instances.len());

    let app = gtk4::Application::new(Some("dev.dool.DaysOfOurLives"), gtk4::gio::ApplicationFlags::NON_UNIQUE);
    let state = std::rc::Rc::new(std::cell::RefCell::new(None));
    let config = std::cell::RefCell::new(Some(config));
    let plugins = std::cell::RefCell::new(Some(plugins));
    let st = state.clone();
    app.connect_activate(move |app| {
        let (Some(config), Some(plugins)) = (config.take(), plugins.take()) else { return };
        if !gtk4_layer_shell::is_supported() {
            log::error!("the compositor does not support wlr-layer-shell; is this a Wayland session under Hyprland?");
            app.quit();
            return;
        }
        let files = config.style_files();
        let bars = bars::Bars::start(app, config, plugins);
        let weak = std::rc::Rc::downgrade(&bars);
        let styles = style::install(&files, move || {
            if let Some(b) = weak.upgrade() {
                b.restyle();
            }
        });
        // Keep running even while no display is connected.
        let hold = app.hold();
        *st.borrow_mut() = Some((bars, styles, hold));
    });
    let signals = wait_for_signals();
    let quit_app = app.clone();
    glib::spawn_future_local(async move {
        if let Ok(signal) = signals.recv().await {
            log::info!("signal {signal}: shutting down");
            quit_app.quit();
        }
    });
    app.run_with_args::<&str>(&[]);
    // Dropping the bars closes the windows and stops all plugins.
    state.borrow_mut().take();
    log::info!("bye");
    Ok(())
}

fn check_config(global_dir: &std::path::Path, user_dir: &std::path::Path) -> Result<()> {
    logging::init("warn");
    let config = config::load(global_dir, user_dir)?;
    let plugins = discovery::discover(&discovery::search_dirs(&config.plugin_path));
    let mut problems = 0;
    println!("config: {} (global), {} (user)", global_dir.display(), user_dir.display());
    for b in &config.bars {
        println!("bar {:<10} output={:<8} edge={:<6} margin={:?} gap={}", b.name, if b.output.is_empty() { "default" } else { &b.output }, b.edge.name(), b.margin, b.gap);
    }
    let bar_names: Vec<&str> = config.bars.iter().map(|b| b.name.as_str()).collect();
    for i in &config.instances {
        let status = match plugins.get(&i.plugin) {
            Some(path) => match discovery::describe(path) {
                Ok(d) if d.protocol > PROTOCOL_VERSION => {
                    problems += 1;
                    format!("NEEDS PROTOCOL {}", d.protocol)
                }
                Ok(d) => format!("ok ({} {})", path.display(), d.version),
                Err(e) => format!("ok? (--describe failed: {e:#})"),
            },
            None => {
                problems += 1;
                format!("NOT INSTALLED (no dool-plugin-{})", i.plugin)
            }
        };
        println!("instance {:<12} plugin={:<11} bars={:?} {status}", i.name, i.plugin, i.bars);
        for b in &i.bars {
            if !bar_names.contains(&b.as_str()) {
                problems += 1;
                println!("  warning: bar {b:?} is not defined (or disabled)");
            }
        }
        if bar_names.contains(&i.name.as_str()) {
            problems += 1;
            println!("  warning: instance and bar share the name {:?}; their CSS #ids collide", i.name);
        }
    }
    for f in config.style_files() {
        if f.exists() {
            println!("style: {}", f.display());
        }
    }
    if problems > 0 {
        bail!("{problems} problem(s) found");
    }
    println!("configuration ok");
    Ok(())
}

fn plugins_list(global_dir: &std::path::Path, user_dir: &std::path::Path) -> Result<()> {
    let config = config::load(global_dir, user_dir).ok();
    let plugin_path = config.as_ref().map(|c| c.plugin_path.clone()).unwrap_or_default();
    let plugins = discovery::discover(&discovery::search_dirs(&plugin_path));
    if plugins.is_empty() {
        println!("no plugins found (looking for dool-plugin-* on the plugin path and $PATH)");
    }
    for (name, path) in plugins {
        let (version, description) = match discovery::describe(&path) {
            Ok(d) => (d.version, d.description),
            Err(e) => (String::from("?"), format!("(--describe failed: {e:#})")),
        };
        let users: Vec<String> = config.iter().flat_map(|c| &c.instances).filter(|i| i.plugin == name).map(|i| i.name.clone()).collect();
        println!("{name:<12} {version:<8} {description}\n             {}\n             used by: {}", path.display(), if users.is_empty() { "-".into() } else { users.join(", ") });
    }
    Ok(())
}

fn plugins_init(global_dir: &std::path::Path, user_dir: &std::path::Path, plugin: &str, instance: &str) -> Result<()> {
    let plugin_path = config::load(global_dir, user_dir).map(|c| c.plugin_path).unwrap_or_default();
    let plugins = discovery::discover(&discovery::search_dirs(&plugin_path));
    let path = plugins.get(plugin).with_context(|| format!("plugin {plugin:?} is not installed (no dool-plugin-{plugin} found)"))?;
    let text = discovery::run_flag(path, "--default-config")?;
    let target = user_dir.join("plugins.d").join(format!("{instance}.toml"));
    if target.exists() {
        bail!("{} already exists", target.display());
    }
    std::fs::create_dir_all(target.parent().unwrap())?;
    std::fs::write(&target, text)?;
    println!("wrote {}", target.display());
    Ok(())
}
