//! `dock-switch shortcuts [remove]`: adds or removes our bindings in COSMIC's
//! custom shortcuts file, keeping every other binding as written.
//!
//! The file is a RON map from binding to action. Rather than pull in a RON
//! parser, entries are split at top-level commas and only the two fields we
//! compare (modifiers and key) are read; everything else is kept verbatim.

use std::error::Error;
use std::fs;
use std::path::PathBuf;

struct Shortcut {
    modifiers: &'static [&'static str],
    key: &'static str,
    description: Option<String>,
    action: String,
}

const DIGITS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];

/// Every binding we own. `Super+0` means the last slot, and the workspace
/// shortcuts we displace move to `Ctrl` variants of the same keys.
fn wanted(exe: &str) -> Vec<Shortcut> {
    let exe = exe.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = Vec::new();
    for key in DIGITS {
        let (slot, app) = match key {
            "0" => ("last", "last dock app".to_string()),
            n => (n, format!("dock app {n}")),
        };
        let (workspace, move_to) = match key {
            "0" => (
                "LastWorkspace".to_string(),
                "MoveToLastWorkspace".to_string(),
            ),
            n => (format!("Workspace({n})"), format!("MoveToWorkspace({n})")),
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
        out.push_str(&format!("            {m},\n"));
    }
    out.push_str(&format!("        ],\n        key: \"{}\",\n", s.key));
    if let Some(d) = &s.description {
        out.push_str(&format!("        description: Some(\"{d}\"),\n"));
    }
    out.push_str(&format!("    ): {},\n", s.action));
    out
}

/// Splits `{ a, b, }` into its top-level entries, ignoring commas and braces
/// inside nested groups and strings.
fn split_entries(text: &str) -> Result<Vec<&str>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let open = text
        .find('{')
        .ok_or("shortcuts file does not start with '{'")?;
    let body = &text[open + 1..];
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    let (mut start, mut entries) = (0, Vec::new());
    for (i, c) in body.char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' => depth -= 1,
            '}' if depth == 0 => {
                entries.push(body[start..i].trim());
                entries.retain(|e| !e.is_empty());
                return Ok(entries);
            }
            '}' => depth -= 1,
            ',' if depth == 0 => {
                entries.push(body[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    Err("shortcuts file has no closing '}'".into())
}

/// Splits an entry into its binding `( ... )` and the action after the colon.
fn split_entry(entry: &str) -> Option<(&str, &str)> {
    let mut depth = 0;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in entry.char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
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
    let list = &list[list.find('[')? + 1..list.find(']')?];
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
    for entry in split_entries(text)? {
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

    let mut out = String::from("{\n");
    for entry in kept {
        out.push_str(&format!("    {entry},\n"));
    }
    if !remove {
        wanted
            .iter()
            .for_each(|s| out.push_str(&format_shortcut(s)));
    }
    out.push_str("}\n");
    Ok((out, replaced))
}

fn custom_path() -> PathBuf {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    config.join("cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
}

pub fn run(remove: bool) -> Result<(), Box<dyn Error>> {
    let path = custom_path();
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().ok_or("binary path is not UTF-8")?;
    if exe.contains(' ') {
        // COSMIC splits Spawn commands on whitespace.
        return Err(format!("binary path contains a space: {exe}").into());
    }
    let wanted = wanted(exe);

    let old = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display()).into()),
    };
    let (new, replaced) =
        apply(&old, &wanted, remove).map_err(|e| format!("{}: {e}", path.display()))?;
    for r in &replaced {
        println!("replacing {r}");
    }
    if new == old {
        println!("shortcuts already up to date");
        return Ok(());
    }

    let dir = path.parent().ok_or("shortcuts path has no parent")?;
    fs::create_dir_all(dir)?;
    if !old.is_empty() {
        fs::write(dir.join("custom.bak"), &old)?;
    }
    // Rename into place: COSMIC watches the file and would otherwise read it
    // half written.
    let tmp = dir.join(".custom.dock-switch.tmp");
    fs::write(&tmp, &new)?;
    fs::rename(&tmp, &path)?;
    let verb = if remove {
        "removed dock-switch shortcuts from"
    } else {
        "updated"
    };
    println!("{verb} {}", path.display());
    if !old.is_empty() {
        println!(
            "previous version saved as {}",
            dir.join("custom.bak").display()
        );
    }
    Ok(())
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
        assert_eq!(split_entries(&out).unwrap().len(), 2 + wanted.len());

        // Running again changes nothing and reports nothing.
        let (again, replaced) = apply(&out, &wanted, false).unwrap();
        assert_eq!(again, out);
        assert!(replaced.is_empty());

        // Removing leaves only the bindings that were not ours.
        let (removed, _) = apply(&out, &wanted, true).unwrap();
        assert_eq!(split_entries(&removed).unwrap().len(), 2);
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
        assert!(split_entries("{ (key: \"}\"): A, ").is_err());
    }
}
