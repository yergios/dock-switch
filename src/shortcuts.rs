//! `dock-switch shortcuts [remove]`: adds or removes our bindings in COSMIC's
//! custom shortcuts file, keeping every other binding as written.
//!
//! The file is a RON map from binding to action. Rather than pull in a RON
//! parser, entries are split at top-level commas and only the two fields we
//! compare (modifiers and key) are read; everything else is kept verbatim.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

struct Shortcut {
    modifiers: &'static [&'static str],
    key: &'static str,
    description: Option<String>,
    action: String,
}

const DIGITS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];

/// COSMIC runs `Spawn` commands with `/bin/sh -c`, so the binary path goes
/// into a shell command line. Rather than quote it, only characters that mean
/// nothing to the shell (or to RON strings) are accepted.
fn check_path(exe: &str) -> Result<(), String> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._+-".contains(c);
    match exe.chars().find(|&c| !safe(c)) {
        None => Ok(()),
        Some(c) => Err(format!(
            "binary path {exe:?} contains {c:?}, which would need shell quoting; \
             install it under a plainer path (make install PREFIX=...)"
        )),
    }
}

/// The binary as the shortcuts name it: `~/...` when it is under the home
/// directory, so the file holds no username and works for any user (handy in
/// dotfiles), else the absolute path. The shell expands a leading `~` from
/// HOME and never splits the result, so only the part after it has to pass
/// `check_path`.
fn command_path(exe: &str, home: &str) -> Result<String, String> {
    let home = home.trim_end_matches('/');
    match exe.strip_prefix(home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => {
            check_path(rest)?;
            Ok(format!("~{rest}"))
        }
        _ => {
            check_path(exe)?;
            Ok(exe.to_string())
        }
    }
}

/// Every binding we own. `Super+0` means the last slot, and the workspace
/// shortcuts we displace move to `Ctrl` variants of the same keys. `exe` comes
/// from `command_path`.
fn wanted(exe: &str) -> Vec<Shortcut> {
    let mut out = Vec::new();
    for key in DIGITS {
        let (slot, app, workspace, move_to) = match key {
            "0" => (
                "last",
                "last dock app".to_string(),
                "LastWorkspace".to_string(),
                "MoveToLastWorkspace".to_string(),
            ),
            n => (
                n,
                format!("dock app {n}"),
                format!("Workspace({n})"),
                format!("MoveToWorkspace({n})"),
            ),
        };
        out.push(Shortcut {
            modifiers: &["Super"],
            key,
            description: Some(format!("Switch to {app}")),
            action: format!("Spawn(\"{exe} switch {slot}\")"),
        });
        out.push(Shortcut {
            modifiers: &["Super", "Shift"],
            key,
            description: Some(format!("Open new window of {app}")),
            action: format!("Spawn(\"{exe} new {slot}\")"),
        });
        out.push(Shortcut {
            modifiers: &["Super", "Ctrl"],
            key,
            description: None,
            action: workspace,
        });
        out.push(Shortcut {
            modifiers: &["Super", "Shift", "Ctrl"],
            key,
            description: None,
            action: move_to,
        });
    }
    out.push(Shortcut {
        modifiers: &["Super"],
        key: "comma",
        description: None,
        action: "Minimize".to_string(),
    });
    out
}

fn format_shortcut(s: &Shortcut) -> String {
    let mut out = String::from("    (\n        modifiers: [\n");
    for m in s.modifiers {
        let _ = writeln!(out, "            {m},");
    }
    let _ = writeln!(out, "        ],\n        key: \"{}\",", s.key);
    if let Some(d) = &s.description {
        let _ = writeln!(out, "        description: Some(\"{d}\"),");
    }
    let _ = writeln!(out, "    ): {},", s.action);
    out
}

