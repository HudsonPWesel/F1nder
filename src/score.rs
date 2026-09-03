//! Search matching and ranking for the Search and Browse tabs.
//!
//! Three ideas drive the ranking:
//!
//! 1. **Title dominates.** Field weights are spread far enough apart that no
//!    match in the tool/heading/command text can outrank a title match. A
//!    3-letter token like `get` used to score a full whole-word hit against the
//!    `tool` field of `Get-CimInstance` and beat a real title match; match
//!    quality is now scaled by how much of the matched word the token actually
//!    covers, so short tokens against long words earn very little.
//!
//! 2. **Adjacency matters.** Query words appearing contiguously and in order —
//!    in the title, or in the command itself (`bloodyAD ... get writable`) —
//!    earn a large bonus, so typing a phrase finds the entry that contains it.
//!
//! 3. **Typos are tolerated, but never preferred.** When a token matches nothing
//!    exactly, a bounded (one-edit) Damerau-Levenshtein pass rescues it
//!    (`blodyAD` → `bloodyAD`). The number of fuzzily-matched tokens is the sort key *above*
//!    score, so an exact hit always outranks a typo hit.
//!
//! The all-words gate is unchanged: every query word must match something, or
//! the entry is dropped. A fuzzy hit counts as a match.

use std::collections::HashMap;

use crate::{Entry, SearchMode};

/// Scores are fixed-point with two implied decimals, so the coverage-scaled
/// multipliers below stay in integer arithmetic.
const W_TITLE: i64 = 100_000;
const W_TOOL: i64 = 42_000;
const W_HEADING: i64 = 30_000;
const W_CMD: i64 = 17_000;

/// Adjacency bonuses: full when the query words are contiguous and in order,
/// `GAP_NUM/100` of that when they appear in order but with gaps between them.
const ADJ_TITLE: i64 = 90_000;
const ADJ_CMD: i64 = 60_000;
const ADJ_HEADING: i64 = 30_000;
const GAP_NUM: i64 = 30;

/// A word buried inside a longer word, or a token found only by spanning word
/// boundaries, is weak evidence.
const INSIDE_BASE: i64 = 2000;
const INSIDE_COV: i64 = 2000;
const PREFIX_BASE: i64 = 4500;
const PREFIX_COV: i64 = 3500;
const SPAN_PCT: i64 = 20;
const FUZZY_PCT: i64 = 42;

/// Shorter (more specific) titles win ties.
const LEN_PENALTY: i64 = 60;
const LEN_PENALTY_CAP: usize = 250;

/// One searchable field, pre-tokenized so the hot loop never re-lowercases.
#[derive(Debug, Default, Clone)]
pub struct FieldIndex {
    /// Tokens in document order, split on punctuation *and* camelCase
    /// (`bloodyAD` -> `bloody`, `ad`). Used for matching and for adjacency.
    pub seq: Vec<String>,
    /// Punctuation-split tokens that camelCase split further, kept whole so a
    /// query of `bloodyad` or `netexec` still lands an exact hit.
    pub extra: Vec<String>,
    /// Punctuation-split tokens joined by single spaces — the last-resort
    /// haystack for a token that spans a word boundary.
    pub joined: String,
}

#[derive(Debug, Default, Clone)]
pub struct EntryIndex {
    pub title: FieldIndex,
    pub heading: FieldIndex,
    pub cmd: FieldIndex,
    pub tool: FieldIndex,
    pub title_len: usize,
}

/// The command's tool binary (lowercased) — the first real token, skipping common
/// wrappers and env assignments; empty for URL-only "commands".
pub fn tool_of(cmd: &str) -> String {
    const WRAPPERS: &[&str] = &[
        "sudo",
        "doas",
        "proxychains",
        "proxychains4",
        "python",
        "python3",
        "pipx",
        "env",
        "time",
        "watch",
        "nohup",
    ];
    for tok in cmd.split_whitespace() {
        if tok.is_empty() {
            continue;
        }
        // env assignment like FOO=bar
        if tok.contains('=') && !tok.contains('/') {
            continue;
        }
        let low = tok.to_lowercase();
        if WRAPPERS.contains(&low.as_str()) {
            continue;
        }
        if low.starts_with("http://") || low.starts_with("https://") {
            return String::new();
        }
        // strip any path prefix: /usr/bin/ffuf -> ffuf
        return low.rsplit('/').next().unwrap_or(&low).to_string();
    }
    String::new()
}

