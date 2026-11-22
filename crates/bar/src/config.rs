//! Loading and merging the TOML configuration.
//!
//! Global files live in `/etc/days-of-our-lives/` (override with `$DOOL_ETC_DIR`), user
//! files in `~/.config/days-of-our-lives/`. User values are deep-merged over global ones:
//! tables merge key by key, everything else is replaced.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
    pub fn name(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::Left => "left",
            Edge::Right => "right",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Background,
    Bottom,
    Top,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BarConfig {
    pub name: String,
    /// Connector name, `"*"` for every display, `""` for the compositor's choice.
    pub output: String,
    pub edge: Edge,
    pub layer: Layer,
    pub exclusive: bool,
    /// top, right, bottom, left
    pub margin: [i32; 4],
    pub gap: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub name: String,
    pub plugin: String,
    pub bars: Vec<String>,
    pub section: Section,
    pub order: i64,
    pub expand: bool,
    /// Everything except `plugin`, `enabled` and `[position]`, for the plugin.
    pub config: serde_json::Value,
    /// Directory of the most specific file that defined this instance.
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub plugin_path: Vec<PathBuf>,
    pub log_level: String,
    pub bars: Vec<BarConfig>,
    pub instances: Vec<Instance>,
    pub global_dir: PathBuf,
    pub user_dir: PathBuf,
}

impl Config {
    pub fn style_files(&self) -> [PathBuf; 2] {
        [self.global_dir.join("style.css"), self.user_dir.join("style.css")]
    }
}

pub fn default_global_dir() -> PathBuf {
    std::env::var_os("DOOL_ETC_DIR").map(PathBuf::from).unwrap_or_else(|| "/etc/days-of-our-lives".into())
}

pub fn default_user_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config")).join("days-of-our-lives")
}

/// Deep merge: tables are merged key by key, any other value in `over` replaces `base`.
pub fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Table(b), Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

fn read_toml(path: &Path) -> Result<Option<Table>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.parse::<Table>().with_context(|| format!("{}: invalid TOML", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// `plugins.d/*.toml` of one directory, by instance name.
fn read_instances(dir: &Path) -> Result<BTreeMap<String, Table>> {
    let mut out = BTreeMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "toml") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            if let Some(t) = read_toml(&path)? {
                out.insert(name, t);
            }
        }
    }
    Ok(out)
}

pub fn load(global_dir: &Path, user_dir: &Path) -> Result<Config> {
    let mut main = Value::Table(read_toml(&global_dir.join("config.toml"))?.unwrap_or_default());
    if let Some(user) = read_toml(&user_dir.join("config.toml"))? {
        merge(&mut main, Value::Table(user));
    }
    let Value::Table(main) = main else { unreachable!() };

    let global = read_instances(&global_dir.join("plugins.d"))?;
    let user = read_instances(&user_dir.join("plugins.d"))?;
    let mut instances = Vec::new();
    let names: std::collections::BTreeSet<_> = global.keys().chain(user.keys()).cloned().collect();
    for name in names {
        let mut value = Value::Table(global.get(&name).cloned().unwrap_or_default());
        let dir = if let Some(u) = user.get(&name) {
            merge(&mut value, Value::Table(u.clone()));
            user_dir.join("plugins.d")
        } else {
            global_dir.join("plugins.d")
        };
        let Value::Table(table) = value else { unreachable!() };
        if let Some(inst) = parse_instance(&name, table, dir).with_context(|| format!("plugins.d/{name}.toml"))? {
            instances.push(inst);
        }
    }
    parse_main(main, instances, global_dir, user_dir)
}

