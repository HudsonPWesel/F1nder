//! Engagement profiles.
//!
//! Everything the fill modal learns — the sticky variable store, the usage log
//! that powers Recents and `SearchMode::RECENT`, and the exported `env.sh` — is
//! machine-wide by default. That is right for a single engagement and wrong the
//! moment you work two: one client's DC IP, domain, and credentials start
//! completing into another's commands.
//!
//! A profile namespaces those three files. The `default` profile deliberately
//! maps to the original paths, so existing data keeps working untouched and
//! there is nothing to migrate.

use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT: &str = "default";

/// Reject anything that would escape the profiles directory or confuse the
/// filesystem. Names are used verbatim as a single path component.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 40
        && name != DEFAULT
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}

fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|p| p.join("f1nder"))
}

/// Where the active profile name is remembered. Not view state — it has to
/// survive a reboot, so it does not live in `/tmp` with the `prev_*` files.
fn active_path() -> Option<PathBuf> {
    state_dir().map(|p| p.join("profile"))
}

/// The active profile, falling back to `default` if the marker is missing or
/// names a profile that no longer has a directory.
pub fn active(jsons_dir: &Path) -> String {
    let Some(name) = active_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return DEFAULT.to_string();
    };
    if name == DEFAULT || dir(jsons_dir, &name).is_dir() {
        name
    } else {
        DEFAULT.to_string()
    }
}

pub fn set_active(name: &str) {
    if let Some(path) = active_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, name);
    }
}

/// A named profile's directory under `JSONs/profiles/`. Never called for
/// `default`, which uses the original top-level paths instead.
pub fn dir(jsons_dir: &Path, name: &str) -> PathBuf {
    jsons_dir.join("profiles").join(name)
}

/// Every profile that exists, `default` first and the rest sorted.
pub fn list(jsons_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(jsons_dir.join("profiles"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| valid_name(n))
        .collect();
    names.sort();
    let mut out = vec![DEFAULT.to_string()];
    out.extend(names);
    out
}

pub fn create(jsons_dir: &Path, name: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir(jsons_dir, name))
}

/// Remove a profile's sticky store. The usage log and env export live outside
/// the repo and are cleared separately by the caller.
pub fn remove(jsons_dir: &Path, name: &str) -> std::io::Result<()> {
    if !valid_name(name) {
        return Ok(());
    }
    fs::remove_dir_all(dir(jsons_dir, name))
}

/// The sticky variable store for a profile.
pub fn vars_path(jsons_dir: &Path, name: &str) -> PathBuf {
    if name == DEFAULT {
        jsons_dir.join("vars.json")
    } else {
        dir(jsons_dir, name).join("vars.json")
    }
}

/// Append `<profiles/name>` to a base path's parent when a named profile is
/// active, so `~/.local/share/f1nder/history.jsonl` becomes
/// `~/.local/share/f1nder/profiles/acme/history.jsonl`.
pub fn scope(base: PathBuf, name: &str) -> PathBuf {
    if name == DEFAULT {
        return base;
    }
    let file = base.file_name().map(PathBuf::from).unwrap_or_default();
    match base.parent() {
        Some(parent) => parent.join("profiles").join(name).join(file),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_reserved_names() {
        assert!(!valid_name(""));
        assert!(!valid_name("default"));
        assert!(!valid_name(".."));
        assert!(!valid_name("../etc"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(".hidden"));
        assert!(valid_name("acme-internal"));
        assert!(valid_name("htb_season8"));
    }

    #[test]
    fn default_profile_keeps_original_paths() {
        let jsons = Path::new("/repo/JSONs");
        assert_eq!(vars_path(jsons, DEFAULT), jsons.join("vars.json"));
        let base = PathBuf::from("/home/u/.local/share/f1nder/history.jsonl");
        assert_eq!(scope(base.clone(), DEFAULT), base);
    }

    #[test]
    fn named_profile_scopes_paths() {
        let jsons = Path::new("/repo/JSONs");
        assert_eq!(
            vars_path(jsons, "acme"),
            jsons.join("profiles/acme/vars.json")
        );
        assert_eq!(
            scope(
                PathBuf::from("/home/u/.local/share/f1nder/history.jsonl"),
                "acme"
            ),
            PathBuf::from("/home/u/.local/share/f1nder/profiles/acme/history.jsonl")
        );
    }
}

/// State of the Ctrl+P switcher overlay. `naming` holds the in-progress name
/// when creating a profile; `confirm_delete` the row awaiting a y/n.
#[derive(Default)]
pub struct ProfileUi {
    pub names: Vec<String>,
    pub sel: usize,
    pub naming: Option<String>,
    pub confirm_delete: Option<String>,
    pub error: Option<String>,
}