/// Split `raw` at camelCase boundaries, pushing lowercased parts onto `out`.
/// Returns how many parts were pushed (>1 means the token was a compound).
fn push_camel(raw: &str, out: &mut Vec<String>) -> usize {
    let chars: Vec<char> = raw.chars().collect();
    if chars.is_empty() {
        return 0;
    }
    let mut start = 0usize;
    let mut parts = 0usize;
    for i in 1..chars.len() {
        let prev = chars[i - 1];
        if chars[i].is_uppercase() && (prev.is_lowercase() || prev.is_numeric()) {
            out.push(chars[start..i].iter().collect::<String>().to_lowercase());
            parts += 1;
            start = i;
        }
    }
    out.push(chars[start..].iter().collect::<String>().to_lowercase());
    parts + 1
}

/// Build the token index for one field's text.
pub fn index_field(s: &str) -> FieldIndex {
    /// Longest de-punctuated compound worth keeping; past this it is a file
    /// path or a wordlist, not a tool name anyone will type.
    const MAX_GLUED: usize = 40;

    let mut seq = Vec::new();
    let mut extra = Vec::new();
    let mut joined = String::new();
    for chunk in s.split_whitespace() {
        let mut glued = String::new();
        let mut runs = 0usize;
        for raw in chunk.split(|c: char| !c.is_alphanumeric()) {
            if raw.is_empty() {
                continue;
            }
            let low = raw.to_lowercase();
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(&low);
            if push_camel(raw, &mut seq) > 1 {
                extra.push(low.clone());
            }
            glued.push_str(&low);
            runs += 1;
        }
        // `evil-winrm` is one tool with one name; index it the way it gets typed.
        if runs > 1 && glued.len() <= MAX_GLUED {
            extra.push(glued);
        }
    }
    extra.sort();
    extra.dedup();
    FieldIndex {
        seq,
        extra,
        joined,
    }
}

pub fn build_index(entries: &[Entry]) -> Vec<EntryIndex> {
    entries.iter().map(index_entry).collect()
}

pub fn index_entry(e: &Entry) -> EntryIndex {
    EntryIndex {
        title: index_field(&e.title),
        heading: index_field(&e.heading_path.join(" > ")),
        cmd: index_field(&e.cmd),
        tool: index_field(&tool_of(&e.cmd)),
        title_len: e.title.chars().count(),
    }
}

