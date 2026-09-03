use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Use {
    pub ts: String,
    pub entry_id: String,
    pub title: String,
    pub source_stem: String,
    pub cmd: String,
    #[serde(default)]
    pub vars: HashMap<String, String>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn history_path(profile: &str) -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|p| p.join(".local/share")))
        .map(|p| p.join("f1nder/history.jsonl"))
        .map(|p| crate::profile::scope(p, profile))
}

pub fn env_path(profile: &str) -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|p| p.join(".cache")))
        .map(|p| p.join("f1nder/env.sh"))
        .map(|p| crate::profile::scope(p, profile))
}

/// Where a copied command is left for the shell hook that `--shell-init`
/// installs. The file is named after the shell that launched us, so a hook only
/// ever finds its own line and can delete it on sight — no timestamps, no
/// cross-talk between terminals.
pub fn prompt_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|p| p.join(".cache")))
        .map(|p| {
            p.join(format!(
                "f1nder/prompt-{}.cmd",
                std::os::unix::process::parent_id()
            ))
        })
}

/// Hand the command to the shell that launched us.
///
/// The `f1nder` wrapper function covers the common case by capturing `--print`,
/// but nothing captures a binary invoked by path (`./f1nder`, a tmux binding,
/// an alias), and a process cannot write into its parent's line editor itself.
/// So the command is dropped here and the `precmd` hook picks it up.
pub fn drop_for_prompt(cmd: &str) -> Result<()> {
    if disabled() {
        return Ok(());
    }
    let Some(path) = prompt_path() else {
        return Ok(());
    };
    sweep_stale_prompts(&path);
    secure_write(&path, cmd.as_bytes())
}

/// Delete drop files older than an hour. A shell that dies between the copy and
/// its next prompt leaves one behind, and these hold whole commands — including
/// whatever credentials were filled into them — so they should not pile up.
fn sweep_stale_prompts(path: &Path) {
    let Some(dir) = path.parent() else { return };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let stale = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("prompt-") && n.ends_with(".cmd"))
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age.as_secs() > 3600);
        if stale {
            let _ = fs::remove_file(&p);
        }
    }
}

pub fn disabled() -> bool {
    std::env::var("F1NDER_NO_LOG").as_deref() == Ok("1")
}

pub fn now_iso() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += (month <= 2) as i64;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3600,
        day_seconds / 60 % 60,
        day_seconds % 60
    )
}

pub fn age_label(ts: &str) -> String {
    let then = epoch_of(ts);
    if then == 0 {
        return "?".into();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let age = (now - then).max(0);
    if age < 60 {
        format!("{}s", age)
    } else if age < 3600 {
        format!("{}m", age / 60)
    } else if age < 86_400 {
        format!("{}h", age / 3600)
    } else {
        format!("{}d", age / 86_400)
    }
}

pub fn append(profile: &str, mut item: Use) -> Result<()> {
    if disabled() {
        return Ok(());
    }
    let Some(path) = history_path(profile) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    item.ts = now_iso();
    let line = serde_json::to_string(&item)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    #[cfg(unix)]
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    writeln!(file, "{line}")?;
    Ok(())
}

pub fn load(profile: &str) -> Vec<Use> {
    if disabled() {
        return Vec::new();
    }
    let Some(path) = history_path(profile) else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return Vec::new();
    };
    let lines: Vec<String> = BufReader::new(file).lines().map_while(Result::ok).collect();
    if lines.len() > 5_000 {
        let kept = lines[lines.len() - 5_000..].join("\n") + "\n";
        let _ = secure_write(&path, kept.as_bytes());
    }
    lines
        .iter()
        .rev()
        .take(500)
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect()
}

pub fn recall(recent: &[Use]) -> HashMap<String, HashMap<String, String>> {
    let mut out = HashMap::new();
    for item in recent {
        out.entry(item.entry_id.clone())
            .or_insert_with(|| item.vars.clone());
    }
    out
}

/// Parse an `env.sh` we wrote earlier back into NAME -> value pairs so a new
/// export can merge into it. Anything that does not match our own single-quoted
/// `export NAME='value'` shape is dropped rather than guessed at.
fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("export ") else {
            continue;
        };
        let Some((name, quoted)) = rest.split_once('=') else {
            continue;
        };
        let Some(body) = quoted
            .strip_prefix('\'')
            .and_then(|b| b.strip_suffix('\''))
        else {
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        out.push((name.to_string(), body.replace("'\\''", "'")));
    }
    out
}

