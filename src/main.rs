use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use strum::Display;

use color_eyre::{Result, eyre::eyre};
use rand::RngExt;
use ratatui::widgets::ListState;
use serde::{Deserialize, Serialize};
mod methodology;
mod ui;

use methodology::MethodNode;

static PREV_SEARCH_PATH: OnceLock<String> = OnceLock::new();

pub fn get_prev_search_path() -> &'static str {
    PREV_SEARCH_PATH.get_or_init(|| {
        #[cfg(target_os = "windows")]
        return std::env::var("TEMP").unwrap_or("C:\\Windows\\Temp".into()) + "\\prev_search.txt";

        #[cfg(not(target_os = "windows"))]
        return "/tmp/prev_search.txt".to_string();
    })
}

static PREV_BROWSE_PATH: OnceLock<String> = OnceLock::new();

pub fn get_prev_browse_path() -> &'static str {
    PREV_BROWSE_PATH.get_or_init(|| {
        #[cfg(target_os = "windows")]
        return std::env::var("TEMP").unwrap_or("C:\\Windows\\Temp".into()) + "\\prev_browse.txt";

        #[cfg(not(target_os = "windows"))]
        return "/tmp/prev_browse.txt".to_string();
    })
}

static PREV_METHOD_PATH: OnceLock<String> = OnceLock::new();

pub fn get_prev_method_path() -> &'static str {
    PREV_METHOD_PATH.get_or_init(|| {
        #[cfg(target_os = "windows")]
        return std::env::var("TEMP").unwrap_or("C:\\Windows\\Temp".into()) + "\\prev_method.txt";

        #[cfg(not(target_os = "windows"))]
        return "/tmp/prev_method.txt".to_string();
    })
}
pub struct TreeNode {
    pub text: String,
    pub children: Vec<TreeNode>,
    pub entry_index: Option<usize>,
}
impl TreeNode {
    pub fn folder(text: String) -> Self {
        Self {
            text,
            children: vec![],
            entry_index: None,
        }
    }
    pub fn leaf(display_text: String, index: usize) -> Self {
        Self {
            text: display_text,
            children: vec![],
            entry_index: Some(index),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub cmd: String,
    pub description: String,
    pub source_file: PathBuf,
    pub heading_path: Vec<String>,
    /// Starred by the user — pinned above non-favorites among search matches.
    #[serde(default)]
    pub favorite: bool,
}

impl Entry {
    pub fn new() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            cmd: String::new(),
            description: String::new(),
            source_file: PathBuf::new(),
            heading_path: Vec::new(),
            favorite: false,
        }
    }
}

/// Build the autocomplete vocabulary: every word (len ≥ 2) from entry titles,
/// heading paths, and command tool names, ranked by frequency then brevity so a
/// typed prefix completes to the most common matching term. Tool names are
/// up-weighted since they're the most useful thing to complete.
fn build_vocab(entries: &[Entry]) -> Vec<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, u32> = HashMap::new();
    let add = |text: &str, weight: u32, counts: &mut HashMap<String, u32>| {
        for w in text.split(|c: char| !c.is_alphanumeric()) {
            if w.len() >= 2 {
                *counts.entry(w.to_lowercase()).or_insert(0) += weight;
            }
        }
    };
    for e in entries {
        add(&e.title, 1, &mut counts);
        for h in &e.heading_path {
            add(h, 1, &mut counts);
        }
        // The binary name (first token, path stripped) is prime completion fodder.
        if let Some(first) = e.cmd.split_whitespace().next() {
            let tool = first.rsplit('/').next().unwrap_or(first);
            if tool.len() >= 2 && tool.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')) {
                *counts.entry(tool.to_lowercase()).or_insert(0) += 3;
            }
        }
    }
    let mut words: Vec<(String, u32)> = counts.into_iter().collect();
    words.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(a.0.len().cmp(&b.0.len()))
            .then(a.0.cmp(&b.0))
    });
    words.into_iter().map(|(w, _)| w).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chain {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(
        deserialize_with = "deserialize_steps",
        serialize_with = "serialize_steps"
    )]
    pub steps: Vec<String>,
}