/// Split a user query into lowercase alphanumeric tokens. Unlike the field
/// index this deliberately does *not* camelCase-split: the user types
/// `bloodyAD` as one word and expects it to match as one word.
pub fn query_tokens(q: &str) -> Vec<String> {
    q.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Edit budget by token length. Tokens under 4 chars get none, because a single
/// edit turns `get` into `net`, `set` and `gpt` and floods the results.
///
/// Everything else gets exactly one edit. Measured against this corpus, a second
/// edit buys no extra recall — `blodyad`, `netexc`, `impacekt` and `secretsdmup`
/// all recover fully at one — while costing a lot of noise, because it starts
/// conflating distinct real words: `kerberoast` is two edits from `kerberos` and
/// picked up 72 spurious entries.
fn fuzz_budget(tok: &str) -> usize {
    if tok.chars().count() < 4 { 0 } else { 1 }
}

/// Bounded Damerau-Levenshtein: is `a` within `max` edits of `b`, counting an
/// adjacent transposition (`secretsdmup`) as one edit?
fn edit_within(a: &str, b: &str, max: usize) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return false;
    }
    if a.is_empty() {
        return b.len() <= max;
    }
    if b.is_empty() {
        return a.len() <= max;
    }
    let n = b.len();
    let mut prev2: Vec<usize> = vec![0; n + 1];
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur: Vec<usize> = vec![0; n + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        let mut row_min = i;
        for j in 1..=n {
            let mut c = (prev[j - 1] + usize::from(a[i - 1] != b[j - 1]))
                .min(prev[j] + 1)
                .min(cur[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                c = c.min(prev2[j - 2] + 1);
            }
            cur[j] = c;
            row_min = row_min.min(c);
        }
        // Every remaining path runs through this row, so it can only get worse.
        if row_min > max {
            return false;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n] <= max
}

/// Best score for `tok` against one field, and whether that score came from a
/// fuzzy (typo-tolerant) match rather than a literal one.
fn field_score(f: &FieldIndex, tok: &str, weight: i64, allow_fuzzy: bool) -> (i64, bool) {
    let tl = tok.chars().count() as i64;
    let mut best = 0i64;
    let mut present = false;

    // Coverage-scaled: `get` covering 3 of `ciminstance`'s 11 chars is far
    // weaker evidence than `certip` covering 6 of `certipy`'s 7.
    let consider = |w: &str, best: &mut i64| -> bool {
        if w == tok {
            return true;
        }
        let wl = w.chars().count() as i64;
        if wl == 0 {
            return false;
        }
        if w.starts_with(tok) {
            *best = (*best).max(weight * (PREFIX_BASE * wl + PREFIX_COV * tl) / (10_000 * wl));
        } else if w.contains(tok) {
            *best = (*best).max(weight * (INSIDE_BASE * wl + INSIDE_COV * tl) / (10_000 * wl));
        }
        false
    };

    // `extra` holds the de-punctuated / camelCase compounds, which by
    // construction are *not* in `joined`; it is only ever a handful of tokens,
    // so scan it unconditionally.
    for w in &f.extra {
        if consider(w, &mut best) {
            return (weight, false);
        }
    }
    present |= best > 0;

    // A literal hit in `seq` makes `tok` a substring of `joined`. One scan here
    // rejects the whole field — including the long command bodies — without
    // touching its token list, which is the bulk of the hot loop.
    if f.joined.contains(tok) {
        present = true;
        for w in &f.seq {
            if consider(w, &mut best) {
                return (weight, false);
            }
        }
    }

    if present {
        // `best` stays 0 when the token is only found spanning a word boundary.
        return (best.max(weight * SPAN_PCT / 100), false);
    }

    let budget = if allow_fuzzy { fuzz_budget(tok) } else { 0 };
    if budget > 0 {
        for w in f.seq.iter().chain(f.extra.iter()) {
            if edit_within(tok, w, budget) {
                return (weight * FUZZY_PCT / 100, true);
            }
        }
    }
    (0, false)
}

/// Full bonus when the query words appear contiguously and in order in `seq`,
/// a reduced one when they appear in order with gaps.
fn phrase_bonus(seq: &[String], q: &[String], base: i64) -> i64 {
    if q.len() < 2 {
        return 0;
    }
    if seq.len() >= q.len() {
        for w in seq.windows(q.len()) {
            if w == q {
                return base;
            }
        }
    }
    let mut it = seq.iter();
    if q.iter().all(|t| it.any(|w| w == t)) {
        return base * GAP_NUM / 100;
    }
    0
}

/// The (field, weight, adjacency bonus, typo-tolerant?) set a search mode
/// considers. The prose description is deliberately excluded — a word buried
/// there shouldn't keep an otherwise-unrelated command in the results.
///
/// Typo tolerance is off for the command *body* in ALL mode. It carries the
/// smallest weight, so a fuzzy hit there is worth ~7k against a title hit's
/// 100k and would never change an ordering — but it is by far the largest token
/// list (a PowerShell one-liner runs to hundreds of tokens), so scanning it is
/// most of the cost of a query that matches nothing literally. In CMD mode,
/// where it is what the user is actually searching, it stays on.
fn fields_for<'a>(ix: &'a EntryIndex, mode: &SearchMode) -> Vec<(&'a FieldIndex, i64, i64, bool)> {
    match mode {
        SearchMode::TITLE => vec![(&ix.title, W_TITLE, ADJ_TITLE, true)],
        SearchMode::HEADING => vec![(&ix.heading, W_TITLE, ADJ_TITLE, true)],
        SearchMode::CMD => vec![
            (&ix.tool, 80_000, 0, true),
            (&ix.cmd, 40_000, ADJ_CMD, true),
        ],
        SearchMode::ALL | SearchMode::RECENT => vec![
            (&ix.title, W_TITLE, ADJ_TITLE, true),
            (&ix.tool, W_TOOL, 0, true),
            (&ix.heading, W_HEADING, ADJ_HEADING, true),
            (&ix.cmd, W_CMD, ADJ_CMD, false),
        ],
    }
}