/// Export the fill's values as shell assignments, **merging** into whatever the
/// file already holds. An engagement fills `DC_IP` once and `USER` ten commands
/// later; rewriting the file from scratch each time would leave only the newest
/// command's handful of variables, which is useless for hand-written one-liners.
pub fn export_env(profile: &str, vars: &HashMap<String, String>) -> Result<()> {
    if disabled() {
        return Ok(());
    }
    let Some(path) = env_path(profile) else {
        return Ok(());
    };
    let mut merged: Vec<(String, String)> = fs::read_to_string(&path)
        .map(|t| parse_env(&t))
        .unwrap_or_default();
    for (canon, value) in vars {
        if !exportable(canon, value) {
            continue;
        }
        let name = canon.to_uppercase();
        match merged.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value.clone(),
            None => merged.push((name, value.clone())),
        }
    }
    merged.sort_by(|a, b| a.0.cmp(&b.0));
    let mut body = String::new();
    for (name, value) in &merged {
        let escaped = value.replace('\'', "'\\''");
        body.push_str(&format!("export {name}='{escaped}'\n"));
    }
    secure_write(&path, body.as_bytes())
}

/// Single-character canons and the `arg`/`file`/`wordlist` catch-alls are
/// detector artefacts, not real variables — keep them out of the user's shell.
fn exportable(canon: &str, value: &str) -> bool {
    // Grouping suffixes duplicate labels as `arg_2`, `file_3`; judge the base.
    let base = canon
        .rsplit_once('_')
        .filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .map_or(canon, |(b, _)| b);
    base.len() > 1
        && !value.trim().is_empty()
        && !matches!(base, "arg" | "file" | "wordlist")
}

fn secure_write(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

/// Age-weighted usage score per entry id: how often, discounted by how long
/// ago. Mirrors the classic frecency buckets — something run twice this hour
/// outranks something run twenty times last month.
pub fn frecency(recent: &[Use]) -> HashMap<String, i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut out: HashMap<String, i64> = HashMap::new();
    for item in recent {
        let age = (now - epoch_of(&item.ts)).max(0);
        let weight = match age {
            a if a < 3_600 => 100,
            a if a < 86_400 => 50,
            a if a < 604_800 => 25,
            a if a < 2_592_000 => 10,
            _ => 3,
        };
        *out.entry(item.entry_id.clone()).or_default() += weight;
    }
    out
}

/// Parse one of our own `now_iso` stamps back to a unix timestamp. Returns 0
/// for anything unparseable, which just makes that use look ancient.
fn epoch_of(ts: &str) -> i64 {
    let nums: Vec<i64> = ts
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() < 6 {
        return 0;
    }
    let (y, m, d) = (nums[0], nums[1], nums[2]);
    let y_adj = y - (m <= 2) as i64;
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + nums[3] * 3600 + nums[4] * 60 + nums[5]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_round_trips_through_parse() {
        let text = "export DC_IP='10.0.10.5'\nexport PASS='it'\\''s'\n# comment\nnoise\n";
        let got = parse_env(text);
        assert_eq!(
            got,
            vec![
                ("DC_IP".to_string(), "10.0.10.5".to_string()),
                ("PASS".to_string(), "it's".to_string()),
            ]
        );
    }

    #[test]
    fn parse_env_ignores_anything_it_did_not_write() {
        assert!(parse_env("export PATH=$PATH:/x\n").is_empty());
        assert!(parse_env("export 'weird'='x'\n").is_empty());
        assert!(parse_env("DC_IP='10.0.0.1'\n").is_empty());
    }

    /// The detector artefacts must not reach the user's shell.
    #[test]
    fn exportable_drops_detector_artefacts() {
        assert!(!exportable("c", "All"));
        assert!(!exportable("arg", "x"));
        assert!(!exportable("arg_2", "x"));
        assert!(!exportable("wordlist", "/x.txt"));
        assert!(!exportable("dc_ip", "   "));
        assert!(exportable("dc_ip", "10.0.10.5"));
        assert!(exportable("domain_2", "corp.local"));
    }

    #[test]
    fn frecency_favours_the_recent_over_the_frequent() {
        let now = now_iso();
        let fresh = Use {
            ts: now.clone(),
            entry_id: "fresh".into(),
            title: String::new(),
            source_stem: String::new(),
            cmd: String::new(),
            vars: HashMap::new(),
        };
        let stale = Use {
            ts: "2020-01-01T00:00:00Z".into(),
            entry_id: "stale".into(),
            ..fresh.clone()
        };
        let mut items = vec![fresh, stale.clone()];
        // Ten ancient uses still lose to one from this hour.
        items.extend(std::iter::repeat_n(stale, 9));
        let scores = frecency(&items);
        assert!(
            scores["fresh"] > scores["stale"],
            "{:?}",
            scores
        );
    }

    #[test]
    fn age_label_rejects_garbage() {
        assert_eq!(age_label("not a timestamp"), "?");
        assert_eq!(age_label(&now_iso()), "0s");
    }
}