fn deserialize_steps<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Step {
        entry_id: String,
    }
    let steps = Vec::<Step>::deserialize(deserializer)?;
    std::result::Result::Ok(steps.into_iter().map(|s| s.entry_id).collect())
}

fn serialize_steps<S>(steps: &Vec<String>, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct Step {
        entry_id: String,
    }
    let wrapped: Vec<Step> = steps
        .iter()
        .map(|id| Step {
            entry_id: id.clone(),
        })
        .collect();
    wrapped.serialize(serializer)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainsFile {
    pub chains: Vec<Chain>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntriesFile {
    pub entries: Vec<Entry>,
}

/// Summary of a bulk import: how many entries were added, how many were skipped
/// as duplicates, a per-file breakdown of additions, and any per-block parse errors.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub added: usize,
    pub skipped: usize,
    pub added_by_file: HashMap<String, usize>,
    pub errors: Vec<(usize, String)>,
}

#[derive(Debug, Display)]
pub enum SearchMode {
    CMD,
    HEADING,
    TITLE,
    ALL,
}

/// One methodology document, loaded from a JSONs/methodology/*.md file. Fully
/// independent of the others — its own sections, cards, and inline check state.
pub struct MethodDoc {
    pub name: String,
    pub path: PathBuf,
    pub tree: Vec<MethodNode>,
}

/// Saved view position within a single section of a document — the card, the
/// selected checklist row, and which pane had focus. Keyed per (doc, section)
/// so every section remembers exactly where you left it.
#[derive(Clone, Default)]
pub struct MethodPos {
    pub card: usize,
    pub tree_sel: Option<usize>,
    pub focus: bool,
}

/// The focusable panes on the Search tab. In nav mode, h/l (or ←/→) cycle focus
/// across them and j/k (or ↑/↓) act on whichever is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchPane {
    #[default]
    Results,
    Description,
    Chain,
}

pub struct App {
    pub top_tab: usize,
    pub entries: Vec<Entry>,
    pub browse_state: ListState,
    /// Incremental filter text for the Browse tab.
    pub browse_query: String,
    /// Which field(s) the Browse filter matches against (cycled with Tab).
    pub browse_mode: SearchMode,
    /// Folders the user has collapsed *while filtering* (reset when the filter
    /// text changes). Filtered folders are expanded by default.
    pub browse_collapsed: HashSet<String>,
    /// File-name filter options: `"All"` at index 0, then each source-file stem.
    /// Used for the ⌘F popup's numbered list.
    pub file_filters: Vec<String>,
    /// The set of selected source-file stems (multi-select). Empty = show all.
    /// Shared by the Search and Browse tabs.
    pub file_selected: HashSet<String>,
    pub query: String,
    pub mode: SearchMode,
    pub list_state: ListState,
    /// Search tab: focus is on the results list (j/k navigate) vs the query
    /// input (typing). Transient UI state, not persisted.
    pub search_nav: bool,
    /// Search tab (nav mode only): which of the three panes is focused.
    pub search_focus: SearchPane,
    /// Vertical scroll offset of the Search description pane. Reset to 0 whenever
    /// the selected command changes.
    pub desc_scroll: u16,
    /// The last right-hand pane (Description or Chain) focus was on, so h/l
    /// toggles back to it from the results pane.
    pub last_right_pane: SearchPane,
    /// Highlighted step index within the currently displayed attack chain.
    pub chain_sel: usize,
    /// Search-tab pane split ratios (percent), adjusted with Shift+HJKL:
    /// `main_split_pct` = results-column width, `right_split_pct` = description height.
    pub main_split_pct: u16,
    pub right_split_pct: u16,
    /// Whether the ⌘F file-filter popup is open.
    pub file_filter_active: bool,
    /// Browse tab: same nav-vs-input focus for the folder filter.
    pub browse_nav: bool,
    /// Methodology jump palette: nav-vs-input focus while `/` is active.
    pub method_jump_nav: bool,
    /// Expanded folder keys in the Browse tab (folder path joined by NUL).
    pub expanded: HashSet<String>,
    /// Loaded methodology documents (one per JSONs/methodology/*.md). Each is a
    /// fully independent checklist; checked state lives inline in its markdown.
    pub method_docs: Vec<MethodDoc>,
    /// Active document (index into `method_docs`), switched with Super+F.
    pub method_doc: usize,
    /// The section each document was last on (one per `method_docs` entry), so
    /// returning to a document lands on the right section.
    pub method_doc_section: Vec<usize>,
    /// Saved position per (doc, section): card, selected row, pane focus. The
    /// active (doc, section) is synced from the live fields on switch/save.
    pub method_pos: HashMap<(usize, usize), MethodPos>,
    /// Active section sub-tab (0-based index into the doc's `sections`).
    pub method_section: usize,
    /// Selected attack card in the left list of the active section.
    pub method_card: usize,
    /// Selection in the right-hand detail checklist tree.
    pub method_tree_state: ListState,
    /// Which pane has focus: false = cards list, true = detail tree.
    pub method_focus: bool,
    /// Jump-to-technique search query (activated with `/`).
    pub method_query: String,
    /// Whether the jump palette is currently active.
    pub method_jump_active: bool,
    /// Selected candidate in the jump palette.
    pub method_jump_sel: usize,
    /// Whether a "reset all checks" confirmation is pending (y/n).
    pub method_pending_reset: bool,
    /// Whether floating comments (Note rows) are shown (toggled with `c`).
    pub method_show_comments: bool,
    /// Transient: a `g` was pressed and we're waiting for the second `g` (vim gg).
    pub method_g_pending: bool,
    /// Keys ("doc/section/card/idx-path") of collapsed methodology headings.
    pub method_collapsed: HashSet<String>,
    pub results: Vec<usize>,
    /// Frequency-ranked word list (titles, headings, tool names) for inline
    /// autocomplete on the Search and Browse inputs.
    pub vocab: Vec<String>,
    pub cursor_index: usize,
    pub chains: Vec<Chain>,
    pub entry_index: HashMap<String, usize>,
    pub is_chain_edit_mode: bool,
    pub prev_selected_entry_id: String,
    pub current_chain_index: usize,
    pub cmds_dir: PathBuf,
    pub chains_dir: PathBuf,
    pub dirty: bool,
}