/// Score one entry. `None` means it failed the all-words gate.
/// Returns `(fuzzy_token_count, score)`.
fn score_entry(ix: &EntryIndex, tokens: &[String], mode: &SearchMode) -> Option<(usize, i64)> {
    let fields = fields_for(ix, mode);
    let mut total = 0i64;
    let mut fuzzy = 0usize;
    for tok in tokens {
        let mut best = 0i64;
        let mut best_fuzzy = false;
        for &(f, weight, _, allow_fuzzy) in &fields {
            let (s, was_fuzzy) = field_score(f, tok, weight, allow_fuzzy);
            if s > best {
                best = s;
                best_fuzzy = was_fuzzy;
            }
        }
        if best == 0 {
            return None;
        }
        total += best;
        if best_fuzzy {
            fuzzy += 1;
        }
    }
    for &(f, _, adj, _) in &fields {
        if adj > 0 {
            total += phrase_bonus(&f.seq, tokens, adj);
        }
    }
    total -= LEN_PENALTY * ix.title_len.min(LEN_PENALTY_CAP) as i64;
    Some((fuzzy, total))
}

/// Rank `entries` against `query`, returning indices best-first. `keep` is the
/// caller's hard pre-filter (the source-file multiselect).
pub fn rank<F>(
    entries: &[Entry],
    index: &[EntryIndex],
    query: &str,
    mode: &SearchMode,
    frecency: &HashMap<String, i64>,
    keep: F,
) -> Vec<usize>
where
    F: Fn(&Entry) -> bool,
{
    // In RECENT mode usage leads the ordering, so favorites drop to a tiebreak
    // and unused commands sink below used ones instead of being filtered out.
    let by_use = matches!(mode, SearchMode::RECENT);
    let used = |i: usize| frecency.get(&entries[i].id).copied().unwrap_or(0);

    let tokens = query_tokens(query);
    if tokens.is_empty() {
        let mut results: Vec<usize> = (0..entries.len())
            .filter(|&i| keep(&entries[i]))
            // An empty query in RECENT mode means "show me my history", so
            // anything never run is noise rather than a low-ranked match.
            .filter(|&i| !by_use || used(i) > 0)
            .collect();
        if by_use {
            results.sort_by(|&a, &b| used(b).cmp(&used(a)).then(a.cmp(&b)));
        } else {
            results.sort_by_key(|&i| !entries[i].favorite);
        }
        return results;
    }

    let mut scored: Vec<(usize, bool, usize, i64, i64)> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if !keep(entry) {
            continue;
        }
        let Some(ix) = index.get(i) else { continue };
        if let Some((fuzzy, score)) = score_entry(ix, &tokens, mode) {
            scored.push((i, entry.favorite, fuzzy, score, used(i)));
        }
    }

    // Favorites first (they're already all-words-relevant since they matched),
    // then literal matches ahead of typo matches, then by score.
    scored.sort_by(|a, b| {
        if by_use {
            b.4.cmp(&a.4)
                .then(b.1.cmp(&a.1))
                .then(a.2.cmp(&b.2))
                .then(b.3.cmp(&a.3))
        } else {
            b.1.cmp(&a.1).then(a.2.cmp(&b.2)).then(b.3.cmp(&a.3))
        }
    });
    scored.into_iter().map(|(i, ..)| i).collect()
}