/// Splits `{ a, b, }` into whatever precedes the map (such as a RON
/// `#![enable(...)]` header, kept as is) and its top-level entries, ignoring
/// commas and braces inside nested groups and strings. Comments are refused
/// rather than guessed at; COSMIC does not write any.
fn split_entries(text: &str) -> Result<(&str, Vec<&str>), String> {
    if text.trim().is_empty() {
        return Ok(("", Vec::new()));
    }
    let open = text
        .find('{')
        .ok_or("shortcuts file does not start with '{'")?;
    let (header, body) = (&text[..open], &text[open + 1..]);
    let (mut depth, mut start, mut entries) = (0, 0, Vec::new());
    for (i, c) in outside_strings(body) {
        match c {
            '/' if body[i..].starts_with("//") || body[i..].starts_with("/*") => {
                return Err("comments in the shortcuts file are not supported".into());
            }
            '}' if depth == 0 => {
                entries.push(body[start..i].trim());
                entries.retain(|e| !e.is_empty());
                return Ok((header, entries));
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                entries.push(body[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    Err("shortcuts file has no closing '}'".into())
}

/// The characters of `text` outside RON strings (the quotes count as part
/// of the string), with their byte offsets.
fn outside_strings(text: &str) -> impl Iterator<Item = (usize, char)> {
    let (mut in_str, mut escaped) = (false, false);
    text.char_indices().filter(move |&(_, c)| {
        if !in_str {
            in_str = c == '"';
            return !in_str;
        }
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => in_str = false,
            _ => {}
        }
        false
    })
}

/// Splits an entry into its binding `( ... )` and the action after the colon.
fn split_entry(entry: &str) -> Option<(&str, &str)> {
    let mut depth = 0;
    for (i, c) in outside_strings(entry) {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    let action = entry[i + 1..].trim_start().strip_prefix(':')?;
                    return Some((&entry[..=i], action.trim()));
                }
            }
            _ => {}
        }
    }
    None
}

/// Sorted modifiers and the key of a binding, or None for keycode-only ones.
fn parse_binding(binding: &str) -> Option<(Vec<String>, String)> {
    let mods_at = binding.find("modifiers")?;
    let list = &binding[mods_at..];
    let list = &list[list.find('[')? + 1..];
    let list = &list[..list.find(']')?];
    let mut modifiers: Vec<String> = list
        .split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(String::from)
        .collect();
    modifiers.sort();

    // `key` followed by a colon, so `keycode:` does not match.
    let key_at = binding.match_indices("key").find_map(|(i, _)| {
        let rest = binding[i + 3..].trim_start();
        rest.starts_with(':').then(|| rest[1..].trim_start())
    })?;
    let value = key_at.strip_prefix('"')?;
    Some((modifiers, value[..value.find('"')?].to_string()))
}

fn normalize(action: &str) -> String {
    action.split_whitespace().collect()
}

/// An existing action counts as ours if it is exactly what we would write,
/// or any dock-switch command (an install under a different prefix).
fn is_ours(action: &str, s: &Shortcut) -> bool {
    normalize(action) == normalize(&s.action)
        || (s.action.starts_with("Spawn(") && action.contains("dock-switch"))
}

fn label(s: &Shortcut) -> String {
    format!("{}+{}", s.modifiers.join("+"), s.key)
}

/// Returns the new file and the bindings whose foreign actions were replaced.
fn apply(text: &str, wanted: &[Shortcut], remove: bool) -> Result<(String, Vec<String>), String> {
    let mut kept = Vec::new();
    let mut replaced = Vec::new();
    let (header, entries) = split_entries(text)?;
    for entry in entries {
        let (binding, action) = split_entry(entry).ok_or("cannot parse a shortcuts entry")?;
        let ours = parse_binding(binding).and_then(|(mods, key)| {
            wanted.iter().find(|s| {
                let mut want: Vec<&str> = s.modifiers.to_vec();
                want.sort();
                s.key == key && want == mods
            })
        });
        match ours {
            Some(s) if !remove => {
                if !is_ours(action, s) {
                    replaced.push(format!("{} (was {})", label(s), normalize(action)));
                }
            }
            Some(s) if is_ours(action, s) => {}
            _ => kept.push(entry),
        }
    }

    let mut out = format!("{header}{{\n");
    for entry in kept {
        out.push_str(&format!("    {entry},\n"));
    }
    if !remove {
        for s in wanted {
            out.push_str(&format_shortcut(s));
        }
    }
    out.push_str("}\n");
    Ok((out, replaced))
}

fn custom_path() -> PathBuf {
    crate::apps::config_home().join("cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
}

pub fn run(remove: bool) -> Result<(), Box<dyn Error>> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().ok_or("binary path is not UTF-8")?;
    let exe = command_path(exe, &std::env::var("HOME").unwrap_or_default())?;
    let wanted = wanted(&exe);

    // Through a symlink (a dotfiles manager, say) to the real file, so the
    // rename below replaces the file rather than the link.
    let path = custom_path();
    let path = fs::canonicalize(&path).unwrap_or(path);
    let old = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display()).into()),
    };
    if remove && old.is_none() {
        println!("no shortcuts file, nothing to remove");
        return Ok(());
    }
    let old_text = old.as_deref().unwrap_or("");
    let (new, replaced) =
        apply(old_text, &wanted, remove).map_err(|e| format!("{}: {e}", path.display()))?;
    for r in &replaced {
        println!("replacing {r}");
    }
    if new == old_text {
        println!("shortcuts already up to date");
        return Ok(());
    }

    let dir = path.parent().ok_or("shortcuts path has no parent")?;
    fs::create_dir_all(dir)?;
    // Only the first run's original is kept: a later backup would hold our
    // own bindings, and `remove` would overwrite the bindings it replaced.
    let backup = dir.join("custom.before-dock-switch");
    if let Some(old) = &old
        && write_new(&backup, old, 0o600).is_ok()
    {
        println!("original saved as {}", backup.display());
    }
    // Rename into place: COSMIC watches the file and would otherwise read it
    // half written. The file keeps its permissions.
    let mode = fs::metadata(&path).map_or(0o644, |m| m.permissions().mode() & 0o7777);
    let tmp = dir.join(".custom.dock-switch.tmp");
    let _ = fs::remove_file(&tmp); // left over from an interrupted run
    write_new(&tmp, &new, mode)?;
    fs::rename(&tmp, &path)?;
    let verb = if remove {
        "removed dock-switch shortcuts from"
    } else {
        "updated"
    };
    println!("{verb} {}", path.display());
    Ok(())
}