fn parse_main(mut main: Table, instances: Vec<Instance>, global_dir: &Path, user_dir: &Path) -> Result<Config> {
    let plugin_path = match main.remove("plugin_path") {
        Some(v) => strings(&v, "plugin_path")?.into_iter().map(|s| expand_home(&s)).collect(),
        None => vec![],
    };
    let log_level = match main.remove("log_level") {
        Some(v) => str_of(&v, "log_level")?.to_owned(),
        None => "info".into(),
    };
    let defaults = match main.remove("bar_defaults") {
        Some(Value::Table(t)) => t,
        Some(_) => bail!("config.toml: bar_defaults must be a table"),
        None => Table::new(),
    };
    let mut bars = Vec::new();
    match main.remove("bars") {
        Some(Value::Table(t)) => {
            for (name, v) in t {
                let Value::Table(bar) = v else { bail!("config.toml: bars.{name} must be a table") };
                let mut merged = Value::Table(defaults.clone());
                merge(&mut merged, Value::Table(bar));
                let Value::Table(merged) = merged else { unreachable!() };
                if let Some(b) = parse_bar(&name, merged).with_context(|| format!("config.toml: bars.{name}"))? {
                    bars.push(b);
                }
            }
        }
        Some(_) => bail!("config.toml: bars must be a table of tables ([bars.<name>])"),
        None => bars.extend(parse_bar("main", defaults).context("config.toml: bar_defaults")?),
    }
    for key in main.keys() {
        log::warn!("config.toml: unknown key {key:?} ignored");
    }
    Ok(Config { plugin_path, log_level, bars, instances, global_dir: global_dir.into(), user_dir: user_dir.into() })
}

fn parse_bar(name: &str, mut t: Table) -> Result<Option<BarConfig>> {
    if !take_bool(&mut t, "enabled", true)? {
        return Ok(None);
    }
    let output = take_str(&mut t, "output", "")?;
    let edge = match take_str(&mut t, "edge", "top")?.as_str() {
        "top" => Edge::Top,
        "bottom" => Edge::Bottom,
        "left" => Edge::Left,
        "right" => Edge::Right,
        e => bail!("edge: expected top | bottom | left | right, got {e:?}"),
    };
    let layer = match take_str(&mut t, "layer", "top")?.as_str() {
        "background" => Layer::Background,
        "bottom" => Layer::Bottom,
        "top" => Layer::Top,
        "overlay" => Layer::Overlay,
        l => bail!("layer: expected background | bottom | top | overlay, got {l:?}"),
    };
    let exclusive = take_bool(&mut t, "exclusive", true)?;
    let margin = match t.remove("margin") {
        Some(v) => parse_margin(&v)?,
        None => [0; 4],
    };
    let gap = match t.remove("gap") {
        Some(v) => int_of(&v, "gap")? as i32,
        None => 0,
    };
    for key in t.keys() {
        log::warn!("bars.{name}: unknown key {key:?} ignored");
    }
    Ok(Some(BarConfig { name: name.into(), output, edge, layer, exclusive, margin, gap }))
}

/// CSS-like shorthand: `6`, `[v, h]` or `[top, right, bottom, left]`.
pub fn parse_margin(v: &Value) -> Result<[i32; 4]> {
    let n = |v: &Value| int_of(v, "margin").map(|n| n as i32);
    match v {
        Value::Integer(_) => {
            let m = n(v)?;
            Ok([m; 4])
        }
        Value::Array(a) if a.len() == 2 => {
            let (v, h) = (n(&a[0])?, n(&a[1])?);
            Ok([v, h, v, h])
        }
        Value::Array(a) if a.len() == 4 => Ok([n(&a[0])?, n(&a[1])?, n(&a[2])?, n(&a[3])?]),
        _ => bail!("margin: expected a number, [vertical, horizontal] or [top, right, bottom, left]"),
    }
}

fn parse_instance(name: &str, mut t: Table, config_dir: PathBuf) -> Result<Option<Instance>> {
    if !take_bool(&mut t, "enabled", true)? {
        return Ok(None);
    }
    let plugin = match t.remove("plugin") {
        Some(v) => str_of(&v, "plugin")?.to_owned(),
        None => bail!("missing `plugin = \"<name>\"`"),
    };
    let mut pos = match t.remove("position") {
        Some(Value::Table(p)) => p,
        Some(_) => bail!("position must be a table"),
        None => Table::new(),
    };
    let bars = match pos.remove("bar") {
        Some(Value::String(s)) => vec![s],
        Some(v @ Value::Array(_)) => strings(&v, "position.bar")?,
        Some(_) => bail!("position.bar: expected a bar name or a list of names"),
        None => vec!["main".into()],
    };
    let section = match take_str(&mut pos, "section", "start")?.as_str() {
        "start" => Section::Start,
        "center" => Section::Center,
        "end" => Section::End,
        s => bail!("position.section: expected start | center | end, got {s:?}"),
    };
    let order = match pos.remove("order") {
        Some(v) => int_of(&v, "position.order")?,
        None => 0,
    };
    let expand = take_bool(&mut pos, "expand", false)?;
    for key in pos.keys() {
        log::warn!("plugins.d/{name}.toml: unknown key position.{key} ignored");
    }
    let config = serde_json::to_value(&t).context("convert config to JSON")?;
    Ok(Some(Instance { name: name.into(), plugin, bars, section, order, expand, config, config_dir }))
}