/// The filter-only gate shared by the Browse tab and the methodology jump
/// palette: does every query token match somewhere in these fields?
pub fn matches_fields(fields: &[&FieldIndex], tokens: &[String]) -> bool {
    tokens.iter().all(|tok| {
        fields
            .iter()
            .any(|f| field_score(f, tok, W_TITLE, true).0 > 0)
    })
}

/// Same gate against a one-off string (jump-palette labels, which are few).
pub fn matches_text(text: &str, tokens: &[String]) -> bool {
    let f = index_field(text);
    matches_fields(&[&f], tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(title: &str, cmd: &str, heading: &[&str]) -> Entry {
        Entry {
            id: title.chars().take(8).collect(),
            title: title.to_string(),
            cmd: cmd.to_string(),
            description: String::new(),
            source_file: PathBuf::from("/tmp/TEST-CMDs.json"),
            heading_path: heading.iter().map(|s| s.to_string()).collect(),
            favorite: false,
        }
    }

    /// The two entries from the bug report, verbatim from the corpus.
    fn corpus() -> Vec<Entry> {
        vec![
            entry(
                "Find Writable Service Binaries via icacls (PowerShell)",
                "Get-CimInstance Win32_Service | ? PathName | % { icacls $bin }",
                &["Windows PrivEsc", "I. Environmental Awareness", "Services"],
            ),
            entry(
                "Enumerate Writable Objects with bloodyAD",
                "bloodyAD -d 'DOMAIN' --host 'DC_FQDN' -u 'USER' -p 'PASSWORD' -k get writable",
                &["DACL Attacks", "Linux"],
            ),
            entry(
                "Add Shadow Credential over Kerberos with bloodyAD",
                "bloodyAD -d 'DOMAIN' -k add shadowCredentials 'TARGET'",
                &["Attribute Modification", "Shadow Credentials", "Linux"],
            ),
            entry(
                "Enumerate SMB Shares with NetExec",
                "nxc smb 'TARGET' -u USER -p PASS --shares",
                &["AD Enumeration", "Linux"],
            ),
        ]
    }

    fn titles(query: &str) -> Vec<String> {
        let e = corpus();
        let ix = build_index(&e);
        rank(&e, &ix, query, &SearchMode::ALL, &HashMap::new(), |_| true)
            .into_iter()
            .map(|i| e[i].title.clone())
            .collect()
    }

    #[test]
    fn norm_splits_camel_case_and_punctuation() {
        let f = index_field("bloodyAD");
        assert_eq!(f.seq, vec!["bloody", "ad"]);
        assert_eq!(f.extra, vec!["bloodyad"]);

        let f = index_field("Get-CimInstance Win32_Service");
        assert_eq!(f.seq, vec!["get", "cim", "instance", "win32", "service"]);
        // camelCase compounds and de-punctuated ones are both kept whole.
        assert_eq!(f.extra, vec!["ciminstance", "getciminstance", "win32service"]);
        assert_eq!(f.joined, "get ciminstance win32 service");

        // A hyphenated tool name is reachable as one typed word.
        assert!(index_field("evil-winrm").extra.contains(&"evilwinrm".to_string()));
    }

    #[test]
    fn query_is_not_camel_split() {
        // The user typed one word; it stays one token and matches `extra`.
        assert_eq!(query_tokens("bloodyAD"), vec!["bloodyad"]);
        assert_eq!(query_tokens("get  writable"), vec!["get", "writable"]);
        assert_eq!(query_tokens("Get-CimInstance"), vec!["get", "ciminstance"]);
    }

    #[test]
    fn edit_budget_scales_with_token_length() {
        assert_eq!(fuzz_budget("get"), 0);
        assert_eq!(fuzz_budget("nxc"), 0);
        assert_eq!(fuzz_budget("certipy"), 1);
        assert_eq!(fuzz_budget("blody"), 1);

        assert!(edit_within("blodyad", "bloodyad", 1));
        assert!(edit_within("secretsdmup", "secretsdump", 1)); // transposition
        assert!(!edit_within("get", "net", 0));
        // Distinct real words must not collapse into each other.
        assert!(!edit_within("kerberoast", "kerberos", 1));
    }

    #[test]
    fn get_writable_ranks_the_bloodyad_command_first() {
        let out = titles("get writable");
        assert_eq!(out[0], "Enumerate Writable Objects with bloodyAD");
        assert_eq!(out[1], "Find Writable Service Binaries via icacls (PowerShell)");
    }

    #[test]
    fn short_token_does_not_win_on_the_tool_field() {
        // `get` is a real token of `Get-CimInstance`, but covers so little of
        // the tool name that it must not outrank a genuine title match.
        let ix = index_entry(&corpus()[0]);
        let (tool, _) = field_score(&ix.tool, "get", W_TOOL, true);
        let (title, _) = field_score(&ix.title, "writable", W_TITLE, true);
        assert!(tool < title, "tool {tool} should score below title {title}");
    }

    #[test]
    fn typos_still_find_the_entry() {
        let out = titles("blodyAD");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|t| t.contains("bloodyAD")));
    }

    #[test]
    fn exact_matches_outrank_fuzzy_ones() {
        let e = corpus();
        let ix = build_index(&e);
        let tokens = query_tokens("bloodyad");
        let exact = score_entry(&ix[1], &tokens, &SearchMode::ALL).unwrap();
        let fuzzy = score_entry(&ix[1], &query_tokens("blodyad"), &SearchMode::ALL).unwrap();
        assert_eq!(exact.0, 0);
        assert_eq!(fuzzy.0, 1);
        // Fuzzy count is the sort key above score, so ordering is guaranteed
        // regardless of the raw totals.
        assert!(exact.0 < fuzzy.0);
    }

    #[test]
    fn correct_spelling_returns_the_same_set_without_fuzz() {
        assert_eq!(titles("bloodyAD").len(), titles("blodyAD").len());
    }

    #[test]
    fn all_words_gate_still_drops_partial_matches() {
        let out = titles("bloodyad shadow");
        assert_eq!(out, vec!["Add Shadow Credential over Kerberos with bloodyAD"]);
        assert!(titles("bloodyad rubeus").is_empty());
    }

    /// `extra` compounds are deliberately absent from `joined`, so the
    /// `joined` prefilter must never be the only gate on a literal match.
    #[test]
    fn hyphenated_tool_names_are_findable_as_one_word() {
        let mut e = corpus();
        e.push(entry(
            "Connect over WinRM with Evil-WinRM",
            "evil-winrm -i 'TARGET' -u 'USER' -p 'PASSWORD'",
            &["Lateral Movement", "Linux"],
        ));
        let ix = build_index(&e);
        let hit = rank(&e, &ix, "evilwinrm", &SearchMode::ALL, &HashMap::new(), |_| true);
        assert_eq!(hit.len(), 1);
        assert_eq!(e[hit[0]].title, "Connect over WinRM with Evil-WinRM");
        // and the way it is actually written
        assert_eq!(rank(&e, &ix, "evil-winrm", &SearchMode::ALL, &HashMap::new(), |_| true), hit);
    }

    #[test]
    fn favorites_pin_above_score() {
        let mut e = corpus();
        e[0].favorite = true;
        let ix = build_index(&e);
        let out = rank(&e, &ix, "writable", &SearchMode::ALL, &HashMap::new(), |_| true);
        assert_eq!(out[0], 0);
    }

    #[test]
    fn browse_gate_requires_every_token() {
        let ix = index_entry(&corpus()[3]);
        let fields = [&ix.title, &ix.heading];
        assert!(matches_fields(&fields, &query_tokens("smb shares")));
        assert!(!matches_fields(&fields, &query_tokens("smb kerberoast")));
        // camelCase compound is reachable both ways
        assert!(matches_fields(&fields, &query_tokens("netexec")));
        assert!(matches_fields(&fields, &query_tokens("net exec")));
    }
}
