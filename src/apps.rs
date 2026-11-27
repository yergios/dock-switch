//! Desktop entries and dock pins, matched the way the COSMIC dock does it, so
//! our slot numbers agree with what the dock shows.

use std::fs;
use std::path::{Path, PathBuf};

pub struct DesktopEntry {
    /// File stem, e.g. `com.mitchellh.ghostty`.
    pub id: String,
    pub path: PathBuf,
    name: String,
    exec: String,
    wm_class: String,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn env_dir(var: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => fallback(),
    }
}

/// Every desktop entry, data home first, so a user's copy of an entry is
/// found before the system one.
pub fn load_entries() -> Vec<DesktopEntry> {
    let mut dirs = vec![env_dir("XDG_DATA_HOME", || home().join(".local/share"))];
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );

    let mut entries = Vec::new();
    for dir in dirs {
        collect(&dir.join("applications"), &mut entries, 0);
    }
    entries
}

fn collect(dir: &Path, out: &mut Vec<DesktopEntry>, depth: u32) {
    let Ok(read) = fs::read_dir(dir) else { return };
    // Sorted so ties between entries resolve the same way on every run.
    let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if depth < 4 {
                collect(&path, out, depth + 1);
            }
        } else if path.extension().is_some_and(|e| e == "desktop")
            && let Some(entry) = parse_entry(&path)
        {
            out.push(entry);
        }
    }
}

fn parse_entry(path: &Path) -> Option<DesktopEntry> {
    let text = fs::read_to_string(path).ok()?;
    let mut entry = DesktopEntry {
        id: path.file_stem()?.to_str()?.to_string(),
        path: path.to_path_buf(),
        name: String::new(),
        exec: String::new(),
        wm_class: String::new(),
    };
    // Only the [Desktop Entry] group; action groups have their own Name/Exec.
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "Name" => entry.name = value,
            "Exec" => entry.exec = value,
            "StartupWMClass" => entry.wm_class = value,
            _ => {}
        }
    }
    Some(entry)
}

fn eq(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

/// Same precedence as freedesktop-desktop-entry's `find_app_by_id`, which the
/// dock uses: WM class, then file name, then Name, then Exec.
pub fn find(entries: &[DesktopEntry], app_id: &str) -> Option<usize> {
    if app_id.is_empty() {
        return None;
    }
    let last_part = app_id.rsplit('.').next().unwrap_or(app_id);
    let rules: [&dyn Fn(&DesktopEntry) -> bool; 5] = [
        &|e| eq(&e.wm_class, app_id),
        &|e| eq(&e.id, app_id) || eq(&e.id, last_part),
        &|e| eq(&e.name, app_id),
        &|e| eq(&e.exec, app_id),
        &|e| {
            e.exec
                .split_ascii_whitespace()
                .next()
                .is_some_and(|w| eq(w, app_id))
        },
    ];
    rules.iter().find_map(|rule| entries.iter().position(rule))
}

/// The dock's pinned app list, as stored by cosmic-config.
pub fn load_favorites() -> Vec<String> {
    let rel = "cosmic/com.system76.CosmicAppList/v1/favorites";
    let user = env_dir("XDG_CONFIG_HOME", || home().join(".config")).join(rel);
    let text = fs::read_to_string(user)
        .or_else(|_| fs::read_to_string(Path::new("/usr/share").join(rel)))
        .unwrap_or_default();
    parse_string_list(&text)
}

/// Extracts the strings of a RON list like `["a", "b",]`. App ids never
/// contain quotes, so escapes are not handled.
fn parse_string_list(text: &str) -> Vec<String> {
    text.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str, exec: &str, wm_class: &str) -> DesktopEntry {
        DesktopEntry {
            id: id.into(),
            path: PathBuf::new(),
            name: name.into(),
            exec: exec.into(),
            wm_class: wm_class.into(),
        }
    }

    #[test]
    fn matches_like_the_dock() {
        let entries = [
            entry(
                "com.anthropic.Claude",
                "Claude",
                "claude-desktop %U",
                "com.anthropic.Claude",
            ),
            entry(
                "app.zen_browser.zen",
                "Zen Browser",
                "/usr/bin/flatpak run app.zen_browser.zen",
                "zen",
            ),
            entry(
                "bitwarden",
                "Bitwarden",
                "/opt/Bitwarden/bitwarden %U",
                "Bitwarden",
            ),
            entry("org.gnome.Calculator", "Calculator", "gnome-calculator", ""),
            entry("foo", "Foo", "foo", ""),
        ];
        assert_eq!(find(&entries, "Claude"), Some(0)); // pin, by Name
        assert_eq!(find(&entries, "com.anthropic.Claude"), Some(0)); // window, by WM class
        assert_eq!(find(&entries, "zen"), Some(1)); // window, by WM class
        assert_eq!(find(&entries, "app.zen_browser.zen"), Some(1)); // pin, by id
        assert_eq!(find(&entries, "BITWARDEN"), Some(2)); // case-insensitive
        assert_eq!(find(&entries, "gnome-calculator"), Some(3)); // by Exec
        assert_eq!(find(&entries, "org.example.foo"), Some(4)); // by last id part
        assert_eq!(find(&entries, "nope"), None);
        assert_eq!(find(&entries, ""), None);
    }

    #[test]
    fn parses_favorites() {
        let text = "[\n    \"com.system76.CosmicFiles\",\n    \"Claude\",\n]";
        assert_eq!(
            parse_string_list(text),
            ["com.system76.CosmicFiles", "Claude"]
        );
        assert!(parse_string_list("").is_empty());
    }
}