fn expand_home(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(s),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.as_str().with_context(|| format!("{key}: expected a string"))
}
fn int_of(v: &Value, key: &str) -> Result<i64> {
    v.as_integer().with_context(|| format!("{key}: expected an integer"))
}
fn strings(v: &Value, key: &str) -> Result<Vec<String>> {
    let a = v.as_array().with_context(|| format!("{key}: expected a list of strings"))?;
    a.iter().map(|v| str_of(v, key).map(str::to_owned)).collect()
}
fn take_str(t: &mut Table, key: &str, default: &str) -> Result<String> {
    match t.remove(key) {
        Some(v) => Ok(str_of(&v, key)?.to_owned()),
        None => Ok(default.into()),
    }
}
fn take_bool(t: &mut Table, key: &str, default: bool) -> Result<bool> {
    match t.remove(key) {
        Some(v) => v.as_bool().with_context(|| format!("{key}: expected true or false")),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dool-config-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn user_values_deep_merge_over_global_ones() {
        let d = tmp("merge");
        let (g, u) = (d.join("etc"), d.join("user"));
        write(&g, "config.toml", "[bar_defaults]\ngap = 2\nmargin = 4\n[bars.main]\noutput = \"DP-1\"\nedge = \"top\"\n[bars.side]\nedge = \"left\"\n");
        write(&u, "config.toml", "[bars.main]\nedge = \"bottom\"\n[bars.side]\nenabled = false\n");
        write(&g, "plugins.d/clock.toml", "plugin = \"clock\"\nface = \"a.svg\"\n[position]\nsection = \"end\"\norder = 3\n");
        write(&u, "plugins.d/clock.toml", "face = \"b.svg\"\n[position]\nbar = [\"main\", \"x\"]\n");
        write(&g, "plugins.d/off.toml", "plugin = \"clock\"\n");
        write(&u, "plugins.d/off.toml", "enabled = false\n");
        let c = load(&g, &u).unwrap();
        assert_eq!(c.bars.len(), 1);
        let b = &c.bars[0];
        assert_eq!((b.name.as_str(), b.output.as_str(), b.edge, b.gap, b.margin), ("main", "DP-1", Edge::Bottom, 2, [4; 4]));
        assert_eq!(c.instances.len(), 1);
        let i = &c.instances[0];
        assert_eq!((i.section, i.order, i.bars.clone()), (Section::End, 3, vec!["main".to_string(), "x".into()]));
        assert_eq!(i.config, serde_json::json!({"face": "b.svg"}));
        assert_eq!(i.config_dir, u.join("plugins.d"));
    }

    #[test]
    fn no_bars_table_means_one_default_bar() {
        let d = tmp("default");
        let c = load(&d.join("etc"), &d.join("user")).unwrap();
        assert_eq!(c.bars.len(), 1);
        assert_eq!((c.bars[0].name.as_str(), c.bars[0].edge), ("main", Edge::Top));
    }

    #[test]
    fn margin_shorthands() {
        let p = |s: &str| parse_margin(&s.parse::<Table>().unwrap()["m"]);
        assert_eq!(p("m = 6").unwrap(), [6; 4]);
        assert_eq!(p("m = [1, 2]").unwrap(), [1, 2, 1, 2]);
        assert_eq!(p("m = [1, 2, 3, 4]").unwrap(), [1, 2, 3, 4]);
        assert!(p("m = [1, 2, 3]").is_err());
    }

    #[test]
    fn errors_name_the_file_and_key() {
        let d = tmp("err");
        write(&d.join("etc"), "plugins.d/cal.toml", "plugin = \"calendar\"\n[position]\nsection = \"middle\"\n");
        let e = format!("{:#}", load(&d.join("etc"), &d.join("user")).unwrap_err());
        assert!(e.contains("plugins.d/cal.toml") && e.contains("position.section"), "{e}");
    }
}