impl App {
    pub fn new(
        entries: Vec<Entry>,
        chains: Vec<Chain>,
        cmds_dir: PathBuf,
        chains_dir: PathBuf,
        method_docs: Vec<MethodDoc>,
    ) -> Self {
        let mut list_state = ListState::default();
        if !entries.is_empty() {
            list_state.select(Some(0));
        }
        // Restore persisted Browse view + filter:
        //   line 0: top_tab, line 1: selected row, line 2: filter query,
        //   line 3: filter mode, line 4+: expanded folder keys (one per line).
        let browse_saved = fs::read_to_string(get_prev_browse_path()).unwrap_or_default();
        let mut browse_lines = browse_saved.lines();
        let saved_top_tab = browse_lines
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
            .min(2);

        // Restore persisted Methodology state:
        //   line 0: active doc; then `S<doc>:<section>` (per-doc active section),
        //   `P<doc>:<section>:<card>:<sel>:<foc>` (per-section position),
        //   `C<key>` collapsed headings, `V<0|1>` comment visibility.
        //   (Checked state is inline in each doc's markdown.)
        let method_saved = fs::read_to_string(get_prev_method_path()).unwrap_or_default();
        let mut method_lines = method_saved.lines();
        let saved_method_doc = method_lines
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let mut method_collapsed: HashSet<String> = HashSet::new();
        let mut method_show_comments = true;
        let mut saved_sections: HashMap<usize, usize> = HashMap::new();
        let mut method_pos: HashMap<(usize, usize), MethodPos> = HashMap::new();
        for line in method_lines {
            if let Some(k) = line.strip_prefix('C') {
                method_collapsed.insert(k.to_owned());
            } else if let Some(v) = line.strip_prefix('V') {
                method_show_comments = v != "0";
            } else if let Some(rest) = line.strip_prefix('S') {
                let mut it = rest.split(':');
                if let (Some(d), Some(s)) = (
                    it.next().and_then(|s| s.parse::<usize>().ok()),
                    it.next().and_then(|s| s.parse::<usize>().ok()),
                ) {
                    saved_sections.insert(d, s);
                }
            } else if let Some(rest) = line.strip_prefix('P') {
                let mut it = rest.split(':');
                if let (Some(d), Some(s)) = (
                    it.next().and_then(|s| s.parse::<usize>().ok()),
                    it.next().and_then(|s| s.parse::<usize>().ok()),
                ) {
                    let card = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let tree_sel = it.next().and_then(|s| s.parse::<usize>().ok());
                    let focus = it.next() == Some("1");
                    method_pos.insert((d, s), MethodPos { card, tree_sel, focus });
                }
            }
        }
        let method_doc = if method_docs.is_empty() {
            0
        } else {
            saved_method_doc.min(method_docs.len() - 1)
        };
        // Per-doc last-active section, clamped to that doc's section count.
        let method_doc_section: Vec<usize> = method_docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let n = methodology::sections(&d.tree).len();
                let s = saved_sections.get(&i).copied().unwrap_or(0);
                if n == 0 { 0 } else { s.min(n - 1) }
            })
            .collect();
        let method_section = method_doc_section.get(method_doc).copied().unwrap_or(0);
        let active_pos = method_pos.get(&(method_doc, method_section)).cloned().unwrap_or_default();
        let method_card = active_pos.card;
        let method_focus = active_pos.focus;
        let mut method_tree_state = ListState::default();
        method_tree_state.select(active_pos.tree_sel.or(Some(0)));
        let saved_browse_sel = browse_lines.next().and_then(|s| s.parse::<usize>().ok());
        let saved_browse_query = browse_lines.next().unwrap_or("").to_owned();
        let saved_browse_mode = match browse_lines.next().unwrap_or("ALL") {
            "CMD" => SearchMode::CMD,
            "HEADING" => SearchMode::HEADING,
            "TITLE" => SearchMode::TITLE,
            _ => SearchMode::ALL,
        };
        let saved_expanded: HashSet<String> = browse_lines
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned())
            .collect();

        let mut browse_state = ListState::default();
        if !entries.is_empty() {
            browse_state.select(Some(saved_browse_sel.unwrap_or(0)));
        }

        // File-filter options: "All" plus each distinct source-file stem.
        let mut stems: Vec<String> = entries
            .iter()
            .filter_map(|e| {
                e.source_file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        stems.sort();
        stems.dedup();
        let mut file_filters = vec!["All".to_string()];
        file_filters.extend(stems);

        // Restore the selected file stems (persisted as a `+`-joined list on
        // line 2). Only keep names that still exist as filter options.
        let saved_file = fs::read_to_string(get_prev_search_path())
            .unwrap_or_default()
            .lines()
            .nth(2)
            .unwrap_or("")
            .to_owned();
        let file_selected: HashSet<String> = saved_file
            .split('+')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "All" && file_filters.iter().any(|f| f == s))
            .collect();

        let entry_index = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.clone(), i))
            .collect();
        let vocab = build_vocab(&entries);
        Self {
            entries,
            vocab,
            query: fs::read_to_string(get_prev_search_path())
                .unwrap_or(String::new())
                .lines()
                .nth(0)
                .unwrap_or("")
                .to_owned(),
            mode: match fs::read_to_string(get_prev_search_path())
                .unwrap_or(String::new())
                .lines()
                .nth(1)
                .unwrap_or("ALL")
            {
                "CMD" => SearchMode::CMD,
                "HEADING" => SearchMode::HEADING,
                "TITLE" => SearchMode::TITLE,
                _ => SearchMode::ALL,
            },
            top_tab: saved_top_tab,
            list_state,
            search_nav: false,
            browse_nav: false,
            method_jump_nav: false,
            expanded: saved_expanded,
            method_docs,
            method_doc,
            method_doc_section,
            method_pos,
            method_section,
            method_card,
            method_tree_state,
            method_focus,
            method_query: String::new(),
            method_jump_active: false,
            method_jump_sel: 0,
            method_pending_reset: false,
            method_show_comments,
            method_g_pending: false,
            method_collapsed,
            browse_state,
            browse_query: saved_browse_query,
            browse_mode: saved_browse_mode,
            browse_collapsed: HashSet::new(),
            file_filters,
            file_selected,
            results: vec![],
            cursor_index: fs::read_to_string(get_prev_search_path())
                .unwrap_or(String::new())
                .lines()
                .nth(0)
                .unwrap_or("")
                .len(),
            chains,
            entry_index,
            is_chain_edit_mode: false,
            prev_selected_entry_id: String::from(""),
            current_chain_index: 0,
            search_focus: SearchPane::Results,
            desc_scroll: 0,
            last_right_pane: SearchPane::Description,
            chain_sel: 0,
            main_split_pct: 60,
            right_split_pct: 60,
            file_filter_active: false,
            cmds_dir,
            chains_dir,
            dirty: false,
        }
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.list_state
            .selected()
            .and_then(|filtered_index| self.results.get(filtered_index))
            .and_then(|&i| self.entries.get(i))
    }

    pub fn selected_entry_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|i| self.results.get(i).copied())
    }

    pub fn rebuild_entry_index(&mut self) {
        self.entry_index = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.clone(), i))
            .collect();
    }

    pub fn sanitize_source_path(&self, raw: &PathBuf) -> PathBuf {
        let filename = raw
            .file_name()
            .unwrap_or_else(|| OsStr::new("unknown-CMDs.json"));
        self.cmds_dir.join(filename)
    }

    pub fn write_entries_to_json(&self) -> Result<()> {
        let mut entries_by_filename: HashMap<PathBuf, EntriesFile> = HashMap::new();

        for entry in &self.entries {
            let safe_path = self.sanitize_source_path(&entry.source_file);
            entries_by_filename
                .entry(safe_path)
                .or_insert(EntriesFile { entries: vec![] })
                .entries
                .push(entry.clone());
        }

        for (filepath, ef) in &entries_by_filename {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(filepath)?;
            serde_json::to_writer_pretty(&mut file, &ef)?;
        }

        if !entries_by_filename.is_empty() {
            for dir_entry in fs::read_dir(&self.cmds_dir)? {
                let path = dir_entry?.path();
                if path.extension() != Some(OsStr::new("json")) {
                    continue;
                }
                if !entries_by_filename.contains_key(&path) {
                    fs::remove_file(&path)?;
                }
            }
        }

        Ok(())
    }

    /// Bulk-import cmd-maker template blocks from `path` into `self.entries`.
    /// Splits the file on `--- TITLE ---` header lines, parses each block with the
    /// same parser the $EDITOR flow uses, skips any block whose (title, command,
    /// target file) already exists, and appends the rest with fresh ids. Does not
    /// touch disk — the caller invokes `write_entries_to_json` afterward.
    pub fn import_commands_file(&mut self, path: &Path) -> Result<ImportReport> {
        let text = fs::read_to_string(path)
            .map_err(|e| eyre!("cannot read import file {}: {e}", path.display()))?;

        // Split into blocks, one per `--- TITLE ---` header. Anything before the
        // first header is ignored; each block keeps its own header line.
        let mut blocks: Vec<String> = Vec::new();
        let mut cur: Option<String> = None;
        for line in text.lines() {
            if line.trim() == "--- TITLE ---" {
                if let Some(b) = cur.take() {
                    blocks.push(b);
                }
                cur = Some(String::new());
            }
            if let Some(b) = cur.as_mut() {
                b.push_str(line);
                b.push('\n');
            }
        }
        if let Some(b) = cur.take() {
            blocks.push(b);
        }

        let mut report = ImportReport::default();
        let mut rng = rand::rng();

        for (i, block) in blocks.iter().enumerate() {
            let id = format!("{:08x}", rng.random::<u32>());
            match ui::parse_template_str(&id, block, &self.cmds_dir, false) {
                Ok(entry) => {
                    let target = self.sanitize_source_path(&entry.source_file);
                    let dup = self.entries.iter().any(|e| {
                        e.title == entry.title
                            && e.cmd == entry.cmd
                            && self.sanitize_source_path(&e.source_file) == target
                    });
                    if dup {
                        report.skipped += 1;
                    } else {
                        let fname = target
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        *report.added_by_file.entry(fname).or_insert(0) += 1;
                        self.entries.push(entry);
                        report.added += 1;
                    }
                }
                Err(e) => report.errors.push((i + 1, e.to_string())),
            }
        }

        self.rebuild_entry_index();
        Ok(report)
    }

    pub fn write_chains_to_json(&mut self) -> Result<()> {
        let mut chains_by_filename: HashMap<PathBuf, ChainsFile> = HashMap::new();

        for chain in &self.chains {
            let mut source_entry: Option<&Entry> = None;
            for entry_id in &chain.steps {
                if let Some(&index) = self.entry_index.get(entry_id) {
                    if let Some(entry) = self.entries.get(index) {
                        source_entry = Some(entry);
                        break;
                    }
                }
            }
            let out_path = match source_entry {
                Some(entry) => {
                    let safe_path = self.sanitize_source_path(&entry.source_file);
                    let stem = safe_path.file_stem().unwrap_or_default().to_string_lossy();
                    self.chains_dir.join(format!("{}-chains.json", stem))
                }
                None => self.chains_dir.join("orphaned-chains.json"),
            };

            chains_by_filename
                .entry(out_path)
                .or_insert(ChainsFile { chains: vec![] })
                .chains
                .push(chain.clone());
        }

        for (filepath, cf) in &chains_by_filename {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(filepath)?;
            serde_json::to_writer_pretty(&mut file, &cf)?;
        }

        if !chains_by_filename.is_empty() && self.chains_dir.exists() {
            for dir_entry in fs::read_dir(&self.chains_dir)? {
                let path = dir_entry?.path();
                if path.extension() != Some(OsStr::new("json")) {
                    continue;
                }
                if !chains_by_filename.contains_key(&path) {
                    fs::remove_file(&path)?;
                }
            }
        }

        Ok(())
    }

    pub fn find_chain_for_entry_mut(&mut self, entry_id: &str) -> Option<&mut Chain> {
        self.chains.iter_mut().find(|c| {
            c.steps
                .iter()
                .any(|current_entry_id| current_entry_id == entry_id)
        })
    }

    pub fn find_chains_for_entry<'a>(&'a self, entry_id: &str) -> Vec<&'a Chain> {
        self.chains
            .iter()
            .filter(|c| c.steps.iter().any(|step| step == entry_id))
            .collect()
    }

    pub fn resolve_chain_steps<'a>(&'a self, chain: &Chain) -> Vec<&'a Entry> {
        chain
            .steps
            .iter()
            .filter_map(|entry_id| {
                self.entry_index
                    .get(entry_id)
                    .and_then(|i| self.entries.get(*i))
            })
            .collect()
    }

    /// Whether an entry passes the file filter (no selection = all pass).
    pub fn entry_passes_file(&self, entry: &Entry) -> bool {
        if self.file_selected.is_empty() {
            return true;
        }
        entry
            .source_file
            .file_stem()
            .map(|s| self.file_selected.contains(&s.to_string_lossy().into_owned()))
            .unwrap_or(false)
    }

    /// A short label for the active filter: "all", the single stem, or "N files".
    pub fn file_filter_label(&self) -> String {
        match self.file_selected.len() {
            0 => "all".to_string(),
            1 => self.file_selected.iter().next().cloned().unwrap_or_default(),
            n => format!("{n} files"),
        }
    }

    pub fn save_prev_search(&self) {
        // Persist the selected stems as a `+`-joined list (in filter order).
        let sel: Vec<&str> = self
            .file_filters
            .iter()
            .filter(|f| self.file_selected.contains(*f))
            .map(|s| s.as_str())
            .collect();
        let file = if sel.is_empty() {
            "All".to_string()
        } else {
            sel.join("+")
        };
        let _ = fs::write(
            get_prev_search_path(),
            format!("{}\n{}\n{}", self.query, self.mode, file),
        );
    }

    pub fn save_prev_browse(&self) {
        let sel = self
            .browse_state
            .selected()
            .map(|i| i.to_string())
            .unwrap_or_default();
        let expanded: Vec<&str> = self.expanded.iter().map(|s| s.as_str()).collect();
        let _ = fs::write(
            get_prev_browse_path(),
            format!(
                "{}\n{}\n{}\n{}\n{}",
                self.top_tab,
                sel,
                self.browse_query,
                self.browse_mode,
                expanded.join("\n")
            ),
        );
    }

    pub fn save_prev_method(&self) {
        // line 0: active doc; then `V<0|1>`, `S<doc>:<section>` per-doc active
        // section, `P<doc>:<section>:<card>:<sel>:<foc>` per-section positions,
        // and `C<key>` collapsed-heading keys. The active (doc, section) reflects
        // the live fields.
        let mut out = format!("{}\nV{}\n", self.method_doc, if self.method_show_comments { 1 } else { 0 });
        for i in 0..self.method_docs.len() {
            let sec = if i == self.method_doc {
                self.method_section
            } else {
                self.method_doc_section.get(i).copied().unwrap_or(0)
            };
            out.push_str(&format!("S{}:{}\n", i, sec));
        }
        // Positions, with the active (doc, section) overridden by live state.
        let mut pos = self.method_pos.clone();
        pos.insert(
            (self.method_doc, self.method_section),
            MethodPos {
                card: self.method_card,
                tree_sel: self.method_tree_state.selected(),
                focus: self.method_focus,
            },
        );
        for ((d, s), p) in &pos {
            let sel = p.tree_sel.map(|n| n.to_string()).unwrap_or_default();
            out.push_str(&format!("P{}:{}:{}:{}:{}\n", d, s, p.card, sel, if p.focus { 1 } else { 0 }));
        }
        for k in &self.method_collapsed {
            out.push('C');
            out.push_str(k);
            out.push('\n');
        }
        let _ = fs::write(get_prev_method_path(), out);
    }

    /// The active methodology document's parsed tree (empty if none loaded).
    pub fn method_tree(&self) -> &[MethodNode] {
        self.method_docs
            .get(self.method_doc)
            .map(|d| d.tree.as_slice())
            .unwrap_or(&[])
    }

    /// The active document's source markdown path.
    pub fn method_path(&self) -> Option<&Path> {
        self.method_docs.get(self.method_doc).map(|d| d.path.as_path())
    }

    /// Re-parse the active document from disk (after a checkbox flip or edit).
    pub fn method_reload(&mut self) {
        if let Some(d) = self.method_docs.get_mut(self.method_doc) {
            if let Ok(md) = fs::read_to_string(&d.path) {
                d.tree = methodology::parse(&md);
            }
        }
    }
}