/// Creates a file that must not exist yet, so a planted symlink is never
/// followed, and gives it `mode` regardless of the umask.
fn write_new(path: &Path, text: &str, mode: u32) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXISTING: &str = r#"{
    (
        modifiers: [
            Super,
        ],
        key: "v",
        description: Some("Copyq toggle"),
    ): Spawn("copyq toggle"),
    (modifiers: [Shift, Super], key: "1"): Spawn("something-else"),
    (modifiers: [Super], keycode: 10): Disable,
}"#;

    #[test]
    fn install_keeps_others_and_replaces_conflicts() {
        let wanted = wanted("/bin/dock-switch");
        let (out, replaced) = apply(EXISTING, &wanted, false).unwrap();
        assert!(out.contains("Spawn(\"copyq toggle\")"));
        assert!(out.contains("keycode: 10"));
        assert!(!out.contains("something-else"));
        assert_eq!(replaced, ["Super+Shift+1 (was Spawn(\"something-else\"))"]);
        assert_eq!(split_entries(&out).unwrap().1.len(), 2 + wanted.len());

        // Running again changes nothing and reports nothing.
        let (again, replaced) = apply(&out, &wanted, false).unwrap();
        assert_eq!(again, out);
        assert!(replaced.is_empty());

        // Removing leaves only the bindings that were not ours.
        let (removed, _) = apply(&out, &wanted, true).unwrap();
        assert_eq!(split_entries(&removed).unwrap().1.len(), 2);
        assert!(removed.contains("copyq") && removed.contains("keycode: 10"));
    }

    #[test]
    fn remove_matches_any_install_prefix_but_spares_foreign_actions() {
        let old = wanted("/old/prefix/dock-switch");
        let (out, _) = apply("", &old, false).unwrap();
        let (removed, _) = apply(&out, &wanted("/bin/dock-switch"), true).unwrap();
        assert_eq!(removed, "{\n}\n");

        let foreign = "{ (modifiers: [Super], key: \"comma\"): ZoomOut, }";
        let (kept, _) = apply(foreign, &wanted("/bin/dock-switch"), true).unwrap();
        assert!(kept.contains("ZoomOut"));
    }

    #[test]
    fn parses_bindings() {
        assert_eq!(
            parse_binding("(modifiers: [Super, Ctrl], key: \"9\", description: None)"),
            Some((vec!["Ctrl".into(), "Super".into()], "9".into()))
        );
        assert_eq!(parse_binding("(modifiers: [Super], keycode: 10)"), None);
        // A `]` between the word "modifiers" and the list used to panic.
        assert_eq!(
            parse_binding("(description: Some(\"modifiers]\"), modifiers: [Super], key: \"v\")"),
            Some((vec!["Super".into()], "v".into()))
        );
        assert!(split_entries("{ (key: \"}\"): A, ").is_err());
    }

    #[test]
    fn keeps_header_and_refuses_comments() {
        let text = "#![enable(implicit_some)]\n{ (modifiers: [Super], key: \"v\"): Disable, }";
        let (out, _) = apply(text, &wanted("/bin/dock-switch"), true).unwrap();
        assert!(out.starts_with("#![enable(implicit_some)]\n{\n"));
        assert!(apply("{ // note\n}", &[], true).is_err());
        assert!(apply("{ /* } */ }", &[], true).is_err());
        // A `//` inside a string is not a comment.
        assert!(
            apply(
                "{ (modifiers: [Super], key: \"v\"): Spawn(\"a//b\"), }",
                &[],
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn accepts_only_shell_safe_paths() {
        assert!(check_path("/home/syl/.local/bin/dock-switch").is_ok());
        assert!(check_path("/home/o'brien/bin/dock-switch").is_err());
        assert!(check_path("/tmp/x$(id)/dock-switch").is_err());
        assert!(check_path("/a b/dock-switch").is_err());
    }

    #[test]
    fn names_the_binary_relative_to_home() {
        let exe = "/home/syl/.local/bin/dock-switch";
        assert_eq!(
            command_path(exe, "/home/syl").unwrap(),
            "~/.local/bin/dock-switch"
        );
        assert_eq!(
            command_path(exe, "/home/syl/").unwrap(),
            "~/.local/bin/dock-switch"
        );
        // Not under home, or only sharing a prefix with it: absolute.
        assert_eq!(
            command_path("/usr/local/bin/dock-switch", "/home/syl").unwrap(),
            "/usr/local/bin/dock-switch"
        );
        assert_eq!(
            command_path("/home/sylvia/bin/dock-switch", "/home/syl").unwrap(),
            "/home/sylvia/bin/dock-switch"
        );
        assert_eq!(command_path(exe, "").unwrap(), exe);
        assert_eq!(command_path(exe, "/").unwrap(), exe);
        // The home part may hold anything; the rest must still be plain.
        assert_eq!(
            command_path("/home/o'b/bin/dock-switch", "/home/o'b").unwrap(),
            "~/bin/dock-switch"
        );
        assert!(command_path("/home/syl/my bin/dock-switch", "/home/syl").is_err());
    }
}
