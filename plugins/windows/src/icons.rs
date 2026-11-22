//! Window class → icon file, via `.desktop` files (matched by file name or
//! `StartupWMClass`) and the icon theme.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct DesktopEntry {
    /// File name without `.desktop`, lowercased (e.g. `org.gnome.nautilus`).
    pub id: String,
    pub wm_class: String,
    pub icon: String,
}

pub fn application_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::iter::once(data_home).chain(std::env::split_paths(&data_dirs)).map(|d| d.join("applications")).collect()
}

/// `Icon=` and `StartupWMClass=` of the `[Desktop Entry]` group.
pub fn parse_desktop(text: &str) -> (String, String) {
    let (mut icon, mut wm_class, mut in_entry) = (String::new(), String::new(), false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some(v) = line.strip_prefix("Icon=") {
                icon = v.trim().to_owned();
            } else if let Some(v) = line.strip_prefix("StartupWMClass=") {
                wm_class = v.trim().to_owned();
            }
        }
    }
    (icon, wm_class)
}

pub fn index(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().is_some_and(|e| e == "desktop") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let (icon, wm_class) = parse_desktop(&text);
                    let id = path.file_stem().unwrap().to_string_lossy().to_lowercase();
                    out.push(DesktopEntry { id, wm_class: wm_class.to_lowercase(), icon });
                }
            }
        }
    }
    out
}

/// The `Icon=` name for a window class: file name match first (exact, then the last
/// reverse-DNS component), then `StartupWMClass`. Earlier directories win.
pub fn icon_name<'a>(entries: &'a [DesktopEntry], class: &str) -> Option<&'a str> {
    let class = class.to_lowercase();
    let suffix = format!(".{class}");
    entries
        .iter()
        .find(|e| e.id == class)
        .or_else(|| entries.iter().find(|e| e.id.ends_with(&suffix)))
        .or_else(|| entries.iter().find(|e| !e.wm_class.is_empty() && e.wm_class == class))
        .map(|e| e.icon.as_str())
        .filter(|i| !i.is_empty())
}

pub fn resolve(entries: &[DesktopEntry], class: &str, theme: &str, size: u16) -> Option<PathBuf> {
    let lowered = class.to_lowercase();
    let name = icon_name(entries, class).unwrap_or(&lowered);
    if Path::new(name).is_absolute() {
        return Some(PathBuf::from(name)).filter(|p| p.exists());
    }
    freedesktop_icons::lookup(name).with_size(size).with_theme(theme).with_cache().find()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_by_file_name_reverse_dns_and_wm_class() {
        let dir = std::env::temp_dir().join(format!("dool-icons-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("firefox.desktop"), "[Desktop Entry]\nName=Firefox\nIcon=firefox\n[Desktop Action new]\nIcon=other\n").unwrap();
        std::fs::write(dir.join("org.gnome.Nautilus.desktop"), "[Desktop Entry]\nIcon=org.gnome.Nautilus\n").unwrap();
        std::fs::write(dir.join("code.desktop"), "[Desktop Entry]\nIcon=vscode\nStartupWMClass=Code\n").unwrap();
        let idx = index(&[dir]);
        assert_eq!(icon_name(&idx, "Firefox"), Some("firefox"));
        assert_eq!(icon_name(&idx, "nautilus"), Some("org.gnome.Nautilus"));
        assert_eq!(icon_name(&idx, "code"), Some("vscode"));
        assert_eq!(icon_name(&idx, "kitty"), None);
    }
}