fn print_usage() {
    println!(
        "f1nder — fuzzy search / browse / methodology for pentest commands\n\n\
         Usage:\n  \
         f1nder                 launch the TUI\n  \
         f1nder --import FILE   bulk-import cmd-maker template blocks from FILE\n  \
         f1nder --help          show this help\n\n\
         --import (-i) reads a file of `--- TITLE ---` … `--- COMMANDS ---` blocks,\n\
         routes each to JSONs/cmds/<SOURCE-FILE>-CMDs.json, and skips duplicates."
    );
}

fn main() -> Result<()> {
    color_eyre::install()?;

    // Minimal hand-rolled argv parsing (no clap dependency). With no args we
    // launch the TUI as before; `--import FILE` runs a headless bulk import.
    let mut import_file: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            "-i" | "--import" => {
                let f = args.next().ok_or_else(|| {
                    eyre!("--import requires a FILE argument (e.g. f1nder --import commands.md)")
                })?;
                import_file = Some(PathBuf::from(f));
            }
            other => {
                return Err(eyre!(
                    "unknown argument: {other}\n\nRun `f1nder --help` for usage."
                ));
            }
        }
    }

    let exe_path = std::env::current_exe()?;
    let root = exe_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| eyre!("Could not determine root path from executable location"))?
        .canonicalize()?;

    let cmds_dir = root.join("JSONs/cmds");
    let chains_dir = root.join("JSONs/chains");

    if !(cmds_dir.exists() && chains_dir.exists()) {
        return Err(eyre!(
            "Cannot find JSON dirs:\n  {}\n  {}",
            cmds_dir.display(),
            chains_dir.display()
        ));
    }

    let mut entries: Vec<Entry> = Vec::new();
    for dir_entry in fs::read_dir(&cmds_dir)? {
        let path = dir_entry?.path();
        if path.extension() != Some(OsStr::new("json")) {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let ef: EntriesFile = serde_json::from_str(&text)?;
        for mut e in ef.entries {
            // Always override source_file with the canonical path we just read from.
            e.source_file = path.clone();
            entries.push(e);
        }
    }

    let mut chains: Vec<Chain> = Vec::new();
    for dir_entry in fs::read_dir(&chains_dir)? {
        let path = dir_entry?.path();
        if path.extension() != Some(OsStr::new("json")) {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let cf: ChainsFile = serde_json::from_str(&text)?;
        chains.extend(cf.chains);
    }

    // Purge steps that no longer resolve to an existing command (e.g. a command
    // was deleted while it was a chain step) and drop any chain left with fewer
    // than two real steps — the same invariant the in-app delete paths keep, so
    // a deleted command can never linger in the attack chain. `chains_healed`
    // marks that on-disk data was stale so we rewrite it on quit.
    let valid_ids: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();
    let steps_before: usize = chains.iter().map(|c| c.steps.len()).sum();
    let chains_before = chains.len();
    for chain in &mut chains {
        chain.steps.retain(|id| valid_ids.contains(id));
    }
    chains.retain(|chain| chain.steps.len() >= 2);
    let chains_healed = chains.len() != chains_before
        || chains.iter().map(|c| c.steps.len()).sum::<usize>() != steps_before;

    // Load methodology documents from JSONs/methodology/*.md (each a separate,
    // switchable checklist). Falls back to a legacy single JSONs/methodology.md.
    let mut method_docs: Vec<MethodDoc> = Vec::new();
    let method_dir = root.join("JSONs/methodology");
    if method_dir.is_dir() {
        let mut files: Vec<PathBuf> = fs::read_dir(&method_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension() == Some(OsStr::new("md")))
            .collect();
        files.sort();
        for path in files {
            if let Ok(md) = fs::read_to_string(&path) {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_uppercase())
                    .unwrap_or_default();
                method_docs.push(MethodDoc {
                    name,
                    path,
                    tree: methodology::parse(&md),
                });
            }
        }
    } else if let Ok(md) = fs::read_to_string(root.join("JSONs/methodology.md")) {
        method_docs.push(MethodDoc {
            name: "METHODOLOGY".to_string(),
            path: root.join("JSONs/methodology.md"),
            tree: methodology::parse(&md),
        });
    }

    let mut app = App::new(entries, chains, cmds_dir, chains_dir, method_docs);
    // Persist the cleaned-up chains on quit if load had to heal stale steps.
    if chains_healed {
        app.dirty = true;
    }

    // Headless bulk-import path: ingest the file, write the affected JSONs, and
    // exit without launching the TUI. `app.entries` already holds the full
    // existing set, so write_entries_to_json regrouping is non-destructive.
    if let Some(import_file) = import_file {
        let report = app.import_commands_file(&import_file)?;
        app.write_entries_to_json()?;

        let skipped = if report.skipped > 0 {
            format!(", skipped {} duplicate(s)", report.skipped)
        } else {
            String::new()
        };
        println!("Imported {} command(s){}.", report.added, skipped);
        let mut files: Vec<_> = report.added_by_file.into_iter().collect();
        files.sort();
        for (f, n) in files {
            println!("  {n:>4}  {f}");
        }
        if !report.errors.is_empty() {
            eprintln!("\n{} block(s) skipped due to errors:", report.errors.len());
            for (i, e) in &report.errors {
                eprintln!("  block {i}: {e}");
            }
        }
        return Ok(());
    }

    ratatui::run(|terminal| ui::run_event_loop(terminal, &mut app))?;

    if app.dirty {
        app.write_entries_to_json()?;
        app.write_chains_to_json()?;
    }
    app.save_prev_search();
    app.save_prev_browse();
    app.save_prev_method();

    Ok(())
}
