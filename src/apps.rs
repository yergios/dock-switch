//! Desktop entries and dock pins, matched the way the COSMIC dock does it, so
//! our slot numbers agree with what the dock shows.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct DesktopEntry {
    /// Desktop file id: the path below `applications/` with `/` turned into
    /// `-`, minus `.desktop`, e.g. `wine-Programs-Steam`.
    pub id: String,
    /// File name minus `.desktop`, which the dock also matches on.
    stem: String,
    pub path: PathBuf,
    name: String,
    exec: String,
    wm_class: String,
}

/// Real desktop files are a few KiB even with many translations; anything far
/// bigger is not one and is not worth reading.
const MAX_ENTRY_SIZE: u64 = 1 << 20;

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn env_dir(var: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => fallback(),
    }
}

/// Every directory a scan looked at, with its modification time (None if it
/// did not exist). Installing or removing an app adds or removes a file and
/// so changes its directory's time, which lets an unchanged tree skip a
/// rescan. An entry edited in place is not noticed until something else
/// changes the directory.
pub struct ScanStamp(Vec<(PathBuf, Option<SystemTime>)>);

impl ScanStamp {
    pub fn changed(&self) -> bool {
        self.0.iter().any(|(dir, time)| mtime(dir) != *time)
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Every desktop entry, data home first, so a user's copy of an entry is
/// found before the system one.
pub fn load_entries() -> (Vec<DesktopEntry>, ScanStamp) {
    let mut dirs = vec![env_dir("XDG_DATA_HOME", || home().join(".local/share"))];
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    // The XDG base directory spec says relative entries are to be ignored.
    dirs.extend(
        data_dirs
            .split(':')
            .map(PathBuf::from)
            .filter(|d| d.is_absolute()),
    );

    let mut entries = Vec::new();
    let mut seen = Vec::new();
    for dir in dirs {
        collect(&dir.join("applications"), &mut entries, &mut seen);
    }
    (entries, ScanStamp(seen))
}

/// Walks the way the dock's library does, since the first match wins and
/// order decides ties: a directory's files in sorted order, then its
/// subdirectories in reverse sorted order. Directories are followed through
/// symlinks but each is visited once, which also stops symlink loops.
/// `seen` holds the directories visited so far (a handful, so a list).
fn collect(dir: &Path, out: &mut Vec<DesktopEntry>, seen: &mut Vec<(PathBuf, Option<SystemTime>)>) {
    let Ok(dir) = dir.canonicalize() else {
        // Remember missing directories too, so creating one is noticed.
        seen.push((dir.to_path_buf(), None));
        return;
    };
    if seen.iter().any(|(d, _)| *d == dir) {
        return;
    }
    // Stamped before reading, so a change during the scan triggers the next.
    seen.push((dir.clone(), mtime(&dir)));
    let Ok(read) = fs::read_dir(&dir) else { return };
    let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    let mut subdirs = Vec::new();
    for path in paths {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            subdirs.push(path);
        } else if meta.is_file()
            && meta.len() <= MAX_ENTRY_SIZE
            && path.extension().is_some_and(|e| e == "desktop")
            && let Some(entry) = parse_entry(&path)
        {
            out.push(entry);
        }
    }
    for sub in subdirs.iter().rev() {
        collect(sub, out, seen);
    }
}

/// Desktop file id per the desktop entry spec, as the dock computes it.
fn entry_id(path: &Path) -> Option<String> {
    let path = path.to_str()?.strip_suffix(".desktop")?;
    Some(match path.rsplit_once("/applications/") {
        Some((_, rel)) => rel.replace('/', "-"),
        None => path.rsplit('/').next()?.to_string(),
    })
}

fn parse_entry(path: &Path) -> Option<DesktopEntry> {
    let text = fs::read_to_string(path).ok()?;
    let mut entry = DesktopEntry {
        id: entry_id(path)?,
        stem: path.file_stem()?.to_str()?.to_string(),
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
        // Most lines are translations; only these three are kept.
        let field = match key.trim() {
            "Name" => &mut entry.name,
            "Exec" => &mut entry.exec,
            "StartupWMClass" => &mut entry.wm_class,
            _ => continue,
        };
        *field = value.trim().to_string();
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
        &|e| eq(&e.id, app_id) || eq(&e.stem, app_id) || eq(&e.stem, last_part),
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

const FAVORITES_DIR: &str = "cosmic/com.system76.CosmicAppList/v1";

/// Where cosmic-config keeps the user's dock settings, favorites included.
pub fn favorites_dir() -> PathBuf {
    env_dir("XDG_CONFIG_HOME", || home().join(".config")).join(FAVORITES_DIR)
}

/// The dock's pinned app list, as stored by cosmic-config, falling back to
/// the system default like cosmic-config does.
pub fn load_favorites() -> Vec<String> {
    let text = fs::read_to_string(favorites_dir().join("favorites"))
        .or_else(|_| {
            fs::read_to_string(
                Path::new("/usr/share")
                    .join(FAVORITES_DIR)
                    .join("favorites"),
            )
        })
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
            stem: id.into(),
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
    fn scans_like_the_dock() {
        let root = std::env::temp_dir().join(format!("dock-switch-scan-{}", std::process::id()));
        let apps = root.join("applications");
        for dir in ["a", "wine/Programs", "zz"] {
            fs::create_dir_all(apps.join(dir)).unwrap();
        }
        let desktop = "[Desktop Entry]\nName=X\n";
        for file in ["b", "a/z", "wine/Programs/Steam", "zz/y"] {
            fs::write(apps.join(format!("{file}.desktop")), desktop).unwrap();
        }
        let big = fs::File::create(apps.join("big.desktop")).unwrap();
        big.set_len(MAX_ENTRY_SIZE + 1).unwrap();
        std::os::unix::fs::symlink(&apps, apps.join("loop")).unwrap();

        let mut out = Vec::new();
        let mut seen = Vec::new();
        collect(&apps, &mut out, &mut seen);
        let ids: Vec<&str> = out.iter().map(|e| e.id.as_str()).collect();

        // Adding a file in any scanned directory, nested ones included, marks
        // the scan stale; nothing changing keeps it fresh.
        let stamp = ScanStamp(seen);
        assert!(!stamp.changed());
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(apps.join("wine/Programs/new.desktop"), desktop).unwrap();
        assert!(stamp.changed());
        fs::remove_dir_all(&root).unwrap();

        // Files first, then subdirectories in reverse order; the oversized
        // file and the symlink back to the root are skipped.
        assert_eq!(ids, ["b", "zz-y", "wine-Programs-Steam", "a-z"]);
        let entries = out;
        assert_eq!(
            find(&entries, "steam").map(|i| entries[i].id.as_str()),
            Some("wine-Programs-Steam")
        );
        assert_eq!(
            find(&entries, "wine-programs-steam").map(|i| entries[i].id.as_str()),
            Some("wine-Programs-Steam")
        );
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
