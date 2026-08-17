use rand::RngExt;
use ratatui::widgets::Wrap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::stdout;
use std::path::{Path, PathBuf};

use crate::methodology::{MethodKind, MethodNode};
use crate::{App, Chain, Entry, MethodPos, SearchMode, SearchPane, TreeNode};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph,
};
use ratatui::{DefaultTerminal, Frame};
use std::process::Command;
use std::sync::OnceLock;

// Palette — tuned for a dark terminal. Three text tiers (bright > body > dim)
// plus a fainter guide tone, so content reads above the recessive frames.
const C_BORDER: Color = Color::Rgb(66, 76, 98); // subtle frames that recede behind content
const C_DIM: Color = Color::Rgb(128, 140, 162); // tier-3: breadcrumbs / secondary text
const C_GUIDE: Color = Color::Rgb(80, 90, 112); // tree connector guides (fainter than text)
const C_FG_BRIGHT: Color = Color::Rgb(230, 236, 249); // tier-1: primary text
const C_ACCENT: Color = Color::Rgb(104, 202, 255); // cyan accent (tabs, mode badge)
const C_ACCENT_BG: Color = Color::Rgb(11, 19, 31); // ink behind accent pills
const C_HIGHLIGHT_BG: Color = Color::Rgb(33, 48, 67); // selected row (focused pane)
const C_HIGHLIGHT_DIM: Color = Color::Rgb(22, 30, 43); // selected row (unfocused pane)
const C_TITLE: Color = Color::Rgb(182, 193, 216); // tier-2: body / subtitle text
const C_DESC: Color = Color::Rgb(154, 164, 186); // description body text
const C_STAR: Color = Color::Rgb(242, 202, 96); // favorite star (warm gold)
const C_CHIP_BG: Color = Color::Rgb(24, 42, 62); // muted accent chip (soft, not loud cyan)

// Nerd Font glyphs (JetBrainsMono Nerd Font Mono renders each as one cell; we
// pad with a trailing space for a 2-column marker aligned like the old symbols).
const IC_SEARCH: &str = "\u{f002}"; //
const IC_BROWSE: &str = "\u{f03a}"; //
const IC_METHOD: &str = "\u{f0ae}"; //
const IC_STAR: &str = "\u{2726}"; // ✦ (favorite marker, recolored gold)
const IC_FOLDER: &str = "\u{f07b}"; //
const IC_FOLDER_OPEN: &str = "\u{f07c}"; //
const IC_CMD: &str = "\u{f120}"; //
const IC_CHECK_ON: &str = "\u{f046}"; //
const IC_CHECK_OFF: &str = "\u{f096}"; //
const IC_SECTION: &str = "\u{f0ca}"; //
const IC_ITEM: &str = "\u{f105}"; //

static EDITOR_TEMP_PATH: OnceLock<String> = OnceLock::new();

pub fn get_editor_temp_path() -> &'static str {
    EDITOR_TEMP_PATH.get_or_init(|| {
        #[cfg(target_os = "windows")]
        return std::env::var("TEMP").unwrap_or("C:\\Windows\\Temp".into()) + "\\temp.txt";

        #[cfg(not(target_os = "windows"))]
        return "/tmp/f1nder_editor_temp.txt".to_string();
    })
}

enum Section {
    None,
    Title,
    HeadingPath,
    Description,
    Commands,
    SourceFile,
}

pub fn run_event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    search(app, false);
    loop {
        terminal.draw(|frame| render(frame, app))?;
        if handle_key_event(app, terminal)? {
            break Ok(());
        }
    }
}

fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("clip").stdin(Stdio::piped()).spawn() {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            return child.wait().map(|s| s.success()).unwrap_or(false);
        }
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            return child.wait().map(|s| s.success()).unwrap_or(false);
        }
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        // Try xsel first, fall back to xclip
        let result = Command::new("xsel")
            .args(["--clipboard", "--input"])
            .stdin(Stdio::piped())
            .spawn();

        let mut child = match result {
            Ok(c) => c,
            Err(_) => match Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => return false,
            },
        };

        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        return child.wait().map(|s| s.success()).unwrap_or(false);
    }
}

fn entry_to_template(entry: &Entry) -> String {
    let mut out = String::new();

    out.push_str("--- TITLE ---\n");
    out.push_str(&entry.title);
    out.push('\n');

    out.push_str("--- HEADING_PATH ---\n");
    out.push_str(&entry.heading_path.join(" > "));
    out.push('\n');

    out.push_str("--- DESCRIPTION ---\n");
    out.push_str(&entry.description);
    out.push('\n');

    out.push_str("--- SOURCE-FILE ---\n");
    out.push_str(&entry.source_file.to_str().unwrap_or_default());
    out.push('\n');

    out.push_str("--- COMMANDS ---\n");
    out.push_str(&entry.cmd);
    out.push('\n');

    out
}

// Pure template parser: no temp file, no App. Parses one cmd-maker block
// (`--- TITLE ---` … `--- COMMANDS ---`), anchoring SOURCE-FILE under `cmds_dir`.
// Reused by the interactive $EDITOR flow and by the CLI bulk-import path.
pub fn parse_template_str(
    entry_id: &str,
    contents: &str,
    cmds_dir: &Path,
    favorite: bool,
) -> Result<Entry> {
    let mut section = Section::None;

    let mut title = String::new();
    let mut heading_raw = String::new();
    let mut description = String::new();
    let mut cmd = String::new();
    let mut source_file = String::new();

    for line in contents.lines() {
        match line.trim() {
            "--- TITLE ---" => {
                section = Section::Title;
                continue;
            }
            "--- HEADING_PATH ---" => {
                section = Section::HeadingPath;
                continue;
            }
            "--- DESCRIPTION ---" => {
                section = Section::Description;
                continue;
            }
            "--- COMMANDS ---" => {
                section = Section::Commands;
                continue;
            }
            "--- SOURCE-FILE ---" => {
                section = Section::SourceFile;
                continue;
            }
            _ => {}
        }

        match section {
            Section::Title => {
                title.push_str(line);
                title.push('\n');
            }
            Section::HeadingPath => {
                heading_raw.push_str(line);
                heading_raw.push('\n');
            }
            Section::Description => {
                description.push_str(line);
                description.push('\n');
            }
            Section::Commands => {
                if line.trim_start().starts_with('#') {
                    continue;
                }
                cmd.push_str(line);
                cmd.push('\n');
            }
            Section::SourceFile => {
                if line.trim_start().starts_with('#') {
                    continue;
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Extract just the filename, discarding any directory components
                let mut filename = Path::new(trimmed)
                    .file_name()
                    .unwrap_or_else(|| OsStr::new(trimmed))
                    .to_string_lossy()
                    .to_string();

                // Strip .json and -CMDs suffixes, then re-add canonical form
                filename = filename
                    .trim_end_matches(".json")
                    .trim_end_matches(".JSON")
                    .trim_end_matches("-CMDs")
                    .trim_end_matches("-cmds")
                    .to_string();

                filename = format!("{}-CMDs.json", filename);

                // Always anchor under JSONs/cmds/
                let full_path = cmds_dir.join(filename);

                source_file.push_str(full_path.to_string_lossy().as_ref());
                source_file.push('\n');
            }
            Section::None => {}
        }
    }

    if title.trim().is_empty() {
        return Err(eyre!("missing or empty TITLE section"));
    }
    if heading_raw.trim().is_empty() {
        return Err(eyre!("missing or empty HEADING_PATH section"));
    }
    if description.trim().is_empty() {
        return Err(eyre!("missing or empty DESCRIPTION section"));
    }
    if cmd.trim().is_empty() {
        return Err(eyre!("missing or empty COMMANDS section"));
    }

    let new_entry = Entry {
        id: entry_id.to_string(),
        title: title.trim().to_string(),
        cmd: cmd.trim().to_string(),
        description: description.trim().to_string(),
        heading_path: heading_raw
            .trim()
            .split(" > ")
            .map(|s| s.trim().to_string())
            .collect(),
        source_file: PathBuf::from(source_file.trim()),
        favorite,
    };
    Ok(new_entry)
}

fn parse_template(entry_id: &str, app: &App) -> Result<Entry> {
    let contents = fs::read_to_string(get_editor_temp_path())?;
    // Preserve the star across an in-place edit (new entries default false).
    let favorite = app
        .entry_index
        .get(entry_id)
        .and_then(|&i| app.entries.get(i))
        .map(|e| e.favorite)
        .unwrap_or(false);
    parse_template_str(entry_id, &contents, &app.cmds_dir, favorite)
}

fn open_editor(path: &str) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "windows")]
    {
        Command::new("nvim").arg(path).status()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".to_string());
        Command::new("sh")
            .arg("-c")
            .arg(format!("{} {}", editor, path))
            .status()
    }
}

/// Open `$EDITOR` with the cursor on `line` (1-based). The `+N` flag is honored
/// by nvim/vim/nano/emacs; falls back gracefully if the editor ignores it.
fn open_editor_at(path: &str, line: usize) -> std::io::Result<std::process::ExitStatus> {
    let line = line.max(1);
    #[cfg(target_os = "windows")]
    {
        Command::new("nvim")
            .arg(format!("+{}", line))
            .arg(path)
            .status()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".to_string());
        Command::new("sh")
            .arg("-c")
            .arg(format!("{} +{} {}", editor, line, path))
            .status()
    }
}

/// Move the Search results selection down (clamped to the last row).
fn search_sel_down(app: &mut App) {
    let len = app.results.len();
    if len == 0 {
        return;
    }
    let i = app
        .list_state
        .selected()
        .map(|i| (i + 1).min(len - 1))
        .unwrap_or(0);
    app.list_state.select(Some(i));
    app.current_chain_index = 0;
    app.desc_scroll = 0;
    app.chain_sel = 0;
}

/// Move the Search results selection up (clamped to the first row).
fn search_sel_up(app: &mut App) {
    if let Some(i) = app.list_state.selected() {
        app.list_state.select(Some(i.saturating_sub(1)));
    } else if !app.results.is_empty() {
        app.list_state.select(Some(0));
    }
    app.current_chain_index = 0;
    app.desc_scroll = 0;
    app.chain_sel = 0;
}

/// The resolvable step ids of the attack chain currently shown for the selection.
fn displayed_chain_steps(app: &App) -> Vec<String> {
    let Some(entry) = app.selected_entry() else {
        return Vec::new();
    };
    let chains = app.find_chains_for_entry(&entry.id);
    let Some(chain) = chains.get(app.current_chain_index) else {
        return Vec::new();
    };
    chain
        .steps
        .iter()
        .filter(|id| app.entry_index.contains_key(*id))
        .cloned()
        .collect()
}

/// Id of the chain currently displayed for the selection (for keeping the same
/// chain highlighted after the selection jumps to one of its steps).
fn current_displayed_chain_id(app: &App) -> Option<String> {
    let entry = app.selected_entry()?;
    let chains = app.find_chains_for_entry(&entry.id);
    chains.get(app.current_chain_index).map(|c| c.id.clone())
}

/// Focus the results/search pane.
fn focus_search_pane(app: &mut App) {
    app.search_focus = SearchPane::Results;
}

/// Focus the last-used right pane (Description or Chain).
fn focus_right_pane(app: &mut App) {
    app.search_focus = app.last_right_pane;
    if app.search_focus == SearchPane::Chain {
        init_chain_sel(app);
    }
}

/// Start the chain cursor on the current command within the chain (else the top).
fn init_chain_sel(app: &mut App) {
    let steps = displayed_chain_steps(app);
    let cur = app.selected_entry().map(|e| e.id.clone());
    app.chain_sel = cur
        .and_then(|id| steps.iter().position(|s| *s == id))
        .unwrap_or(0);
}

/// j / ↓ in nav mode — move *between panels* only (never scroll/step content):
/// results selection, or Description → Chain.
fn search_nav_down(app: &mut App) {
    match app.search_focus {
        SearchPane::Results => search_sel_down(app),
        SearchPane::Description => {
            app.search_focus = SearchPane::Chain;
            app.last_right_pane = SearchPane::Chain;
            init_chain_sel(app);
        }
        SearchPane::Chain => {}
    }
}

/// k / ↑ in nav mode — move *between panels* only: results selection, or
/// Chain → Description.
fn search_nav_up(app: &mut App) {
    match app.search_focus {
        SearchPane::Results => search_sel_up(app),
        SearchPane::Chain => {
            app.search_focus = SearchPane::Description;
            app.last_right_pane = SearchPane::Description;
        }
        SearchPane::Description => {}
    }
}

/// j / ↓ with nav OFF: scroll the focused pane (or move the results selection).
fn search_scroll_down(app: &mut App) {
    match app.search_focus {
        SearchPane::Results => search_sel_down(app),
        SearchPane::Description => app.desc_scroll = app.desc_scroll.saturating_add(1),
        SearchPane::Chain => chain_sel_move(app, 1),
    }
}

/// k / ↑ with nav OFF: scroll the focused pane (or move the results selection).
fn search_scroll_up(app: &mut App) {
    match app.search_focus {
        SearchPane::Results => search_sel_up(app),
        SearchPane::Description => app.desc_scroll = app.desc_scroll.saturating_sub(1),
        SearchPane::Chain => chain_sel_move(app, -1),
    }
}

/// Move the highlighted chain step, clamped to the chain's resolvable length.
fn chain_sel_move(app: &mut App, delta: i32) {
    let len = displayed_chain_steps(app).len();
    if len == 0 {
        return;
    }
    app.chain_sel = (app.chain_sel as i32 + delta).clamp(0, len as i32 - 1) as usize;
}

/// Present the highlighted chain step as the main selection ("present in search").
fn chain_present(app: &mut App) {
    let steps = displayed_chain_steps(app);
    let Some(target_id) = steps.get(app.chain_sel).cloned() else {
        return;
    };
    let Some(&idx) = app.entry_index.get(&target_id) else {
        return;
    };
    let Some(p) = app.results.iter().position(|&i| i == idx) else {
        return;
    };
    // Keep the same chain highlighted after the selection jumps to this step.
    let chain_id = current_displayed_chain_id(app);
    let new_ci = chain_id.and_then(|cid| {
        app.find_chains_for_entry(&target_id)
            .iter()
            .position(|c| c.id == cid)
    });
    app.list_state.select(Some(p));
    app.desc_scroll = 0;
    if let Some(ci) = new_ci {
        app.current_chain_index = ci;
    }
    app.search_focus = SearchPane::Results;
}

/// Open the given entry in `$EDITOR` via the cmd-maker template and save changes.
fn edit_command(app: &mut App, terminal: &mut DefaultTerminal, entry_index: usize) -> Result<()> {
    let Some(entry) = app.entries.get(entry_index).cloned() else {
        return Ok(());
    };
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, Show)?;
    let out = entry_to_template(&entry);
    fs::write(get_editor_temp_path(), out)?;
    let _ = open_editor(get_editor_temp_path());
    let updated = parse_template(&entry.id, app)?;
    app.entries[entry_index] = updated;
    app.dirty = true;
    let _ = fs::remove_file(get_editor_temp_path());
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    terminal.clear()?;
    Ok(())
}

/// Reset the Search-tab layout and view to defaults: pane sizes, focus, and
/// scroll positions. Stays in nav mode and leaves the query and file filter alone.
fn reset_search_view(app: &mut App) {
    app.main_split_pct = 60;
    app.right_split_pct = 60;
    app.search_focus = SearchPane::Results;
    app.last_right_pane = SearchPane::Description;
    app.desc_scroll = 0;
    app.chain_sel = 0;
}

/// Toggle the file-filter selection for the stem at `app.file_filters[idx]`
/// (idx >= 1; idx 0 = "All" clears the selection).
fn file_filter_toggle(app: &mut App, idx: usize) {
    if idx == 0 {
        app.file_selected.clear();
        return;
    }
    if let Some(name) = app.file_filters.get(idx).cloned() {
        if !app.file_selected.remove(&name) {
            app.file_selected.insert(name);
        }
    }
}

/// Modal key handling for the number-based file-filter popup (⌘F). Number keys
/// toggle files (0 = All / clear), Enter/Esc close.
fn handle_file_filter_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter => {
            app.file_filter_active = false;
            search(app, true);
        }
        // Also allow ⌘F to close the popup it opened.
        KeyCode::Char('f' | 'F')
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
        {
            app.file_filter_active = false;
            search(app, true);
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let idx = c.to_digit(10).unwrap() as usize;
            file_filter_toggle(app, idx);
            search(app, true);
        }
        _ => {}
    }
    Ok(false)
}

fn handle_key_event(app: &mut App, terminal: &mut DefaultTerminal) -> Result<bool> {
    if let Event::Key(key) = event::read()? {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        // The file-filter popup is modal on any tab (it is shared).
        if app.file_filter_active {
            return handle_file_filter_key(app, key);
        }
        // The Browse and Methodology tabs have their own key handling. Esc (quit)
        // and `[`/`]` (tab switching) still fall through to the shared match below.
        if key.code != KeyCode::Esc
            && !matches!(key.code, KeyCode::Char('[') | KeyCode::Char(']'))
        {
            if app.top_tab == 1 {
                return handle_browse_key(app, terminal, key);
            } else if app.top_tab == 2 {
                return handle_method_key(app, terminal, key);
            }
        }
        match key.code {
            // Esc first backs out of any list-nav focus; otherwise it cancels the
            // jump palette, or quits.
            KeyCode::Esc => {
                // Esc backs out one step at a time: nav mode → keep the focused
                // pane; then focus → back to the results pane; only then quit.
                if app.top_tab == 0 && app.search_nav {
                    app.search_nav = false;
                    return Ok(false);
                }
                if app.top_tab == 0 && app.search_focus != SearchPane::Results {
                    app.search_focus = SearchPane::Results;
                    app.desc_scroll = 0;
                    return Ok(false);
                }
                if app.top_tab == 1 && app.browse_nav {
                    app.browse_nav = false;
                    return Ok(false);
                }
                if app.top_tab == 2 && app.method_jump_active {
                    if app.method_jump_nav {
                        app.method_jump_nav = false;
                        return Ok(false);
                    }
                    app.method_jump_active = false;
                    app.method_query.clear();
                    app.method_jump_nav = false;
                    return Ok(false);
                }
                return Ok(true);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.query.clear();
                app.cursor_index = 0;
                search(app, true);
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry_index) = app.selected_entry_index() {
                    let removed_id = app.entries[entry_index].id.clone();
                    app.entries.remove(entry_index);

                    app.rebuild_entry_index();

                    for chain in &mut app.chains {
                        chain.steps.retain(|step_id| step_id != &removed_id);
                    }
                    app.chains.retain(|c| c.steps.len() >= 2);

                    app.dirty = true;
                    search(app, true);

                    if app.results.is_empty() {
                        app.list_state.select(None);
                    } else {
                        let current = app.list_state.selected().unwrap_or(0);
                        let new_sel = current.min(app.results.len() - 1);
                        app.list_state.select(Some(new_sel));
                    }
                }
                app.current_chain_index = 0;
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut entry = Entry::new();

                let mut rng = rand::rng();
                let id = format!("{:08x}", rng.random::<u32>());
                entry.id = id;

                // Disable raw mode and leave alternate screen
                disable_raw_mode()?;
                execute!(stdout(), LeaveAlternateScreen, Show)?;

                let out = entry_to_template(&entry);
                fs::write(get_editor_temp_path(), out)?;

                open_editor(get_editor_temp_path()).expect("Failed to execute editor");
                let updated_entry = parse_template(&entry.id, &app)?;

                fs::remove_file(get_editor_temp_path())?;

                app.entries.push(updated_entry);
                app.rebuild_entry_index();
                app.dirty = true;
                search(app, false);

                let new_entry_idx = app.entries.len() - 1;
                if let Some(filtered_pos) = app.results.iter().position(|&i| i == new_entry_idx) {
                    app.list_state.select(Some(filtered_pos));
                }
                app.current_chain_index = 0;

                // Re-enable raw mode and re-enter alternate screen
                enable_raw_mode()?;
                execute!(stdout(), EnterAlternateScreen, Hide)?;
                terminal.clear()?;
            }
            // Chain-edit toggle: Super+C (or Ctrl+C) — moved off 'n' so Super+N is
            // free for list-nav.
            KeyCode::Char('c')
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                if !app.is_chain_edit_mode {
                    if let Some(entry) = app.selected_entry() {
                        app.prev_selected_entry_id = entry.id.clone();
                    }
                }
                app.is_chain_edit_mode = !app.is_chain_edit_mode;
                app.query.clear();
                app.cursor_index = 0;
                search(app, false);
            }
            // Super+S (or Ctrl+S): toggle the selected command as a favorite.
            KeyCode::Char('s')
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                if let Some(idx) = app.selected_entry_index() {
                    app.entries[idx].favorite = !app.entries[idx].favorite;
                    app.dirty = true;
                    search(app, false);
                    // Follow the toggled entry to its new (reordered) position.
                    if let Some(pos) = app.results.iter().position(|&r| r == idx) {
                        app.list_state.select(Some(pos));
                    }
                }
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(i) = app.selected_entry_index() {
                    edit_command(app, terminal, i)?;
                }
            }
            // Plain `e` edits when a right pane is focused: the current command
            // (Description) or the highlighted chain step (Chain).
            KeyCode::Char('e')
                if app.search_focus == SearchPane::Description
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                if let Some(i) = app.selected_entry_index() {
                    edit_command(app, terminal, i)?;
                }
            }
            KeyCode::Char('e')
                if app.search_focus == SearchPane::Chain
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                let steps = displayed_chain_steps(app);
                if let Some(i) = steps.get(app.chain_sel).and_then(|id| app.entry_index.get(id)) {
                    let i = *i;
                    edit_command(app, terminal, i)?;
                }
            }
            KeyCode::Enter if app.is_chain_edit_mode => {
                let prev_id = app.prev_selected_entry_id.clone();

                if let Some(selected) = app.selected_entry() {
                    let selected_id = selected.id.clone();

                    if let Some(chain) = app.find_chain_for_entry_mut(&prev_id) {
                        if !chain.steps.contains(&selected_id) {
                            chain.steps.push(selected_id);
                        }
                    } else {
                        // Create new chain
                        let mut rng = rand::rng();
                        let chain_id = format!("{:08x}", rng.random::<u32>());

                        app.chains.push(Chain {
                            id: chain_id,
                            steps: vec![prev_id, selected_id],
                            name: String::from("new-chain"),
                            description: String::from("new-chain"),
                        });
                    }
                    app.dirty = true;
                }
                app.current_chain_index = 0;
                app.is_chain_edit_mode = false;
                app.query.clear();
                app.cursor_index = 0;
                search(app, true);
            }
            // Enter on a chain step presents that command as the main selection.
            KeyCode::Enter if app.search_focus == SearchPane::Chain => {
                chain_present(app);
            }
            KeyCode::Enter => {
                if let Some(entry) = app.selected_entry() {
                    copy_to_clipboard(&entry.cmd);
                }
                return Ok(true);
            }
            KeyCode::BackTab => {
                app.mode = match app.mode {
                    SearchMode::HEADING => SearchMode::CMD,
                    SearchMode::TITLE => SearchMode::HEADING,
                    SearchMode::ALL => SearchMode::TITLE,
                    SearchMode::CMD => SearchMode::ALL,
                };
                search(app, true);
            }
            KeyCode::Tab => {
                // While typing, Tab accepts a pending ghost-text completion;
                // otherwise it cycles the search mode.
                if !app.search_nav
                    && let Some(sfx) = complete_suffix(&app.vocab, last_token(&app.query))
                {
                    app.query.push_str(&sfx);
                    app.cursor_index = app.query.len();
                    search(app, true);
                } else {
                    app.mode = match app.mode {
                        SearchMode::CMD => SearchMode::HEADING,
                        SearchMode::HEADING => SearchMode::TITLE,
                        SearchMode::TITLE => SearchMode::ALL,
                        SearchMode::ALL => SearchMode::CMD,
                    };
                    search(app, true);
                }
            }
            // File filter: Ctrl+F or Super+F opens the numbered file-filter popup.
            KeyCode::Char('f' | 'F')
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                app.file_filter_active = true;
            }

            KeyCode::Char('[') => {
                app.top_tab = (app.top_tab + 2) % 3;
                app.search_nav = false;
                app.search_focus = SearchPane::Results;
                app.browse_nav = false;
                app.save_prev_method();
            }
            KeyCode::Char(']') => {
                app.top_tab = (app.top_tab + 1) % 3;
                app.search_nav = false;
                app.search_focus = SearchPane::Results;
                app.browse_nav = false;
                app.save_prev_method();
            }

            // Resize the Search panes with Shift + H/J/K/L (any mode): H/L
            // shrink/grow the results column, J/K grow/shrink the description
            // height. Search is case-insensitive, so stealing uppercase HJKL from
            // typing costs nothing, and these are delivered reliably (unlike
            // Cmd/Option+arrows, which many terminals intercept).
            KeyCode::Char('H') => {
                app.main_split_pct = app.main_split_pct.saturating_sub(5).max(20);
            }
            KeyCode::Char('L') => {
                app.main_split_pct = (app.main_split_pct + 5).min(80);
            }
            KeyCode::Char('J') => {
                app.right_split_pct = (app.right_split_pct + 5).min(80);
            }
            KeyCode::Char('K') => {
                app.right_split_pct = app.right_split_pct.saturating_sub(5).max(20);
            }

            // Arrows: nav mode moves focus/selection; nav off scrolls the focused
            // pane (or moves the results selection).
            KeyCode::Down => {
                if app.search_nav {
                    search_nav_down(app)
                } else {
                    search_scroll_down(app)
                }
            }
            KeyCode::Up => {
                if app.search_nav {
                    search_nav_up(app)
                } else {
                    search_scroll_up(app)
                }
            }
            // Super+N (or Ctrl+N) toggles nav mode (pane focus + list nav).
            KeyCode::Char('n')
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                if app.search_nav {
                    // Leave nav but keep the focused pane, so you can now scroll it.
                    app.search_nav = false;
                } else if !app.results.is_empty() {
                    app.search_nav = true;
                    if app.list_state.selected().is_none() {
                        app.list_state.select(Some(0));
                    }
                }
            }
            // Space in nav mode resets the layout/view to defaults.
            KeyCode::Char(' ') if app.search_nav => reset_search_view(app),
            // Nav mode: j/k move the selection or switch Description↔Chain; h/l
            // toggle between the results pane and the last-used right pane.
            KeyCode::Char('j')
                if app.search_nav
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                search_nav_down(app);
            }
            KeyCode::Char('k')
                if app.search_nav
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                search_nav_up(app);
            }
            KeyCode::Char('l')
                if app.search_nav
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                focus_right_pane(app);
            }
            KeyCode::Char('h')
                if app.search_nav
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                focus_search_pane(app);
            }
            // Nav off with a right pane focused: j/k scroll it. (On the results
            // pane, j/k fall through to the typing arm below.)
            KeyCode::Char('j')
                if !app.search_nav
                    && app.search_focus != SearchPane::Results
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                search_scroll_down(app);
            }
            KeyCode::Char('k')
                if !app.search_nav
                    && app.search_focus != SearchPane::Results
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                search_scroll_up(app);
            }
            // ←/→: nav mode toggles panes; while typing they move the query cursor.
            KeyCode::Left => {
                if app.search_nav {
                    focus_search_pane(app);
                } else {
                    app.cursor_index = app.cursor_index.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if app.search_nav {
                    focus_right_pane(app);
                } else if app.cursor_index < app.query.len() {
                    app.cursor_index += 1;
                }
            }
            KeyCode::Backspace => {
                if app.cursor_index > 0 {
                    app.cursor_index -= 1;
                    app.query.remove(app.cursor_index);
                    search(app, true);
                }
            }
            // Typing filters — only on the results pane (nav off), never while nav
            // has focus or a right pane is focused.
            KeyCode::Char(c)
                if !app.search_nav
                    && app.search_focus == SearchPane::Results
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                app.query.insert(app.cursor_index, c);
                app.cursor_index += 1;
                search(app, true);
            }
            _ => {}
        }
    }
    Ok(false)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(2), // top tabs (Search / Methodology)
        Constraint::Length(1), // spacer
        Constraint::Length(3), // search input (bordered = 3 rows)
        Constraint::Min(0),    // main content
        Constraint::Length(1), // footer
    ])
    .split(frame.area());

    render_top_tabs(frame, chunks[0], app);
    // chunks[1] is intentional whitespace

    // Browse & Methodology use the combined search-input + main area so their
    // filter box sits right under the tabs, aligned with the search bar.
    let combined = Rect {
        x: chunks[2].x,
        y: chunks[2].y,
        width: chunks[2].width,
        height: chunks[2].height + chunks[3].height,
    };
    match app.top_tab {
        0 => {
            render_search_input(frame, chunks[2], app);
            render_main(frame, chunks[3], app);
        }
        1 => render_folder_view(frame, combined, app),
        _ => render_method_view(frame, combined, app),
    }

    // The shared file-filter popup overlays whichever tab is active.
    if app.file_filter_active {
        render_file_filter(frame, chunks[3], app);
    }

    render_footer(frame, chunks[4], app);
}

/// A centered rectangle of the given size within `area` (clamped to fit).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The numbered file-filter popup (⌘F). Each source file has a number; pressing
/// it toggles that file in the multi-select. "0 All" clears the selection.
fn render_file_filter(frame: &mut Frame, area: Rect, app: &mut App) {
    let n = app.file_filters.len();
    let width = 40u16.min(area.width.saturating_sub(4)).max(24);
    let height = ((n as u16) + 4).clamp(6, area.height.saturating_sub(2).max(6));
    let popup = centered_rect(width, height, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_ACCENT))
        .padding(Padding::new(1, 1, 0, 0))
        .title(" Filter by file ")
        .title_alignment(Alignment::Center);
    let inner = block.inner(popup);

    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

    let mut lines: Vec<Line> = Vec::new();
    for (i, name) in app.file_filters.iter().enumerate() {
        let selected = if i == 0 {
            app.file_selected.is_empty()
        } else {
            app.file_selected.contains(name)
        };
        // "All" behaves like a radio (on when nothing is selected); the rest are
        // checkboxes.
        let mark = if i == 0 {
            if selected { "◉" } else { "○" }
        } else if selected {
            "☑"
        } else {
            "☐"
        };
        let label = if i == 0 {
            "All"
        } else {
            name.trim_end_matches("-CMDs")
        };
        let name_style = if selected {
            Style::default().fg(C_FG_BRIGHT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(C_DIM)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {i}  "), Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{mark} "),
                Style::default().fg(if selected { C_STAR } else { C_DIM }),
            ),
            Span::styled(label.to_string(), name_style),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), rows[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "number toggles · Enter/Esc close",
            Style::default().fg(C_DIM),
        )))
        .alignment(Alignment::Center),
        rows[1],
    );
}

/// A dim key-hint strip in the reserved bottom row, with the app name pinned right.
fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hint = match app.top_tab {
        0 => "⌘N nav · hjkl panels · ⇧HJKL resize · ⌘F filter · nav+Space reset · e edit",
        1 => "↑↓ move · ⇥ complete · h/l fold · Enter copy · ⌘F file · ⌘N nav",
        _ => "Tab section · ⌘F doc · hjkl move · Space check · / jump",
    };
    let brand = "F1nder";
    let left = format!("  {}   [ ] switch tab", hint);
    let pad = (area.width as usize)
        .saturating_sub(left.chars().count() + brand.chars().count() + 2);
    let line = Line::from(vec![
        Span::styled(left, Style::default().fg(C_DIM)),
        Span::raw(" ".repeat(pad)),
        Span::styled(
            brand,
            Style::default().fg(C_GUIDE).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// A single visible line of the Browse tree, after expand/collapse is applied.
struct BrowseRow {
    depth: usize,
    text: String,
    key: String,
    entry_index: Option<usize>,
    is_folder: bool,
    expanded: bool,
    count: usize,
}

/// Count matching leaf descendants (all leaves when `matches` is None).
fn count_matches(node: &TreeNode, matches: Option<&HashSet<usize>>) -> usize {
    match node.entry_index {
        Some(i) => match matches {
            Some(m) => usize::from(m.contains(&i)),
            None => 1,
        },
        None => node
            .children
            .iter()
            .map(|c| count_matches(c, matches))
            .sum(),
    }
}

/// The set of entry indices matching the Browse filter: every whitespace-separated
/// word must appear (case-insensitively) as a substring of the entry's title or
/// heading path. Predictable and precise — `sql injection` needs both words.
fn browse_match_set(app: &App) -> HashSet<usize> {
    let ql = app.browse_query.to_lowercase();
    let terms: Vec<&str> = ql.split_whitespace().collect();
    app.entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            if !app.entry_passes_file(e) {
                return None;
            }
            // Match against the field(s) selected by the browse filter mode.
            let hay = match app.browse_mode {
                SearchMode::TITLE => e.title.to_lowercase(),
                SearchMode::HEADING => e.heading_path.join(" ").to_lowercase(),
                SearchMode::CMD => e.cmd.to_lowercase(),
                SearchMode::ALL => {
                    format!("{} {} {}", e.title, e.heading_path.join(" "), e.cmd).to_lowercase()
                }
            };
            if terms.iter().all(|t| hay.contains(t)) {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

/// Walk the tree, emitting the visible Browse rows. A row appears only when all
/// its ancestor folders are expanded. A row's `key` is its real path (heading
/// segments joined by NUL) so expand/collapse state and `$EDITOR` renames stay
/// stable; the merge+hoist below only changes the displayed `text` and `depth`.
/// When `prune` is set, only matching leaves and their ancestors show;
/// `auto_expand` (a text filter) force-expands survivors unless explicitly
/// collapsed, otherwise the persistent `expanded` set is honored.
///
/// merge+hoist de-clutters the deep, narrow taxonomies in the data:
///   * hoist — a folder whose only surviving child is a command shows that
///     command in the folder's place (no one-item wrapper folders);
///   * merge — a pass-through folder (its only surviving child is another
///     folder) folds into a combined `A / B` breadcrumb row keyed on the deepest
///     node, so single-command chains collapse to just the command.
struct FlattenCtx<'a> {
    expanded: &'a HashSet<String>,
    collapsed: &'a HashSet<String>,
    prune: Option<&'a HashSet<usize>>,
    auto_expand: bool,
}

/// Does this node survive the active filter? (Always true when unfiltered.)
fn node_survives(node: &TreeNode, prune: Option<&HashSet<usize>>) -> bool {
    match prune {
        None => true,
        Some(m) => match node.entry_index {
            Some(i) => m.contains(&i),
            None => count_matches(node, Some(m)) > 0,
        },
    }
}

/// The children of `node` that survive the active filter, in order.
fn surviving_children<'a>(node: &'a TreeNode, prune: Option<&HashSet<usize>>) -> Vec<&'a TreeNode> {
    node.children
        .iter()
        .filter(|c| node_survives(c, prune))
        .collect()
}

/// A folder key is the parent's key with `name` appended (NUL-joined).
fn join_key(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}\u{0}{}", prefix, name)
    }
}

fn is_key_expanded(ctx: &FlattenCtx, key: &str) -> bool {
    if ctx.auto_expand {
        !ctx.collapsed.contains(key)
    } else {
        ctx.expanded.contains(key)
    }
}

fn flatten(
    roots: &[TreeNode],
    expanded: &HashSet<String>,
    collapsed: &HashSet<String>,
    prune: Option<&HashSet<usize>>,
    auto_expand: bool,
    out: &mut Vec<BrowseRow>,
) {
    let ctx = FlattenCtx {
        expanded,
        collapsed,
        prune,
        auto_expand,
    };
    for root in roots {
        if node_survives(root, prune) {
            emit_node(root, 0, "", &ctx, out);
        }
    }
}

/// Emit one node (and its visible descendants). `depth` is the *display* depth;
/// `prefix` is the parent's real NUL-joined key.
fn emit_node(node: &TreeNode, depth: usize, prefix: &str, ctx: &FlattenCtx, out: &mut Vec<BrowseRow>) {
    // Command leaf — emitted as-is.
    if node.entry_index.is_some() {
        out.push(BrowseRow {
            depth,
            text: node.text.clone(),
            key: join_key(prefix, &node.text),
            entry_index: node.entry_index,
            is_folder: false,
            expanded: false,
            count: 1,
        });
        return;
    }

    // Root (file-stem) folder: never hoist/merge; show a friendly label.
    if depth == 0 {
        let key = node.text.clone();
        let is_expanded = is_key_expanded(ctx, &key);
        out.push(BrowseRow {
            depth,
            text: root_label(&node.text),
            key: key.clone(),
            entry_index: None,
            is_folder: true,
            expanded: is_expanded,
            count: count_matches(node, ctx.prune),
        });
        if is_expanded {
            for child in surviving_children(node, ctx.prune) {
                emit_node(child, depth + 1, &key, ctx, out);
            }
        }
        return;
    }

    // Deeper folder: fold a single-sub-folder pass-through chain into `A / B`.
    let mut label = node.text.clone();
    let mut key = join_key(prefix, &node.text);
    let mut cur = node;
    loop {
        let only = {
            let surv = surviving_children(cur, ctx.prune);
            if surv.len() == 1 && surv[0].entry_index.is_none() {
                Some(surv[0])
            } else {
                None
            }
        };
        match only {
            Some(child) => {
                label = format!("{} / {}", label, child.text);
                key = join_key(&key, &child.text);
                cur = child;
            }
            None => break,
        }
    }

    let surv = surviving_children(cur, ctx.prune);
    // Hoist: a folder wrapping a single command shows the command itself.
    if surv.len() == 1 && surv[0].entry_index.is_some() {
        let leaf = surv[0];
        out.push(BrowseRow {
            depth,
            text: leaf.text.clone(),
            key: join_key(&key, &leaf.text),
            entry_index: leaf.entry_index,
            is_folder: false,
            expanded: false,
            count: 1,
        });
        return;
    }

    // Branching folder — emit the (possibly merged) folder row, then recurse.
    let is_expanded = is_key_expanded(ctx, &key);
    out.push(BrowseRow {
        depth,
        text: label,
        key: key.clone(),
        entry_index: None,
        is_folder: true,
        expanded: is_expanded,
        count: count_matches(cur, ctx.prune),
    });
    if is_expanded {
        for child in surv {
            emit_node(child, depth + 1, &key, ctx, out);
        }
    }
}

/// The Browse tree flattened to its currently-visible rows, honoring the filters.
fn browse_rows(app: &App) -> Vec<BrowseRow> {
    let roots = build_tree(&app.entries);

    // A text filter auto-expands to reveal matches; a file-only filter just
    // prunes to that file's subtree and stays foldable.
    let has_text = !app.browse_query.trim().is_empty();
    let has_file = !app.file_selected.is_empty();
    let prune: Option<HashSet<usize>> = if has_text || has_file {
        Some(browse_match_set(app))
    } else {
        None
    };

    let mut out = Vec::new();
    flatten(
        &roots,
        &app.expanded,
        &app.browse_collapsed,
        prune.as_ref(),
        has_text,
        &mut out,
    );
    out
}

fn browse_selected_entry_index(app: &App) -> Option<usize> {
    let sel = app.browse_state.selected()?;
    browse_rows(app).get(sel).and_then(|r| r.entry_index)
}

/// Set a folder's expanded state, writing to the store that matches the current
/// mode: the persistent `expanded` set normally, or the transient
/// `browse_collapsed` set (inverted meaning) while filtering.
fn set_folder_expanded(app: &mut App, key: &str, expanded: bool, filtering: bool) {
    if filtering {
        if expanded {
            app.browse_collapsed.remove(key);
        } else {
            app.browse_collapsed.insert(key.to_string());
        }
    } else if expanded {
        app.expanded.insert(key.to_string());
    } else {
        app.expanded.remove(key);
    }
}

/// Rename a heading folder across every entry beneath its path. `key` is the
/// folder's tree key: the source-file stem then heading components, joined by NUL.
fn rename_heading(app: &mut App, key: &str, new_name: &str) {
    let comps: Vec<&str> = key.split('\u{0}').collect();
    if comps.len() < 2 {
        return; // file-level node or invalid — nothing to rename
    }
    let file = comps[0];
    let prefix = &comps[1..]; // matches entry.heading_path[..prefix.len()]
    let idx = prefix.len() - 1;

    for e in &mut app.entries {
        if entry_stem(e) != file {
            continue;
        }
        if e.heading_path.len() >= prefix.len()
            && e.heading_path[..prefix.len()]
                .iter()
                .zip(prefix.iter())
                .all(|(a, b)| a == b)
        {
            e.heading_path[idx] = new_name.to_string();
        }
    }
}

/// Move the Browse selection down (clamped).
fn browse_sel_down(app: &mut App) {
    let len = browse_rows(app).len();
    if len == 0 {
        return;
    }
    let i = app
        .browse_state
        .selected()
        .map(|i| (i + 1).min(len - 1))
        .unwrap_or(0);
    app.browse_state.select(Some(i));
}

/// Move the Browse selection up (clamped to the first row).
fn browse_sel_up(app: &mut App) {
    if let Some(i) = app.browse_state.selected() {
        app.browse_state.select(Some(i.saturating_sub(1)));
    } else if !browse_rows(app).is_empty() {
        app.browse_state.select(Some(0));
    }
}

/// Toggle the selected folder's expansion (no-op on a command leaf) — the `l`
/// / Right action.
fn browse_toggle_folder(app: &mut App) {
    let filtering = !app.browse_query.trim().is_empty();
    let rows = browse_rows(app);
    if let Some(row) = app.browse_state.selected().and_then(|s| rows.get(s)) {
        if row.is_folder {
            let key = row.key.clone();
            let expanded_now = row.expanded;
            set_folder_expanded(app, &key, !expanded_now, filtering);
        }
    }
}

/// Collapse an expanded folder, else jump to the parent row — the `h` / Left action.
fn browse_collapse_or_parent(app: &mut App) {
    let filtering = !app.browse_query.trim().is_empty();
    let rows = browse_rows(app);
    if let Some(sel) = app.browse_state.selected() {
        if let Some(row) = rows.get(sel) {
            if row.is_folder && row.expanded {
                let key = row.key.clone();
                set_folder_expanded(app, &key, false, filtering);
            } else {
                let depth = row.depth;
                for j in (0..sel).rev() {
                    if rows[j].depth < depth {
                        app.browse_state.select(Some(j));
                        break;
                    }
                }
            }
        }
    }
}

fn handle_browse_key(app: &mut App, terminal: &mut DefaultTerminal, key: KeyEvent) -> Result<bool> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let plain = !ctrl && !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER);
    match key.code {
        // Arrows always move the selection (both typing and nav modes).
        KeyCode::Down => browse_sel_down(app),
        KeyCode::Up => browse_sel_up(app),
        // Super+N (or Ctrl+N) toggles list-nav (j/k navigate, typing off).
        KeyCode::Char('n')
            if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
        {
            if app.browse_nav {
                app.browse_nav = false;
            } else if !browse_rows(app).is_empty() {
                app.browse_nav = true;
                if app.browse_state.selected().is_none() {
                    app.browse_state.select(Some(0));
                }
            }
        }
        KeyCode::Char('j') if app.browse_nav && plain => browse_sel_down(app),
        KeyCode::Char('k') if app.browse_nav && plain => browse_sel_up(app),
        // h/l fold and unfold folders in nav mode (mirror Left/Right).
        KeyCode::Char('l') if app.browse_nav && plain => browse_toggle_folder(app),
        KeyCode::Char('h') if app.browse_nav && plain => browse_collapse_or_parent(app),
        // Tab accepts a pending ghost-text completion while typing; with no
        // completion pending it falls through to the mode-cycle Tab arm below.
        KeyCode::Tab
            if !app.browse_nav
                && complete_suffix(&app.vocab, last_token(&app.browse_query)).is_some() =>
        {
            if let Some(sfx) = complete_suffix(&app.vocab, last_token(&app.browse_query)) {
                app.browse_query.push_str(&sfx);
                app.browse_collapsed.clear();
                app.browse_state.select(Some(0));
            }
        }
        // Enter/Right: toggle a folder, or copy + exit on a command.
        KeyCode::Enter | KeyCode::Right => {
            let filtering = !app.browse_query.trim().is_empty();
            let rows = browse_rows(app);
            if let Some(row) = app.browse_state.selected().and_then(|s| rows.get(s)) {
                if row.is_folder {
                    let key = row.key.clone();
                    let expanded_now = row.expanded;
                    set_folder_expanded(app, &key, !expanded_now, filtering);
                } else if key.code == KeyCode::Enter {
                    if let Some(idx) = row.entry_index {
                        copy_to_clipboard(&app.entries[idx].cmd);
                    }
                    return Ok(true);
                }
            }
        }
        // Left: collapse an expanded folder, else jump to the parent row.
        KeyCode::Left => browse_collapse_or_parent(app),
        // Edit the selected command, reusing the existing editor template flow.
        // Ctrl+E: edit the selected command, or rename the selected heading folder.
        KeyCode::Char('e') if ctrl => {
            let rows = browse_rows(app);
            let selected = app
                .browse_state
                .selected()
                .and_then(|s| rows.get(s))
                .map(|r| (r.is_folder, r.depth, r.key.clone(), r.text.clone(), r.entry_index));

            if let Some((is_folder, depth, key, text, entry_index)) = selected {
                if is_folder {
                    // depth 0 is the source-file node — not an editable heading.
                    if depth >= 1 {
                        disable_raw_mode()?;
                        execute!(stdout(), LeaveAlternateScreen, Show)?;

                        fs::write(get_editor_temp_path(), format!("{}\n", text))?;
                        let _ = open_editor(get_editor_temp_path());
                        let new_name = fs::read_to_string(get_editor_temp_path())?
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        fs::remove_file(get_editor_temp_path())?;

                        enable_raw_mode()?;
                        execute!(stdout(), EnterAlternateScreen, Hide)?;
                        terminal.clear()?;

                        if !new_name.is_empty() && new_name != text {
                            rename_heading(app, &key, &new_name);
                            app.dirty = true;
                        }
                    }
                } else if let Some(idx) = entry_index {
                    let entry = app.entries[idx].clone();

                    disable_raw_mode()?;
                    execute!(stdout(), LeaveAlternateScreen, Show)?;

                    fs::write(get_editor_temp_path(), entry_to_template(&entry))?;
                    let _ = open_editor(get_editor_temp_path());
                    let updated_entry = parse_template(&entry.id, app)?;
                    app.entries[idx] = updated_entry;
                    app.dirty = true;
                    fs::remove_file(get_editor_temp_path())?;

                    enable_raw_mode()?;
                    execute!(stdout(), EnterAlternateScreen, Hide)?;
                    terminal.clear()?;
                }
            }
        }
        KeyCode::Char('d') if ctrl => {
            if let Some(idx) = browse_selected_entry_index(app) {
                let removed_id = app.entries[idx].id.clone();
                app.entries.remove(idx);
                app.rebuild_entry_index();

                for chain in &mut app.chains {
                    chain.steps.retain(|step_id| step_id != &removed_id);
                }
                app.chains.retain(|c| c.steps.len() >= 2);
                app.dirty = true;

                let len = browse_rows(app).len();
                if len == 0 {
                    app.browse_state.select(None);
                } else {
                    let cur = app.browse_state.selected().unwrap_or(0);
                    app.browse_state.select(Some(cur.min(len - 1)));
                }
            }
        }
        KeyCode::Char('a') if ctrl => {
            let mut entry = Entry::new();
            let mut rng = rand::rng();
            entry.id = format!("{:08x}", rng.random::<u32>());

            disable_raw_mode()?;
            execute!(stdout(), LeaveAlternateScreen, Show)?;

            fs::write(get_editor_temp_path(), entry_to_template(&entry))?;
            open_editor(get_editor_temp_path()).expect("Failed to execute editor");
            let updated_entry = parse_template(&entry.id, app)?;
            fs::remove_file(get_editor_temp_path())?;

            app.entries.push(updated_entry);
            app.rebuild_entry_index();
            app.dirty = true;

            enable_raw_mode()?;
            execute!(stdout(), EnterAlternateScreen, Hide)?;
            terminal.clear()?;
        }
        // File filter: Ctrl+F or Super+F opens the numbered file-filter popup.
        KeyCode::Char('f' | 'F')
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
        {
            app.file_filter_active = true;
        }
        // Cycle the filter field mode (TITLE / HEADING / CMD / ALL), like the
        // regular search's Tab. Shift+Tab cycles in reverse.
        KeyCode::Tab => {
            app.browse_mode = match app.browse_mode {
                SearchMode::CMD => SearchMode::HEADING,
                SearchMode::HEADING => SearchMode::TITLE,
                SearchMode::TITLE => SearchMode::ALL,
                SearchMode::ALL => SearchMode::CMD,
            };
            app.browse_collapsed.clear();
            app.browse_state.select(Some(0));
        }
        KeyCode::BackTab => {
            app.browse_mode = match app.browse_mode {
                SearchMode::HEADING => SearchMode::CMD,
                SearchMode::TITLE => SearchMode::HEADING,
                SearchMode::ALL => SearchMode::TITLE,
                SearchMode::CMD => SearchMode::ALL,
            };
            app.browse_collapsed.clear();
            app.browse_state.select(Some(0));
        }
        // Incremental filter typing. Editing the filter resets transient
        // collapse state so new matches are revealed.
        KeyCode::Char('u') if ctrl => {
            app.browse_query.clear();
            app.browse_collapsed.clear();
            app.browse_state.select(Some(0));
        }
        KeyCode::Backspace if !app.browse_nav => {
            app.browse_query.pop();
            app.browse_collapsed.clear();
            app.browse_state.select(Some(0));
        }
        // Typing filters — but not while list-nav has focus.
        KeyCode::Char(c) if !app.browse_nav && plain => {
            app.browse_query.push(c);
            app.browse_collapsed.clear();
            app.browse_state.select(Some(0));
        }
        _ => {}
    }
    Ok(false)
}
fn build_tree(entries: &[Entry]) -> Vec<TreeNode> {
    let mut roots: Vec<TreeNode> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        let mut level = &mut roots;

        let source_filename = entry
            .source_file
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let mut path = vec![source_filename];
        path.extend(entry.heading_path.iter().cloned());

        for name in &path {
            let index = match level.iter().position(|node| node.text == *name) {
                Some(i) => i,
                None => {
                    level.push(TreeNode::folder(name.clone()));
                    level.len() - 1
                }
            };
            level = &mut level[index].children;
        }
        level.push(TreeNode::leaf(entry.title.clone(), i));
    }
    sort_tree(&mut roots, None);
    roots
}

/// Stable-sort every folder level into methodology (chronological testing) order.
/// `stem` is the source-file stem governing the current level's order map (None at
/// the file-stem root). Folders not in the map keep insertion order (they sort
/// after mapped folders); leaves always trail folders, keeping their own order.
fn sort_tree(nodes: &mut [TreeNode], stem: Option<&str>) {
    match stem {
        None => nodes.sort_by_key(|n| root_rank(&n.text)),
        Some(st) => nodes.sort_by_key(|n| match n.entry_index {
            Some(_) => i32::MAX,
            None => folder_rank(st, &n.text),
        }),
    }
    for n in nodes.iter_mut() {
        match stem {
            None => {
                let child_stem = n.text.clone();
                sort_tree(&mut n.children, Some(&child_stem));
            }
            Some(st) => sort_tree(&mut n.children, Some(st)),
        }
    }
}

/// Order of the top-level command sets (file stems): recon-heavy first, then AD,
/// web, cloud, wireless, misc.
fn root_rank(stem: &str) -> i32 {
    const ORDER: &[&str] = &[
        "CPTS-CMDs",
        "CAPE-CMDs",
        "CWES-CMDs",
        "CWEE-CMDs",
        "OAOTC-CMDs",
        "CWPE-CMDs",
        "DEPTH-CMDs",
    ];
    ORDER
        .iter()
        .position(|s| *s == stem)
        .map(|i| i as i32)
        .unwrap_or(i32::MAX)
}

/// Friendly display label for a file-stem root folder (data/key unchanged).
fn root_label(stem: &str) -> String {
    match stem {
        "CAPE-CMDs" => "Active Directory  (CAPE)",
        "CPTS-CMDs" => "Penetration Testing  (CPTS)",
        "CWEE-CMDs" => "Web — Expert  (CWEE)",
        "CWES-CMDs" => "Web — Standard  (CWES)",
        "CWPE-CMDs" => "Wireless  (CWPE)",
        "OAOTC-CMDs" => "Azure / Cloud  (OAOTC)",
        "DEPTH-CMDs" => "Depth  (DEPTH)",
        other => other,
    }
    .to_string()
}

/// Rank of a heading folder within its file, from the curated methodology order.
/// Unlisted names return `i32::MAX` (kept in insertion order, after ranked ones).
/// The same map is applied at every depth, so names at any level order correctly.
fn folder_rank(stem: &str, name: &str) -> i32 {
    let order: &[&str] = match stem {
        "CAPE-CMDs" => CAPE_ORDER,
        "CPTS-CMDs" => CPTS_ORDER,
        "CWEE-CMDs" => CWEE_ORDER,
        "CWES-CMDs" => CWES_ORDER,
        "CWPE-CMDs" => CWPE_ORDER,
        "OAOTC-CMDs" => OAOTC_ORDER,
        "DEPTH-CMDs" => DEPTH_ORDER,
        _ => &[],
    };
    order
        .iter()
        .position(|s| *s == name)
        .map(|i| i as i32)
        .unwrap_or(i32::MAX)
}

// Curated folder orders per command set, aligned to JSONs/methodology/*.md.
// Names not present in a file are harmless; unmatched folders fall to the end.

/// CAPE ← ad.md phase order (Recon → Enum → Foothold → ADCS → Relay → DACL →
/// Delegation → MSSQL → Exchange → SCCM → Cred Theft → Lateral → Trusts → Post).
const CAPE_ORDER: &[&str] = &[
    "Getting Started",
    "Setting Up",
    "Active Directory",
    "Initial Access",
    "Network Scanning",
    "AD Enumeration",
    "NetExec (nxc)",
    "nxc",
    "Rusthound-CE",
    "Remote Services",
    "Remote Management Tools",
    "Spoofing",
    "ADCS Attacks",
    "NTLM Relay Attacks",
    "Advanced NTLM Relay Attacks",
    "DACL Attacks",
    "Roasting Attacks",
    "Unconstrained Delegation",
    "Constrained Delegation",
    "Ticket Abuse",
    "Kerberos Authentication",
    "MSSQL Server",
    "Abusing SQL Server Links",
    "Microsoft Exchange",
    "SCCM",
    "Group Policy",
    "Attribute Modification",
    "Credential Theft",
    "Command Execution",
    "Execute Commands",
    "Getting a Remote Shell",
    "Lateral Movement",
    "Tunneling & Pivoting & Lateral Movement",
    "Server Message Block (SMB)",
    "Inter Forest Attacks",
    "Cross Forest Attacks",
    "Privilege Escalation",
    "Post Exploitation",
    "Establishing Persistence",
    "Antivirus Evasion",
    "C2 Frameworks",
    "Sliver C2",
    "Shell Utilities",
    "String Manipulation",
    "Miscellaneous",
];

/// OAOTC ← azure.md. Everything lives under the single `Azure` h0, so the h1
/// subsection names are ordered here too (applied at every depth).
const OAOTC_ORDER: &[&str] = &[
    "Initial Enumeration",
    "Cloud Enumeration",
    "Azure",
    "Authentication",
    "OAuth",
    "AD Enumeration",
    "Resource Enumeration",
    "Post-Authentication",
    "Managed Identity",
    "Credential Theft",
    "Blob Storage",
    "Post Exploitation",
    "Persistence",
];

/// CWEE ← web.md (Enum → Server-Side → Client-Side → Transport → AuthN/Z).
const CWEE_ORDER: &[&str] = &[
    "Getting Started",
    "Intro To Whitebox Pentesting",
    "Web Enumeration",
    "SQL Injection",
    "LDAP Injection",
    "XPath Injection",
    "NoSQL Injection",
    "Exploiting PHP Deserialization",
    "HTML Injection in PDF Generators",
    "DNS Rebinding",
    "XSS",
    "Web Cache Poisoning",
    "CRLF Injection",
    "CSRF Exploitation",
    "Prototype Pollution",
    "HTTP Request Smuggling",
    "Host Header Attacks",
    "JWTs",
    "OAuth",
    "SAML",
    "Session Puzzling",
];

/// CWES ← web.md (Enum → Server-Side → Client-Side → AuthN/Z → API).
const CWES_ORDER: &[&str] = &[
    "Web Enumeration",
    "Web Fuzzing",
    "WordPress",
    "SQL Injection",
    "NoSQL Injection",
    "XXE Injection",
    "XSLT Injection",
    "Server-Side Includes",
    "SSTI",
    "SSRF",
    "Path Traversal",
    "File Upload Attacks",
    "XSS",
    "Obfuscation & Deobfuscation",
    "Session Attacks",
    "Authentication Attacks",
    "Password Attacks",
    "Host Header Attacks",
    "API Attacks",
    "Attacking GraphQL",
];

/// CWPE (wireless): setup → fundamentals → WEP → WPA personal → WPA enterprise → WPS.
const CWPE_ORDER: &[&str] = &[
    "Getting Started",
    "Setup",
    "Interfaces and Modes",
    "802.11 Fundamentals",
    "Aircrack-ng Essentials",
    "Basic Control Bypasses",
    "WEP Encryption",
    "WEP Attacks",
    "Cracking WEP",
    "WPA/WPA2 Personal Networks",
    "Cracking WPA",
    "WPA/WPA2 Enterprise Networks",
    "WPA Enterprise Attacks",
    "Certificates",
    "Certificate Configuration",
    "WPS Attacks",
];

/// CPTS is multi-domain: bucket External/recon → services & web → web attacks →
/// initial access/creds → AD → post-exploitation (best-effort).
const CPTS_ORDER: &[&str] = &[
    "Getting Started",
    "WHOIS",
    "OSINT",
    "Infrastructure Enumeration",
    "Network Scanning",
    "Enumeration Tools",
    "Web Enumeration",
    "Web Fuzzing",
    "Initial Enumeration",
    "Host Enumeration",
    "Linux Enumeration",
    "Attacking Common Services",
    "Attacking Common Applications",
    "WordPress",
    "Joomla",
    "Drupal",
    "Tomcat",
    "ColdFusion",
    "GitLab",
    "Jenkins",
    "Splunk",
    "PRTG",
    "OS Ticket",
    "IIS Tilde",
    "SQL Injection",
    "Database Enumeration",
    "UNION Injection",
    "ERROR-Based Injection",
    "Blind Injection — Boolean Based",
    "Blind Injection — Error Based",
    "Blind Injection — Error Based (Oracle)",
    "Blind Injection — Time Delays",
    "Reading Files",
    "Writing Files",
    "DNS Lookups & Data Exfiltration",
    "Command Injection",
    "File Inclusion",
    "File Upload Attacks",
    "XSS",
    "XXE Injection",
    "IDOR",
    "HTTP Attacks",
    "WAF Bypass",
    "Initial Access",
    "Password Attacks",
    "Attacking The OS",
    "Living Off the Land",
    "Shells",
    "Metasploit",
    "Meterpreter (It’s its own monster)",
    "Msf Handler",
    "MSFVenom Payloads",
    "Plugins & Mixins",
    "AD Enumeration",
    "AD Attacks",
    "Kerberoasting",
    "LDAP",
    "Trust Attacks",
    "Privilege Escalation",
    "Credential Theft",
    "Lateral Movement",
    "Pivoting and Tunneling",
    "Pivoting",
    "File Transfers",
    "Post Exploitation",
];

/// DEPTH (misc/internal): recon → creds → tooling.
const DEPTH_ORDER: &[&str] = &[
    "Initial Enumeration",
    "Password Attacks",
    "Armory",
    "VirtualBox",
];

/// The last whitespace-delimited token of `s` — the word autocomplete extends.
fn last_token(s: &str) -> &str {
    s.rsplit(char::is_whitespace).next().unwrap_or("")
}

/// The dim ghost-text completion for `token`, drawn from a frequency-ranked word
/// list. Returns the suffix to append (never the whole word), or None when the
/// token is too short or no longer word starts with it.
fn complete_suffix(words: &[String], token: &str) -> Option<String> {
    if token.len() < 2 {
        return None;
    }
    let tl = token.to_lowercase();
    words
        .iter()
        .find(|w| w.len() > tl.len() && w.starts_with(&tl))
        .map(|w| w[tl.len()..].to_string())
}

/// Frequency-ranked words from the methodology jump targets, for the jump
/// palette's inline autocomplete.
fn jump_vocab(app: &App) -> Vec<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, u32> = HashMap::new();
    for t in jump_targets(app) {
        for w in t.label.split(|c: char| !c.is_alphanumeric()) {
            if w.len() >= 2 {
                *counts.entry(w.to_lowercase()).or_insert(0) += 1;
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
fn render_browse_filter(frame: &mut Frame, area: Rect, app: &App) {
    let mut title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!(" {} ", app.browse_mode),
            Style::default()
                .bg(C_CHIP_BG)
                .fg(C_ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!(" {} ", IC_FOLDER), Style::default().fg(C_DIM)),
        Span::styled(
            format!("{} ", app.file_filter_label()),
            Style::default().fg(C_TITLE),
        ),
        Span::raw(" "),
    ]);
    // NAV badge when the tree has focus (j/k navigate, typing is off).
    if app.browse_nav {
        title.spans.push(Span::styled(
            " NAV ",
            Style::default().bg(C_CHECK).fg(C_ACCENT_BG).add_modifier(Modifier::BOLD),
        ));
        title.spans.push(Span::raw(" "));
    }
    let block = Block::default()
        .title_top(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.browse_nav { C_ACCENT } else { C_BORDER }));

    let line = if app.browse_query.is_empty() {
        Line::from(vec![Span::styled(
            "  type to filter…",
            Style::default().fg(C_DIM),
        )])
    } else {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(app.browse_query.as_str(), Style::default().fg(C_FG_BRIGHT)),
        ];
        // Inline ghost-text completion (Browse filter is append-only).
        if !app.browse_nav {
            if let Some(sfx) = complete_suffix(&app.vocab, last_token(&app.browse_query)) {
                spans.push(Span::styled(sfx, Style::default().fg(C_GUIDE)));
            }
        }
        Line::from(spans)
    };

    if !app.browse_nav {
        frame.set_cursor_position(Position::new(
            area.x + 1 + 2 + app.browse_query.len() as u16,
            area.y + 1,
        ));
    }
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_folder_view(frame: &mut Frame, area: Rect, app: &mut App) {
    let vparts = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(area);
    render_browse_filter(frame, vparts[0], app);

    let cols = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(vparts[1]);

    let rows = browse_rows(app);

    // Keep the selection in range as rows appear/disappear on expand/collapse.
    if rows.is_empty() {
        app.browse_state.select(None);
    } else {
        let sel = app.browse_state.selected().unwrap_or(0).min(rows.len() - 1);
        app.browse_state.select(Some(sel));
    }

    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| {
            let indent = "  ".repeat(r.depth);
            if r.is_folder {
                let marker = if r.expanded { "▾" } else { "▸" };
                let folder = if r.expanded { IC_FOLDER_OPEN } else { IC_FOLDER };
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(format!("{} ", marker), Style::default().fg(C_GUIDE)),
                    Span::styled(format!("{} ", folder), Style::default().fg(C_ACCENT)),
                    Span::styled(
                        r.text.clone(),
                        Style::default()
                            .fg(C_FG_BRIGHT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {}", r.count), Style::default().fg(C_DIM)),
                ]))
            } else {
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(format!("  {} ", IC_CMD), Style::default().fg(C_DIM)),
                    Span::styled(r.text.clone(), Style::default().fg(C_TITLE)),
                ]))
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 0))
        .title_bottom(Line::from(vec![Span::styled(
            format!(" {} entries ", app.entries.len()),
            Style::default().fg(C_DIM),
        )]))
        .border_style(Style::default().fg(C_BORDER));

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(C_HIGHLIGHT_BG)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, cols[0], &mut app.browse_state);

    let entry = app
        .browse_state
        .selected()
        .and_then(|i| rows.get(i))
        .and_then(|r| r.entry_index)
        .and_then(|idx| app.entries.get(idx));

    render_browse_detail(frame, cols[1], entry);
}

fn render_browse_detail(frame: &mut Frame, area: Rect, entry: Option<&Entry>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER))
        .padding(Padding::new(2, 2, 1, 0))
        .title(" COMMAND ")
        .title_alignment(Alignment::Center);

    let Some(e) = entry else {
        let p = Paragraph::new(vec![Line::from(""), Line::from("Select a command")])
            .style(Style::default().fg(C_DIM))
            .alignment(Alignment::Center)
            .block(block);
        frame.render_widget(p, area);
        return;
    };

    let mut lines = vec![
        Line::from(Span::styled(
            e.title.clone(),
            Style::default()
                .fg(C_FG_BRIGHT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            e.heading_path.join(" › "),
            Style::default().fg(C_DIM),
        )),
        Line::from(""),
    ];
    for l in e.cmd.lines() {
        lines.push(Line::from(Span::styled(
            format!("$ {}", l),
            Style::default().fg(C_ACCENT),
        )));
    }
    lines.push(Line::from(""));
    for l in e.description.lines() {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(C_DESC),
        )));
    }

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    frame.render_widget(p, area);
}
// ---------------------------------------------------------------------------
// Methodology tab: a collapsible checklist walked during an engagement, parsed
// from JSONs/methodology.md. Mirrors the Browse-tree machinery.
// ---------------------------------------------------------------------------

const C_CHECK: Color = Color::Rgb(112, 222, 152);

/// A flattened row of the detail checklist tree for the selected attack card.
struct MethodRow {
    depth: usize,
    key: String,
    title: String,
    kind: MethodKind,
    is_heading: bool,
    /// Has at least one visible child (so it can fold / shows a ▾▸ marker).
    has_children: bool,
    /// A directly-checkable leaf check (no check descendant).
    is_leaf: bool,
    expanded: bool,
    /// Last among its visible siblings (drives └─ vs ├─).
    is_last: bool,
    /// For each ancestor level ≥1: does that ancestor have a following sibling
    /// (draw a continuing `│`) — used to render the tree connector guides.
    guides: Vec<bool>,
    done: usize,
    total: usize,
    checked: bool,
    src_line: usize,
}

fn roots_counts(roots: &[&MethodNode]) -> (usize, usize) {
    roots.iter().fold((0, 0), |(d, t), r| {
        let (rd, rt) = r.leaf_counts();
        (d + rd, t + rt)
    })
}

fn method_overall(app: &App) -> (usize, usize) {
    app.method_tree().iter().fold((0, 0), |(d, t), n| {
        let (nd, nt) = n.leaf_counts();
        (d + nd, t + nt)
    })
}

/// The attack cards for a section: an optional leading "General" card holding
/// loose checks that appear before the first `##`, then one card per `##` group.
fn card_roots(section: &MethodNode) -> Vec<(String, Vec<&MethodNode>)> {
    let mut cards: Vec<(String, Vec<&MethodNode>)> = Vec::new();
    let lead: Vec<&MethodNode> = section
        .children
        .iter()
        .take_while(|c| !c.is_heading())
        .collect();
    if !lead.is_empty() {
        cards.push(("General".to_string(), lead));
    }
    for c in section.children.iter().filter(|c| c.is_heading()) {
        cards.push((c.title.clone(), c.children.iter().collect()));
    }
    cards
}

#[allow(clippy::too_many_arguments)]
fn push_method_row(
    node: &MethodNode,
    key: String,
    depth: usize,
    guides: &[bool],
    is_last: bool,
    collapsed: &HashSet<String>,
    show_comments: bool,
    out: &mut Vec<MethodRow>,
) {
    // Optionally hide floating comments (Note rows); checks/bullets stay.
    if node.kind == MethodKind::Note && !show_comments {
        return;
    }
    let is_heading = node.is_heading();
    // Visible children (with original indices, so keys stay stable when comments
    // are toggled). A note-only parent with comments hidden reads as a leaf.
    let visible: Vec<(usize, &MethodNode)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| show_comments || c.kind != MethodKind::Note)
        .collect();
    let has_children = !visible.is_empty();
    let expanded = has_children && !collapsed.contains(&key);
    let (done, total) = node.leaf_counts();
    let checked = match node.kind {
        MethodKind::Check => {
            if node.is_leaf_check() {
                node.checked
            } else {
                node.all_leaves_checked()
            }
        }
        _ => false,
    };
    out.push(MethodRow {
        depth,
        key: key.clone(),
        title: node.title.clone(),
        kind: node.kind.clone(),
        is_heading,
        has_children,
        is_leaf: node.is_leaf_check(),
        expanded,
        is_last,
        guides: guides.to_vec(),
        done,
        total,
        checked,
        src_line: node.src_line,
    });
    if expanded {
        let n = visible.len();
        for (vi, (oi, ch)) in visible.iter().enumerate() {
            // Roots (depth 0) draw no vertical bar; deeper levels append this
            // node's "has a following sibling" flag for its children.
            let mut cg = guides.to_vec();
            if depth >= 1 {
                cg.push(!is_last);
            }
            push_method_row(
                ch,
                format!("{}/{}", key, oi),
                depth + 1,
                &cg,
                vi == n - 1,
                collapsed,
                show_comments,
                out,
            );
        }
    }
}

/// Flatten the selected card's checklist into rows, keyed `doc/section/card/path`
/// so collapse state never leaks between documents.
fn card_rows(
    roots: &[&MethodNode],
    di: usize,
    si: usize,
    ci: usize,
    collapsed: &HashSet<String>,
    show_comments: bool,
) -> Vec<MethodRow> {
    let mut out = Vec::new();
    let n = roots.len();
    for (i, root) in roots.iter().enumerate() {
        push_method_row(
            root,
            format!("{}/{}/{}/{}", di, si, ci, i),
            0,
            &[],
            i == n - 1,
            collapsed,
            show_comments,
            &mut out,
        );
    }
    out
}

/// Rows for a section+card by index (recomputes the card list).
fn rows_for(app: &App, si: usize, ci: usize) -> Vec<MethodRow> {
    let sections = crate::methodology::sections(app.method_tree());
    let Some(sec) = sections.get(si) else {
        return Vec::new();
    };
    let cards = card_roots(sec);
    let Some((_, roots)) = cards.get(ci) else {
        return Vec::new();
    };
    card_rows(
        roots,
        app.method_doc,
        si,
        ci,
        &app.method_collapsed,
        app.method_show_comments,
    )
}

fn method_section_count(app: &App) -> usize {
    crate::methodology::sections(app.method_tree()).len()
}

fn method_card_count(app: &App, si: usize) -> usize {
    crate::methodology::sections(app.method_tree())
        .get(si)
        .map(|s| card_roots(s).len())
        .unwrap_or(0)
}

/// A jump-palette destination: a card, or a heading within a card.
struct JumpTarget {
    si: usize,
    ci: usize,
    /// `Some(key)` for a heading row; `None` to just open the card.
    key: Option<String>,
    label: String,
}

/// Strip a leading `N. ` from a section title for cleaner jump labels.
fn short_section(title: &str) -> String {
    match title.split_once(". ") {
        Some((num, rest)) if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) => {
            rest.to_string()
        }
        _ => title.to_string(),
    }
}

fn collect_heading_targets(
    node: &MethodNode,
    key: String,
    base_label: &str,
    si: usize,
    ci: usize,
    out: &mut Vec<JumpTarget>,
) {
    if node.is_heading() {
        out.push(JumpTarget {
            si,
            ci,
            key: Some(key.clone()),
            label: format!("{} › {}", base_label, node.title),
        });
    }
    for (i, ch) in node.children.iter().enumerate() {
        collect_heading_targets(ch, format!("{}/{}", key, i), base_label, si, ci, out);
    }
}

fn jump_targets(app: &App) -> Vec<JumpTarget> {
    let sections = crate::methodology::sections(app.method_tree());
    let mut out = Vec::new();
    for (si, sec) in sections.iter().enumerate() {
        let sname = short_section(&sec.title);
        for (ci, (ctitle, roots)) in card_roots(sec).iter().enumerate() {
            let base = format!("{} › {}", sname, ctitle);
            out.push(JumpTarget {
                si,
                ci,
                key: None,
                label: base.clone(),
            });
            for (i, root) in roots.iter().enumerate() {
                collect_heading_targets(
                    root,
                    format!("{}/{}/{}/{}", app.method_doc, si, ci, i),
                    &base,
                    si,
                    ci,
                    &mut out,
                );
            }
        }
    }
    out
}

fn jump_filtered(app: &App) -> Vec<JumpTarget> {
    let q = app.method_query.to_lowercase();
    let terms: Vec<&str> = q.split_whitespace().collect();
    jump_targets(app)
        .into_iter()
        .filter(|t| {
            let l = t.label.to_lowercase();
            terms.iter().all(|term| l.contains(term))
        })
        .collect()
}

/// Greedy word-wrap `text` to `width` columns (>=1). Words longer than `width`
/// are emitted on their own line rather than split mid-word.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn method_row_item<'a>(r: &'a MethodRow, inner_width: u16) -> ListItem<'a> {
    let guide_style = Style::default().fg(C_GUIDE);

    // Tree connector prefix. Depth 0 (card roots) draw no guides.
    let mut gpre = String::new();
    let mut cont = String::new();
    if r.depth >= 1 {
        for &bar in &r.guides {
            gpre.push_str(if bar { "│ " } else { "  " });
            cont.push_str(if bar { "│ " } else { "  " });
        }
        gpre.push_str(if r.is_last { "└─" } else { "├─" });
        cont.push_str("  ");
    }
    let gwidth = gpre.chars().count();
    let child_bar = r.has_children && r.expanded;

    // Fold marker for any parent (heading or parent item).
    let fold: &str = if r.has_children {
        if r.expanded { "▾ " } else { "▸ " }
    } else {
        ""
    };
    let fold_w = fold.chars().count();

    // Per-kind marker, text style, and optional rollup badge.
    let (marker, marker_style, text_style, badge): (String, Style, Style, Option<(String, Style)>) =
        match r.kind {
            MethodKind::Heading(level) => (
                String::new(),
                guide_style,
                Style::default()
                    .fg(if level <= 3 { C_FG_BRIGHT } else { C_TITLE })
                    .add_modifier(Modifier::BOLD),
                (r.total > 0).then(|| {
                    (
                        format!("  {}/{}", r.done, r.total),
                        Style::default().fg(if r.done == r.total { C_CHECK } else { C_DIM }),
                    )
                }),
            ),
            MethodKind::Check => {
                let (m, ts) = if r.checked {
                    (
                        format!("{} ", IC_CHECK_ON),
                        Style::default().fg(C_DIM).add_modifier(Modifier::CROSSED_OUT),
                    )
                } else {
                    (format!("{} ", IC_CHECK_OFF), Style::default().fg(C_FG_BRIGHT))
                };
                let ms = Style::default().fg(if r.checked { C_CHECK } else { C_DIM });
                // Parent items show a rollup of their leaf checks.
                let badge = (r.has_children && r.total > 0).then(|| {
                    (
                        format!("  {}/{}", r.done, r.total),
                        Style::default().fg(if r.done == r.total { C_CHECK } else { C_DIM }),
                    )
                });
                (m, ms, ts, badge)
            }
            MethodKind::Note => (
                "· ".to_string(),
                guide_style,
                Style::default().fg(C_DIM).add_modifier(Modifier::ITALIC),
                None,
            ),
        };
    let marker_w = marker.chars().count();

    let lead_w = gwidth + fold_w + marker_w;
    let text_width = (inner_width as usize).saturating_sub(lead_w).max(1);
    let wrapped = wrap_text(&r.title, text_width);

    // Continuation lead: ancestor bars + blank elbow slot, a bar under this node
    // if it has children, then spaces to align under the text.
    let mut cont_lead = cont.clone();
    if fold_w > 0 {
        cont_lead.push_str(if child_bar { "│ " } else { "  " });
    }
    cont_lead.push_str(&" ".repeat(marker_w));

    let mut lines: Vec<Line> = Vec::new();
    for (i, seg) in wrapped.iter().enumerate() {
        if i == 0 {
            let mut spans: Vec<Span> = Vec::new();
            if !gpre.is_empty() {
                spans.push(Span::styled(gpre.clone(), guide_style));
            }
            if fold_w > 0 {
                spans.push(Span::styled(fold, guide_style));
            }
            if marker_w > 0 {
                spans.push(Span::styled(marker.clone(), marker_style));
            }
            spans.push(Span::styled(seg.clone(), text_style));
            if let Some((b, bs)) = &badge {
                spans.push(Span::styled(b.clone(), *bs));
            }
            lines.push(Line::from(spans));
        } else {
            lines.push(Line::from(vec![
                Span::styled(cont_lead.clone(), guide_style),
                Span::styled(seg.clone(), text_style),
            ]));
        }
    }
    ListItem::new(lines)
}

fn render_method_bar(frame: &mut Frame, area: Rect, app: &App) {
    let (done, total) = method_overall(app);
    let mut title = vec![
        Span::raw(" "),
        Span::styled(
            format!(" {}/{} done ", done, total),
            Style::default()
                .bg(C_DIM)
                .fg(C_FG_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    // Document tabs (Tab to switch) — one chip per loaded methodology.
    for (i, d) in app.method_docs.iter().enumerate() {
        let active = i == app.method_doc;
        title.push(Span::styled(
            format!(" {} ", d.name),
            if active {
                Style::default()
                    .bg(C_CHIP_BG)
                    .fg(C_ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_DIM)
            },
        ));
        title.push(Span::raw(" "));
    }
    let block = Block::default()
        .title_top(Line::from(title))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.method_jump_active || app.method_pending_reset {
            C_ACCENT
        } else {
            C_BORDER
        }));

    let line = if app.method_pending_reset {
        let name = app.method_docs.get(app.method_doc).map(|d| d.name.as_str()).unwrap_or("");
        Line::from(vec![Span::styled(
            format!("  Reset ALL checks in {}?  press y to confirm, any key to cancel", name),
            Style::default().fg(C_CHECK).add_modifier(Modifier::BOLD),
        )])
    } else if app.method_jump_active {
        let mut spans = vec![Span::styled(
            "  jump ",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        )];
        if app.method_jump_nav {
            spans.push(Span::styled(
                " NAV ",
                Style::default().bg(C_CHECK).fg(C_ACCENT_BG).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(app.method_query.as_str(), Style::default().fg(C_FG_BRIGHT)));
        // Inline ghost-text completion (jump query is append-only).
        if !app.method_jump_nav {
            if let Some(sfx) = complete_suffix(&jump_vocab(app), last_token(&app.method_query)) {
                spans.push(Span::styled(sfx, Style::default().fg(C_GUIDE)));
            }
        }
        Line::from(spans)
    } else {
        Line::from(vec![Span::styled(
            format!(
                "  ⌘F doc · Tab/1-9 section · hjkl move · gg/G ends · Space check · e/a/d edit · R reset · c comments {} · / jump",
                if app.method_show_comments { "on" } else { "off" }
            ),
            Style::default().fg(C_DIM),
        )])
    };
    if app.method_jump_active && !app.method_jump_nav {
        frame.set_cursor_position(Position::new(
            area.x + 1 + 7 + app.method_query.len() as u16,
            area.y + 1,
        ));
    }
    frame.render_widget(Paragraph::new(line).block(block), area);
}

/// The row of section number badges + the active section's name and progress.
fn render_method_sections(frame: &mut Frame, area: Rect, app: &App) {
    let sections = crate::methodology::sections(app.method_tree());
    let mut spans = vec![Span::raw(" ")];
    for i in 0..sections.len() {
        let active = i == app.method_section;
        let style = if active {
            Style::default()
                .bg(C_CHIP_BG)
                .fg(C_ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(C_DIM)
        };
        spans.push(Span::styled(format!(" {} ", i + 1), style));
        spans.push(Span::raw(" "));
    }
    if let Some(sec) = sections.get(app.method_section) {
        let (d, t) = sec.leaf_counts();
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            sec.title.clone(),
            Style::default().fg(C_FG_BRIGHT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("  {}/{}", d, t),
            Style::default().fg(if t > 0 && d == t { C_CHECK } else { C_DIM }),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The jump palette, drawn in place of the detail pane while active.
fn render_jump_palette(frame: &mut Frame, area: Rect, app: &App) {
    let cands = jump_filtered(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_ACCENT))
        .title(" JUMP ")
        .title_alignment(Alignment::Center);
    if cands.is_empty() {
        let p = Paragraph::new(vec![Line::from(""), Line::from("No matches")])
            .style(Style::default().fg(C_DIM))
            .alignment(Alignment::Center)
            .block(block);
        frame.render_widget(p, area);
        return;
    }
    let sel = app.method_jump_sel.min(cands.len() - 1);
    let items: Vec<ListItem> = cands
        .iter()
        .map(|t| {
            let (icon, istyle) = if t.key.is_some() {
                (format!("  {} ", IC_ITEM), Style::default().fg(C_DIM))
            } else {
                (format!("{} ", IC_SECTION), Style::default().fg(C_ACCENT))
            };
            ListItem::new(Line::from(vec![
                Span::styled(icon, istyle),
                Span::styled(t.label.clone(), Style::default().fg(C_FG_BRIGHT)),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(sel));
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(C_HIGHLIGHT_BG)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_method_view(frame: &mut Frame, area: Rect, app: &mut App) {
    let vparts = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);
    render_method_bar(frame, vparts[0], app);

    if crate::methodology::sections(app.method_tree()).is_empty() {
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from("No methodology loaded."),
            Line::from(Span::styled(
                "Add JSONs/methodology.md and restart.",
                Style::default().fg(C_DIM),
            )),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(C_BORDER)),
        );
        frame.render_widget(p, vparts[2]);
        return;
    }

    render_method_sections(frame, vparts[1], app);

    // Compute the active section's cards + selected-card rows as owned data so
    // the immutable borrow of the tree ends before we mutate `app`.
    let (si, ci, card_meta, rows): (usize, usize, Vec<(String, usize, usize)>, Vec<MethodRow>) = {
        let sections = crate::methodology::sections(app.method_tree());
        let si = app.method_section.min(sections.len() - 1);
        let sec = sections[si];
        let cards = card_roots(sec);
        let ci = if cards.is_empty() {
            0
        } else {
            app.method_card.min(cards.len() - 1)
        };
        let card_meta = cards
            .iter()
            .map(|(title, roots)| {
                let (d, t) = roots_counts(roots);
                (title.clone(), d, t)
            })
            .collect();
        let rows = cards
            .get(ci)
            .map(|(_, roots)| {
                card_rows(
                    roots,
                    app.method_doc,
                    si,
                    ci,
                    &app.method_collapsed,
                    app.method_show_comments,
                )
            })
            .unwrap_or_default();
        (si, ci, card_meta, rows)
    };
    app.method_section = si;
    app.method_card = ci;

    let cols = Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(vparts[2]);

    // Left: attack cards.
    let cards_focused = !app.method_focus && !app.method_jump_active;
    let items: Vec<ListItem> = card_meta
        .iter()
        .map(|(title, d, t)| {
            let complete = *t > 0 && d == t;
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("▸ {}", title),
                    Style::default().fg(C_FG_BRIGHT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}/{}", d, t),
                    Style::default().fg(if complete { C_CHECK } else { C_DIM }),
                ),
            ]))
        })
        .collect();
    let cblock = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(1, 1, 1, 0))
        .border_style(Style::default().fg(if cards_focused { C_ACCENT } else { C_BORDER }))
        .title(" ATTACKS ")
        .title_alignment(Alignment::Center);
    let mut cstate = ListState::default();
    if !card_meta.is_empty() {
        cstate.select(Some(ci));
    }
    let clist = List::new(items).block(cblock).highlight_style(if cards_focused {
        Style::default().bg(C_HIGHLIGHT_BG).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(C_HIGHLIGHT_DIM)
    });
    frame.render_stateful_widget(clist, cols[0], &mut cstate);

    // Right: jump palette, or the detail checklist tree.
    if app.method_jump_active {
        render_jump_palette(frame, cols[1], app);
        return;
    }

    if rows.is_empty() {
        app.method_tree_state.select(None);
    } else {
        let sel = app
            .method_tree_state
            .selected()
            .unwrap_or(0)
            .min(rows.len() - 1);
        app.method_tree_state.select(Some(sel));
    }

    let tree_focused = app.method_focus;
    let tblock = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(1, 1, 1, 0))
        .border_style(Style::default().fg(if tree_focused { C_ACCENT } else { C_BORDER }))
        .title(format!(
            " {} ",
            card_meta.get(ci).map(|(t, _, _)| t.as_str()).unwrap_or("DETAIL")
        ))
        .title_alignment(Alignment::Center);
    if rows.is_empty() {
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from("No items — 'a' to add, 'e' to edit"),
        ])
        .style(Style::default().fg(C_DIM))
        .alignment(Alignment::Center)
        .block(tblock);
        frame.render_widget(p, cols[1]);
        return;
    }
    let inner_width = cols[1].width.saturating_sub(4);
    let titems: Vec<ListItem> = rows.iter().map(|r| method_row_item(r, inner_width)).collect();
    let tlist = List::new(titems).block(tblock).highlight_style(if tree_focused {
        Style::default().bg(C_HIGHLIGHT_BG).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(C_HIGHLIGHT_DIM)
    });
    frame.render_stateful_widget(tlist, cols[1], &mut app.method_tree_state);
}

fn handle_method_key(app: &mut App, terminal: &mut DefaultTerminal, key: KeyEvent) -> Result<bool> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Jump palette captures all typing while active.
    if app.method_jump_active {
        // Esc is handled in the shared match (it backs out of nav, then cancels).
        let plain = !ctrl && !key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER);
        let sel_down = |app: &mut App| {
            let n = jump_filtered(app).len();
            if n > 0 {
                app.method_jump_sel = (app.method_jump_sel + 1).min(n - 1);
            }
        };
        match key.code {
            KeyCode::Enter => commit_method_jump(app),
            // Super+N (or Ctrl+N) toggles list-nav (j/k navigate, typing off).
            KeyCode::Char('n')
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                if app.method_jump_nav {
                    app.method_jump_nav = false;
                } else if !jump_filtered(app).is_empty() {
                    app.method_jump_nav = true;
                }
            }
            // Arrows always move the candidate selection.
            KeyCode::Down => sel_down(app),
            KeyCode::Up => app.method_jump_sel = app.method_jump_sel.saturating_sub(1),
            // j/k navigate only in nav mode.
            KeyCode::Char('j') if app.method_jump_nav && plain => sel_down(app),
            KeyCode::Char('k') if app.method_jump_nav && plain => {
                app.method_jump_sel = app.method_jump_sel.saturating_sub(1);
            }
            KeyCode::Char('u') if ctrl => {
                app.method_query.clear();
                app.method_jump_sel = 0;
            }
            KeyCode::Backspace if !app.method_jump_nav => {
                app.method_query.pop();
                app.method_jump_sel = 0;
            }
            // Tab accepts the inline ghost-text completion.
            KeyCode::Tab if !app.method_jump_nav => {
                if let Some(sfx) = complete_suffix(&jump_vocab(app), last_token(&app.method_query)) {
                    app.method_query.push_str(&sfx);
                    app.method_jump_sel = 0;
                }
            }
            KeyCode::Char(c) if !app.method_jump_nav && plain => {
                app.method_query.push(c);
                app.method_jump_sel = 0;
            }
            _ => {}
        }
        return Ok(false);
    }

    // A pending "reset all" confirmation swallows the next keypress.
    if app.method_pending_reset {
        if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            reset_method_doc(app);
            app.method_focus = false;
            app.method_tree_state.select(Some(0));
        }
        app.method_pending_reset = false;
        return Ok(false);
    }

    // Track vim `gg`: any key other than a follow-up `g` clears the pending state.
    let g_pending = app.method_g_pending;
    app.method_g_pending = false;

    match key.code {
        KeyCode::Char('/') => {
            app.method_jump_active = true;
            app.method_jump_nav = false;
            app.method_query.clear();
            app.method_jump_sel = 0;
        }
        // gg → jump to top of the focused pane.
        KeyCode::Char('g') => {
            if g_pending {
                if app.method_focus {
                    if !rows_for(app, app.method_section, app.method_card).is_empty() {
                        app.method_tree_state.select(Some(0));
                    }
                } else {
                    app.method_card = 0;
                    app.method_tree_state.select(Some(0));
                }
            } else {
                app.method_g_pending = true;
            }
        }
        // G → jump to bottom of the focused pane.
        KeyCode::Char('G') => {
            if app.method_focus {
                let len = rows_for(app, app.method_section, app.method_card).len();
                if len > 0 {
                    app.method_tree_state.select(Some(len - 1));
                }
            } else {
                let n = method_card_count(app, app.method_section);
                if n > 0 {
                    app.method_card = n - 1;
                    app.method_tree_state.select(Some(0));
                }
            }
        }
        // Switch document (Web ⇄ AD ⇄ …): Super+F / Ctrl+F forward, +Shift reverse.
        KeyCode::Char('f' | 'F')
            if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
        {
            let n = app.method_docs.len();
            if n > 0 {
                let reverse = key.modifiers.contains(KeyModifiers::SHIFT)
                    || matches!(key.code, KeyCode::Char('F'));
                let next = if reverse {
                    (app.method_doc + n - 1) % n
                } else {
                    (app.method_doc + 1) % n
                };
                switch_method_doc(app, next);
            }
        }
        // Section scrolling: Tab / Shift-Tab, or , / .
        KeyCode::Tab | KeyCode::Char('.') => {
            let n = method_section_count(app);
            if n > 0 {
                switch_method_section(app, (app.method_section + 1) % n);
            }
        }
        KeyCode::BackTab | KeyCode::Char(',') => {
            let n = method_section_count(app);
            if n > 0 {
                switch_method_section(app, (app.method_section + n - 1) % n);
            }
        }
        // Direct section jump (1-9).
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c as usize) - ('1' as usize);
            if idx < method_section_count(app) {
                switch_method_section(app, idx);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.method_focus {
                let len = rows_for(app, app.method_section, app.method_card).len();
                if len > 0 {
                    let i = app
                        .method_tree_state
                        .selected()
                        .map(|i| (i + 1).min(len - 1))
                        .unwrap_or(0);
                    app.method_tree_state.select(Some(i));
                }
            } else {
                let len = method_card_count(app, app.method_section);
                if len > 0 {
                    app.method_card = (app.method_card + 1).min(len - 1);
                    app.method_tree_state.select(Some(0));
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.method_focus {
                let i = app
                    .method_tree_state
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.method_tree_state.select(Some(i));
            } else {
                app.method_card = app.method_card.saturating_sub(1);
                app.method_tree_state.select(Some(0));
            }
        }
        // Right / l: focus the tree from the cards, else expand a collapsed parent.
        KeyCode::Right | KeyCode::Char('l') => {
            if !app.method_focus {
                focus_method_tree(app);
            } else {
                let rows = rows_for(app, app.method_section, app.method_card);
                if let Some(r) = app.method_tree_state.selected().and_then(|s| rows.get(s)) {
                    if r.has_children && !r.expanded {
                        app.method_collapsed.remove(&r.key);
                    }
                }
            }
        }
        // Left / h: collapse a parent, step to the parent row, or fall back to cards.
        KeyCode::Left | KeyCode::Char('h') => {
            if app.method_focus {
                let rows = rows_for(app, app.method_section, app.method_card);
                match app.method_tree_state.selected().and_then(|s| rows.get(s)) {
                    Some(r) if r.has_children && r.expanded => {
                        let k = r.key.clone();
                        app.method_collapsed.insert(k);
                    }
                    Some(r) if r.depth > 0 => {
                        if let Some((parent, _)) = r.key.rsplit_once('/') {
                            if let Some(pos) = rows.iter().position(|x| x.key == parent) {
                                app.method_tree_state.select(Some(pos));
                            }
                        }
                    }
                    // A card-root row (or nothing selected) — back to the cards.
                    _ => app.method_focus = false,
                }
            }
        }
        // Enter / Space: focus tree; toggle a heading; check a leaf; cascade a parent.
        KeyCode::Enter | KeyCode::Char(' ') => {
            if !app.method_focus {
                focus_method_tree(app);
            } else {
                let rows = rows_for(app, app.method_section, app.method_card);
                if let Some(r) = app.method_tree_state.selected().and_then(|s| rows.get(s)) {
                    if r.is_heading {
                        let k = r.key.clone();
                        toggle_method_collapsed(app, &k);
                    } else if r.kind == MethodKind::Check {
                        let want = !r.checked;
                        if r.is_leaf {
                            apply_method_checks(app, &[r.src_line], want);
                        } else {
                            // Parent: cascade the new state to every leaf under it.
                            let key = r.key.clone();
                            let lines = method_leaf_lines(app, &key);
                            apply_method_checks(app, &lines, want);
                        }
                    }
                }
            }
        }
        KeyCode::Char('e') => edit_method_section(app, terminal, false)?,
        KeyCode::Char('a') => edit_method_section(app, terminal, true)?,
        // Delete: the whole technique from the cards pane, else a tree item.
        KeyCode::Char('d') => {
            if app.method_focus {
                delete_method_row(app);
            } else {
                delete_method_card(app);
            }
        }
        // Reset all checks in the active document (asks y/n).
        KeyCode::Char('R') => app.method_pending_reset = true,
        // Toggle visibility of floating comments.
        KeyCode::Char('c') => {
            app.method_show_comments = !app.method_show_comments;
            // Keep the tree selection in range after rows appear/disappear.
            let len = rows_for(app, app.method_section, app.method_card).len();
            if len == 0 {
                app.method_tree_state.select(None);
            } else if let Some(s) = app.method_tree_state.selected() {
                app.method_tree_state.select(Some(s.min(len - 1)));
            }
        }
        _ => {}
    }
    Ok(false)
}

/// Save the live position of the current (doc, section) into `method_pos` and
/// record it as the document's active section.
fn stash_method_pos(app: &mut App) {
    app.method_pos.insert(
        (app.method_doc, app.method_section),
        MethodPos {
            card: app.method_card,
            tree_sel: app.method_tree_state.selected(),
            focus: app.method_focus,
        },
    );
    if let Some(s) = app.method_doc_section.get_mut(app.method_doc) {
        *s = app.method_section;
    }
}

/// Clamp the live section/card/row against the active doc's actual shape.
fn clamp_method_live(app: &mut App) {
    let nsec = method_section_count(app);
    app.method_section = if nsec == 0 { 0 } else { app.method_section.min(nsec - 1) };
    let ncards = method_card_count(app, app.method_section);
    app.method_card = if ncards == 0 { 0 } else { app.method_card.min(ncards - 1) };
    let len = rows_for(app, app.method_section, app.method_card).len();
    if len == 0 {
        app.method_tree_state.select(None);
        app.method_focus = false;
    } else {
        let s = app.method_tree_state.selected().unwrap_or(0).min(len - 1);
        app.method_tree_state.select(Some(s));
    }
}

/// Move focus into the detail tree, keeping the current row selection (clamped)
/// rather than snapping back to the top.
fn focus_method_tree(app: &mut App) {
    let len = rows_for(app, app.method_section, app.method_card).len();
    if len == 0 {
        return;
    }
    app.method_focus = true;
    let sel = app.method_tree_state.selected().unwrap_or(0).min(len - 1);
    app.method_tree_state.select(Some(sel));
}

fn switch_method_section(app: &mut App, idx: usize) {
    if idx == app.method_section {
        return;
    }
    // Remember where we were in the section we're leaving.
    stash_method_pos(app);
    let carry_focus = app.method_focus;
    app.method_section = idx;
    if let Some(p) = app.method_pos.get(&(app.method_doc, idx)).cloned() {
        // Revisiting: restore this section's card, row, and pane.
        app.method_card = p.card;
        app.method_focus = p.focus;
        app.method_tree_state.select(p.tree_sel.or(Some(0)));
    } else {
        // First visit: top of the list, but keep the current pane so you stay in flow.
        app.method_card = 0;
        app.method_tree_state.select(Some(0));
        app.method_focus = carry_focus;
    }
    if let Some(s) = app.method_doc_section.get_mut(app.method_doc) {
        *s = idx;
    }
    clamp_method_live(app);
}

/// Switch documents, remembering the position of the one we're leaving and
/// restoring the last section + position of the one we're entering.
fn switch_method_doc(app: &mut App, idx: usize) {
    if idx == app.method_doc {
        return;
    }
    stash_method_pos(app);
    app.method_doc = idx;
    app.method_jump_active = false;
    app.method_query.clear();

    let section = app.method_doc_section.get(idx).copied().unwrap_or(0);
    app.method_section = section;
    let p = app.method_pos.get(&(idx, section)).cloned().unwrap_or_default();
    app.method_card = p.card;
    app.method_focus = p.focus;
    app.method_tree_state.select(p.tree_sel.or(Some(0)));
    clamp_method_live(app);
}

fn toggle_method_collapsed(app: &mut App, key: &str) {
    if app.method_collapsed.contains(key) {
        app.method_collapsed.remove(key);
    } else {
        app.method_collapsed.insert(key.to_string());
    }
}

/// Set the checkbox marker on one line: swap `[ ]`↔`[x]`, or insert a marker
/// after the dash for a bare `- text` bullet so it becomes a real checkbox.
fn set_marker_line(l: &mut String, checked: bool) {
    let target = if checked { "[x]" } else { "[ ]" };
    for m in ["[ ]", "[x]", "[X]"] {
        if let Some(pos) = l.find(m) {
            l.replace_range(pos..pos + 3, target);
            return;
        }
    }
    if let Some(pos) = l.find("- ") {
        l.insert_str(pos + 2, &format!("{} ", target));
    }
}

/// Check/uncheck the given source lines, reconcile parent checkboxes to match
/// their leaves, then re-parse so the in-memory tree matches the file.
fn apply_method_checks(app: &mut App, lines: &[usize], checked: bool) {
    let Some(path) = app.method_path().map(|p| p.to_path_buf()) else {
        return;
    };
    let Ok(content) = fs::read_to_string(&path) else {
        return;
    };
    let mut fl: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    for &ln in lines {
        if let Some(l) = fl.get_mut(ln) {
            set_marker_line(l, checked);
        }
    }
    normalize_parent_markers(&mut fl);
    let _ = fs::write(&path, fl.join("\n") + "\n");
    app.method_reload();
}

/// Make every parent check's marker follow its leaves (`[x]` iff all leaves are
/// checked). Parses the current lines, then rewrites parent lines bottom-up.
fn normalize_parent_markers(fl: &mut [String]) {
    let tree = crate::methodology::parse(&fl.join("\n"));
    fn visit(node: &MethodNode, fl: &mut [String]) {
        for c in &node.children {
            visit(c, fl);
        }
        if node.kind == MethodKind::Check && !node.is_leaf_check() {
            if let Some(l) = fl.get_mut(node.src_line) {
                set_marker_line(l, node.all_leaves_checked());
            }
        }
    }
    for n in &tree {
        visit(n, fl);
    }
}

/// Resolve the node at `key` ("doc/section/card/i/j/…") within the active doc.
fn method_node_at<'a>(app: &'a App, key: &str) -> Option<&'a MethodNode> {
    let parts: Vec<usize> = key.split('/').filter_map(|s| s.parse().ok()).collect();
    if parts.len() < 4 || parts[0] != app.method_doc {
        return None;
    }
    let sections = crate::methodology::sections(app.method_tree());
    let sec = sections.get(parts[1])?;
    let cards = card_roots(sec);
    let (_, roots) = cards.get(parts[2])?;
    let mut node: &MethodNode = roots.get(parts[3]).copied()?;
    for &idx in &parts[4..] {
        node = node.children.get(idx)?;
    }
    Some(node)
}

/// Source lines of every leaf check under the node at `key`.
fn method_leaf_lines(app: &App, key: &str) -> Vec<usize> {
    fn collect(node: &MethodNode, out: &mut Vec<usize>) {
        if node.is_leaf_check() {
            out.push(node.src_line);
        }
        for c in &node.children {
            collect(c, out);
        }
    }
    let mut out = Vec::new();
    if let Some(node) = method_node_at(app, key) {
        collect(node, &mut out);
    }
    out
}

/// Uncheck every `- [x]` in the active document, then re-parse.
fn reset_method_doc(app: &mut App) {
    let Some(path) = app.method_path().map(|p| p.to_path_buf()) else {
        return;
    };
    if let Ok(content) = fs::read_to_string(&path) {
        let out: String = content
            .lines()
            .map(|l| l.replacen("- [x]", "- [ ]", 1).replacen("- [X]", "- [ ]", 1))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = fs::write(&path, out + "\n");
        app.method_reload();
    }
}

/// Delete the selected tree row: a leaf check/note (single line), or a heading
/// that has no remaining checklist items (an emptied sub-technique — its line +
/// following blank). Non-empty headings are left to the `$EDITOR` flow.
fn delete_method_row(app: &mut App) {
    let rows = rows_for(app, app.method_section, app.method_card);
    let Some(sel) = app.method_tree_state.selected() else {
        return;
    };
    let Some(r) = rows.get(sel) else { return };
    // A heading is deletable only when empty (no descendant rows).
    let has_children = rows.get(sel + 1).is_some_and(|n| n.depth > r.depth);
    if has_children {
        return;
    }
    let Some(path) = app.method_path().map(|p| p.to_path_buf()) else {
        return;
    };
    if delete_file_line(&path, r.src_line).is_ok() {
        app.method_reload();
        let len = rows_for(app, app.method_section, app.method_card).len();
        if len == 0 {
            app.method_tree_state.select(None);
        } else {
            let cur = app.method_tree_state.selected().unwrap_or(0).min(len - 1);
            app.method_tree_state.select(Some(cur));
        }
    }
}

/// Delete the entire selected attack card (its `## heading` block up to the next
/// heading). The synthetic "General" card is skipped.
fn delete_method_card(app: &mut App) {
    let Some((start, end)) = card_block_range(app, app.method_section, app.method_card) else {
        return;
    };
    let Some(path) = app.method_path().map(|p| p.to_path_buf()) else {
        return;
    };
    if let Ok(content) = fs::read_to_string(&path) {
        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        if start < lines.len() {
            let end = end.min(lines.len());
            lines.drain(start..end);
            let _ = fs::write(&path, lines.join("\n") + "\n");
            app.method_reload();
            let n = method_card_count(app, app.method_section);
            app.method_card = if n == 0 { 0 } else { app.method_card.min(n - 1) };
            app.method_tree_state.select(Some(0));
        }
    }
}

/// File line range `[start, end)` of the selected card's markdown block. Returns
/// `None` for the synthetic "General" card (it has no `##` heading to delete).
fn card_block_range(app: &App, si: usize, ci: usize) -> Option<(usize, usize)> {
    let sections = crate::methodology::sections(app.method_tree());
    let sec = sections.get(si)?;
    let has_general = sec.children.first().is_some_and(|c| !c.is_heading());
    if has_general && ci == 0 {
        return None;
    }
    let heading_idx = if has_general { ci - 1 } else { ci };
    let heading = sec.children.iter().filter(|c| c.is_heading()).nth(heading_idx)?;
    let path = app.method_path()?;
    let content = fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let start = heading.src_line;
    let end = next_heading_boundary(&lines, start + 1);
    Some((start, end))
}

/// First line index `>= from` that starts a new section/card heading (`# ` or
/// `## `), else the end of the file.
fn next_heading_boundary(lines: &[&str], from: usize) -> usize {
    for (i, l) in lines.iter().enumerate().skip(from) {
        let t = l.trim_start();
        if t.starts_with("# ") || t.starts_with("## ") {
            return i;
        }
    }
    lines.len()
}

fn delete_file_line(path: &Path, line_idx: usize) -> std::io::Result<()> {
    let content = fs::read_to_string(path)?;
    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    if line_idx < lines.len() {
        lines.remove(line_idx);
        // Swallow a single trailing blank line left behind by a heading delete.
        if line_idx < lines.len() && lines[line_idx].trim().is_empty() {
            lines.remove(line_idx);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    fs::write(path, out)
}

/// Jump to a selected palette destination: switch section/card, expand the
/// target heading's ancestors, and select its row.
fn commit_method_jump(app: &mut App) {
    let cands = jump_filtered(app);
    let target = cands.get(app.method_jump_sel).map(|t| (t.si, t.ci, t.key.clone()));
    app.method_jump_active = false;
    app.method_jump_nav = false;
    app.method_query.clear();
    let Some((si, ci, key)) = target else { return };
    // Remember where we were before jumping away.
    stash_method_pos(app);
    app.method_section = si;
    app.method_card = ci;
    app.method_focus = true;
    if let Some(s) = app.method_doc_section.get_mut(app.method_doc) {
        *s = si;
    }
    if let Some(key) = key {
        let mut acc = String::new();
        for part in key.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            app.method_collapsed.remove(&acc);
        }
        let rows = rows_for(app, si, ci);
        let pos = rows.iter().position(|r| r.key == key).unwrap_or(0);
        app.method_tree_state.select(Some(pos));
    } else {
        app.method_tree_state.select(Some(0));
    }
}

/// Edit the active section's markdown in `$EDITOR` (covers add/edit/delete of
/// techniques and items). With `add`, a fresh `## New Technique` card scaffold
/// is inserted before the editor opens. On save the slice is spliced back and
/// the tree re-parsed.
fn edit_method_section(app: &mut App, terminal: &mut DefaultTerminal, add: bool) -> Result<()> {
    let (start, end_opt) = {
        let sections = crate::methodology::sections(app.method_tree());
        let Some(sec) = sections.get(app.method_section) else {
            return Ok(());
        };
        (
            sec.src_line,
            sections.get(app.method_section + 1).map(|s| s.src_line),
        )
    };

    let Some(path) = app.method_path().map(|p| p.to_path_buf()) else {
        return Ok(());
    };
    let content = fs::read_to_string(&path)?;
    let all: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    if start >= all.len() {
        return Ok(());
    }
    let end = end_opt.unwrap_or(all.len()).min(all.len());
    let mut buf: Vec<String> = all[start..end].to_vec();

    // 1-based line within `buf` to place the editor cursor on.
    let mut cursor_line = if app.method_focus {
        let sel = app.method_tree_state.selected().unwrap_or(0);
        rows_for(app, app.method_section, app.method_card)
            .get(sel)
            .map(|r| r.src_line.saturating_sub(start) + 1)
            .unwrap_or(1)
    } else {
        card_block_range(app, app.method_section, app.method_card)
            .map(|(s, _)| s.saturating_sub(start) + 1)
            .unwrap_or(1)
    };

    if add {
        // Insert the scaffold after the section's last real content line.
        let mut ins = buf.len();
        while ins > 0 {
            let l = buf[ins - 1].trim();
            if l.is_empty() || l.chars().all(|c| c == '-') {
                ins -= 1;
            } else {
                break;
            }
        }
        for (k, s) in ["", "## New Technique", "", "- [ ] "].iter().enumerate() {
            buf.insert(ins + k, s.to_string());
        }
        // Land the cursor on the new blank checklist line.
        cursor_line = ins + 4;
    }

    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, Show)?;
    fs::write(get_editor_temp_path(), format!("{}\n", buf.join("\n")))?;
    let _ = open_editor_at(get_editor_temp_path(), cursor_line);
    let edited = fs::read_to_string(get_editor_temp_path())?;
    fs::remove_file(get_editor_temp_path())?;
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    terminal.clear()?;

    let mut new_lines: Vec<String> = Vec::with_capacity(all.len());
    new_lines.extend_from_slice(&all[..start]);
    new_lines.extend(edited.lines().map(|s| s.to_string()));
    new_lines.extend_from_slice(&all[end..]);
    let mut out = new_lines.join("\n");
    out.push('\n');
    fs::write(&path, out)?;

    app.method_reload();
    // Reclamp selections against the new tree.
    let nsec = method_section_count(app);
    if nsec > 0 {
        app.method_section = app.method_section.min(nsec - 1);
    }
    let ncards = method_card_count(app, app.method_section);
    if ncards > 0 {
        app.method_card = app.method_card.min(ncards - 1);
    }
    let len = rows_for(app, app.method_section, app.method_card).len();
    if len == 0 {
        app.method_tree_state.select(None);
    } else {
        let cur = app.method_tree_state.selected().unwrap_or(0).min(len - 1);
        app.method_tree_state.select(Some(cur));
    }
    Ok(())
}

fn render_top_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let tabs = [
        (IC_SEARCH, "Search"),
        (IC_BROWSE, "Browse"),
        (IC_METHOD, "Methodology"),
    ];
    const LEAD: usize = 2; // left inset
    const GAP: usize = 5; // space between tabs

    // Row 0: the tab labels. Active tab = bright accent + bold; others dim.
    // We track each tab's [col, width] so the underline lands exactly under it.
    let mut spans: Vec<Span> = vec![Span::raw(" ".repeat(LEAD))];
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut col = LEAD;
    for (i, (icon, label)) in tabs.iter().enumerate() {
        let active = i == app.top_tab;
        let text = format!("{}  {}", icon, label);
        let w = text.chars().count();
        let style = if active {
            Style::default()
                .fg(C_ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(C_DIM)
        };
        spans.push(Span::styled(text, style));
        ranges.push((col, w));
        col += w;
        if i + 1 < tabs.len() {
            spans.push(Span::styled(
                format!("{}·{}", " ".repeat(GAP / 2), " ".repeat(GAP - GAP / 2 - 1)),
                Style::default().fg(C_GUIDE),
            ));
            col += GAP;
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), Rect { height: 1, ..area });

    // Row 1: a heavy accent underline sitting under just the active tab.
    if area.height > 1 {
        if let Some(&(start, w)) = ranges.get(app.top_tab) {
            let underline = Line::from(vec![
                Span::raw(" ".repeat(start)),
                Span::styled("━".repeat(w), Style::default().fg(C_ACCENT)),
            ]);
            frame.render_widget(
                Paragraph::new(underline),
                Rect {
                    x: area.x,
                    y: area.y + 1,
                    width: area.width,
                    height: 1,
                },
            );
        }
    }
}

fn render_search_input(frame: &mut Frame, area: Rect, app: &App) {
    let mut mode_spans = vec![
        Span::styled(format!(" {} ", IC_SEARCH), Style::default().fg(C_ACCENT)),
        Span::styled(
            format!(" {} ", app.mode.to_string()),
            Style::default()
                .bg(C_CHIP_BG)
                .fg(C_ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    // Show the file filter as a subtle folder chip next to the mode badge (⌘F opens the picker).
    mode_spans.push(Span::raw("  "));
    mode_spans.push(Span::styled(
        format!(" {} ", IC_FOLDER),
        Style::default().fg(C_DIM),
    ));
    mode_spans.push(Span::styled(
        format!("{} ", app.file_filter_label()),
        Style::default().fg(C_TITLE),
    ));
    mode_spans.push(Span::raw(" "));
    // NAV badge when the results list has focus (j/k navigate, typing is off).
    if app.search_nav {
        mode_spans.push(Span::styled(
            " NAV ",
            Style::default()
                .bg(C_CHECK)
                .fg(C_ACCENT_BG)
                .add_modifier(Modifier::BOLD),
        ));
        mode_spans.push(Span::raw(" "));
    }
    let mode_title = Line::from(mode_spans);

    let mut block = Block::default()
        .title_top(mode_title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.search_nav { C_ACCENT } else { C_BORDER }));

    if app.is_chain_edit_mode {
        block = block.title_bottom(Line::from("CHAIN_EDIT_MODE").left_aligned());
    }

    let line = if app.query.is_empty() {
        Line::from(vec![Span::styled(
            "  search commands…",
            Style::default().fg(C_DIM),
        )])
    } else {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(app.query.as_str(), Style::default().fg(C_FG_BRIGHT)),
        ];
        // Inline ghost-text completion, only while typing at the end of the query.
        if !app.search_nav && app.cursor_index == app.query.len() {
            if let Some(sfx) = complete_suffix(&app.vocab, last_token(&app.query)) {
                spans.push(Span::styled(sfx, Style::default().fg(C_GUIDE)));
            }
        }
        Line::from(spans)
    };

    let input = Paragraph::new(line).block(block);

    // Only show the text cursor while typing (input mode).
    if !app.search_nav {
        frame.set_cursor_position(Position::new(
            area.x + 1 + 2 + app.cursor_index as u16, // border + padding + index
            area.y + 1,                               // border
        ));
    }
    frame.render_widget(input, area);
}

fn render_main(frame: &mut Frame, area: Rect, app: &mut App) {
    let cols = Layout::horizontal([
        Constraint::Percentage(app.main_split_pct),
        Constraint::Min(0),
    ])
    .split(area);

    render_results(frame, cols[0], app);

    let right_rows = Layout::vertical([
        Constraint::Percentage(app.right_split_pct),
        Constraint::Min(0),
    ])
    .split(cols[1]);

    render_detail(frame, right_rows[0], app);

    let entry_id = match app.selected_entry() {
        Some(e) => e.id.clone(),
        None => return,
    };

    let chains = app.find_chains_for_entry(&entry_id);

    let chain_entries: Vec<Vec<&Entry>> = chains
        .iter()
        .map(|chain| app.resolve_chain_steps(chain))
        .filter(|steps| !steps.is_empty())
        .collect();

    let current_chain = chain_entries
        .get(app.current_chain_index)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    // The chain pane shows its step cursor whenever it holds focus (nav or not).
    let chain_focused = app.search_focus == SearchPane::Chain;
    render_chain(
        frame,
        right_rows[1],
        current_chain,
        &entry_id,
        chain_focused,
        app.chain_sel,
    );
}

/// The source-file stem of an entry (e.g. `CAPE-CMDs`).
fn entry_stem(entry: &Entry) -> String {
    entry
        .source_file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The command's tool binary (lowercased) — the first real token, skipping common
/// wrappers and env assignments; empty for URL-only "commands".
fn tool_of(cmd: &str) -> String {
    const WRAPPERS: &[&str] = &[
        "sudo", "doas", "proxychains", "proxychains4", "python", "python3", "pipx",
        "env", "time", "watch", "nohup",
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

/// How well `token` matches lowercase `field`: 0 = not a substring, 1 = mid-word
/// substring, 2 = word-prefix, 3 = whole word.
fn token_quality(field: &str, token: &str) -> u32 {
    if token.is_empty() || !field.contains(token) {
        return 0;
    }
    let mut q = 1;
    for word in field.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if word == token {
            return 3;
        }
        if word.starts_with(token) {
            q = q.max(2);
        }
    }
    q
}

fn search(app: &mut App, reset_selection: bool) {
    app.current_chain_index = 0;
    app.desc_scroll = 0;
    app.chain_sel = 0;
    let previous_selection = app.list_state.selected();

    let query = app.query.trim();

    if query.is_empty() {
        // No fuzzy query: list every entry that passes the file filter, with
        // favorites floated to the top (stable, so file order is otherwise kept).
        let mut results: Vec<usize> = (0..app.entries.len())
            .filter(|&i| app.entry_passes_file(&app.entries[i]))
            .collect();
        results.sort_by_key(|&i| !app.entries[i].favorite);
        app.results = results;
    } else {
        // All-words matching: split the query into words and require EVERY word to
        // match some field (as a substring / word-prefix). Entries missing any word
        // are dropped, so loose fuzzy noise disappears. Each word scores by its best
        // field (title > heading > tool/cmd) and match quality (whole-word >
        // prefix > mid-word); the entry score is the sum.
        let query_lower = query.to_lowercase();
        let tokens: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(usize, i64, bool)> = Vec::new();
        for (i, entry) in app.entries.iter().enumerate() {
            if !app.entry_passes_file(entry) {
                continue;
            }
            let title = entry.title.to_lowercase();
            let heading = entry.heading_path.join(" > ").to_lowercase();
            let cmd = entry.cmd.to_lowercase();
            let tool = tool_of(&entry.cmd);

            // (lowercased field text, weight) considered for the active mode. The
            // prose description is deliberately excluded — a word buried there
            // shouldn't keep an otherwise-unrelated command in the results.
            let fields: Vec<(&str, i64)> = match app.mode {
                SearchMode::TITLE => vec![(title.as_str(), 1000)],
                SearchMode::HEADING => vec![(heading.as_str(), 1000)],
                SearchMode::CMD => vec![(tool.as_str(), 800), (cmd.as_str(), 400)],
                SearchMode::ALL => vec![
                    (title.as_str(), 1000),
                    (tool.as_str(), 800),
                    (heading.as_str(), 500),
                    (cmd.as_str(), 200),
                ],
            };

            let mut total: i64 = 0;
            let mut all_matched = true;
            for &tok in &tokens {
                let mut best = 0i64;
                for &(text, weight) in &fields {
                    let q = token_quality(text, tok) as i64;
                    if q > 0 {
                        best = best.max(weight + q * 250);
                    }
                }
                if best == 0 {
                    all_matched = false;
                    break;
                }
                total += best;
            }
            if !all_matched {
                continue;
            }

            // Reward the whole query being a title prefix, and prefer shorter
            // (more specific) titles as a tie-break.
            if title.starts_with(&query_lower) {
                total += 500;
            }
            total -= title.chars().count().min(250) as i64;

            scored.push((i, total, entry.favorite));
        }

        // Favorites first (they're already all-words-relevant since they matched),
        // then by score.
        scored.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
        app.results = scored.into_iter().map(|(i, _, _)| i).collect();
    }

    if app.results.is_empty() {
        app.list_state.select(None);
    } else if reset_selection {
        app.list_state.select(Some(0));
    } else {
        match previous_selection {
            None => app.list_state.select(None),
            Some(i) => app.list_state.select(Some(i.min(app.results.len() - 1))),
        }
    }
}
fn render_results(frame: &mut Frame, area: Rect, app: &mut App) {
    // A pane-focus situation exists when nav is on, or focus has moved to a right
    // pane. In that case the focused pane gets the accent border and the results
    // selection dims when focus is elsewhere; otherwise (plain typing) it's bright.
    let pane_focus_active = app.search_nav || app.search_focus != SearchPane::Results;
    let results_focused = app.search_focus == SearchPane::Results;
    let border = if results_focused && pane_focus_active {
        C_ACCENT
    } else {
        C_BORDER
    };
    let hl_bg = if pane_focus_active && !results_focused {
        C_HIGHLIGHT_DIM
    } else {
        C_HIGHLIGHT_BG
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::new(2, 2, 1, 0))
        .title_bottom(Line::from(vec![Span::styled(
            format!(" {} results ", app.results.len()),
            Style::default().fg(C_DIM),
        )]))
        .border_style(Style::default().fg(border));

    // Inner width = area minus the two border columns and the 2-col padding each side.
    let inner_width = area.width.saturating_sub(6) as usize;
    let cmd_width = inner_width.saturating_sub(4);

    let items: Vec<ListItem> = app
        .results
        .iter()
        .filter_map(|&i| app.entries.get(i))
        .map(|e| {
            let mut lines: Vec<Line> = Vec::new();

            // Title (bold), with a small ✦ pinned top-right on favorites.
            let title_style = Style::default().fg(C_FG_BRIGHT).add_modifier(Modifier::BOLD);
            let tw = if e.favorite {
                inner_width.saturating_sub(2).max(1)
            } else {
                inner_width.max(1)
            };
            let tchunks = textwrap::wrap(&e.title, tw);
            if e.favorite && tchunks.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(inner_width.saturating_sub(1))),
                    Span::styled(IC_STAR, Style::default().fg(C_STAR)),
                ]));
            }
            for (idx, chunk) in tchunks.iter().enumerate() {
                if idx == 0 && e.favorite {
                    let pad = inner_width.saturating_sub(chunk.chars().count() + 1);
                    lines.push(Line::from(vec![
                        Span::styled(chunk.to_string(), title_style),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(IC_STAR, Style::default().fg(C_STAR)),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(chunk.to_string(), title_style)));
                }
            }

            // Heading breadcrumb (dim).
            let breadcrumb = e.heading_path.join(" › ");
            for chunk in textwrap::wrap(&breadcrumb, inner_width.max(1)) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(C_DIM),
                )));
            }

            // Command.
            let wrapped = textwrap::wrap(&e.cmd, cmd_width.max(1));
            for (idx, chunk) in wrapped.iter().enumerate() {
                let prefix = if idx == 0 { "  $ " } else { "    " };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(C_ACCENT)),
                    Span::styled(chunk.to_string(), Style::default().fg(C_FG_BRIGHT)),
                ]));
            }

            lines.push(Line::from(""));

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(hl_bg).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, area, &mut app.list_state);
}
fn render_chain(
    frame: &mut Frame,
    area: Rect,
    chain_entries: &[&Entry],
    selected_entry_id: &str,
    focused: bool,
    chain_sel: usize,
) {
    let border = if focused { C_ACCENT } else { C_BORDER };
    if chain_entries.is_empty() {
        let p = Paragraph::new("No chain for this command")
            .style(Style::default().fg(C_DIM))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(border))
                    .title(" ATTACK CHAIN ")
                    .title_alignment(Alignment::Center),
            );

        frame.render_widget(p, area);
        return;
    };

    // Height (in wrapped rows) each step occupies: 1 blank + wrapped(cmd) + 1
    // blank. ratatui's own line_count is unstable, so approximate the wrapped
    // command height with textwrap, matching render_detail's approach.
    let inner_w = area.width.saturating_sub(6).max(1) as usize;
    let inner_h = area.height.saturating_sub(3);
    let step_heights: Vec<u16> = chain_entries
        .iter()
        .map(|e| 2 + textwrap::wrap(&e.cmd, inner_w).len().max(1) as u16)
        .collect();

    let lines: Vec<Line> = chain_entries
        .iter()
        .enumerate()
        .flat_map(|(idx, chain_entry)| {
            let is_cursor = focused && idx == chain_sel;
            let is_current = selected_entry_id == chain_entry.id;
            let (marker, marker_style, cmd_style) = if is_cursor {
                (
                    "▸ ",
                    Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
                    Style::default()
                        .fg(C_FG_BRIGHT)
                        .bg(C_HIGHLIGHT_BG)
                        .add_modifier(Modifier::BOLD),
                )
            } else if is_current {
                let s = Style::default().fg(C_FG_BRIGHT).add_modifier(Modifier::BOLD);
                ("• ", s, s)
            } else {
                let s = Style::default().fg(C_DIM);
                ("  ", s, s)
            };
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(marker, marker_style),
                    Span::styled(chain_entry.cmd.as_str(), cmd_style),
                ]),
                Line::from(""),
            ]
        })
        .collect();

    // Scroll so the highlighted step (cursor when focused, else the current
    // command) stays inside the viewport instead of only the marker moving.
    let target = if focused {
        chain_sel
    } else {
        chain_entries
            .iter()
            .position(|e| e.id == selected_entry_id)
            .unwrap_or(0)
    };
    let target = target.min(step_heights.len().saturating_sub(1));
    let start: u16 = step_heights[..target].iter().sum();
    let end = start.saturating_add(step_heights.get(target).copied().unwrap_or(0));
    let total: u16 = step_heights.iter().sum();
    let mut scroll = 0u16;
    if end > inner_h {
        scroll = end - inner_h;
    }
    if start < scroll {
        scroll = start;
    }
    scroll = scroll.min(total.saturating_sub(inner_h));

    let chain_widget: Paragraph<'_> = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .padding(Padding::new(2, 2, 1, 0))
                .title_top(" ATTACK CHAIN ")
                .title_alignment(Alignment::Center)
                .border_style(Style::default().fg(border)),
        );

    frame.render_widget(chain_widget, area);
}

fn render_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.search_focus == SearchPane::Description;
    let border = if focused { C_ACCENT } else { C_BORDER };

    let Some(entry) = app.selected_entry() else {
        let p = Paragraph::new(vec![Line::from(""), Line::from("Select an entry")])
            .style(Style::default().fg(C_DIM))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(border))
                    .title(" DESCRIPTION ")
                    .title_alignment(Alignment::Center),
            );

        frame.render_widget(p, area);
        return;
    };

    let lines_iter = entry
        .description
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(C_DESC))));

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(entry.title.clone(), Style::default().fg(C_TITLE))),
    ];
    lines.extend(lines_iter);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .padding(Padding::new(2, 2, 1, 0))
        .title(" DESCRIPTION ")
        .title_alignment(Alignment::Center);

    // Clamp the scroll offset to the wrapped content height so ↓ can't scroll the
    // text off into empty space. Inner area = borders (2) + horizontal padding (4)
    // and top padding (1). ratatui's own line_count is unstable, so approximate
    // the wrapped height with textwrap: 1 blank + wrapped title + wrapped desc.
    let inner_w = area.width.saturating_sub(6).max(1) as usize;
    let inner_h = area.height.saturating_sub(3);
    let mut total: u16 = 1;
    total = total.saturating_add(textwrap::wrap(&entry.title, inner_w).len().max(1) as u16);
    for l in entry.description.lines() {
        total = total.saturating_add(textwrap::wrap(l, inner_w).len().max(1) as u16);
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    app.desc_scroll = app.desc_scroll.min(total.saturating_sub(inner_h));
    let top = para.block(block).scroll((app.desc_scroll, 0));
    frame.render_widget(top, area);
}

#[cfg(test)]
mod tests {
    use super::{
        chain_present, file_filter_toggle, init_chain_sel, parse_template_str, reset_search_view,
        search_nav_down, search_scroll_down,
    };
    use crate::{App, Chain, Entry, SearchPane};
    use std::path::{Path, PathBuf};

    fn mk_entry(id: &str) -> Entry {
        Entry {
            id: id.to_string(),
            title: format!("title {id}"),
            cmd: format!("cmd {id}"),
            description: "desc".to_string(),
            source_file: PathBuf::from("/tmp/X-CMDs.json"),
            heading_path: vec!["H".to_string()],
            favorite: false,
        }
    }

    #[test]
    fn chain_focus_scroll_and_present() {
        let entries = vec![mk_entry("a"), mk_entry("b"), mk_entry("c")];
        let chains = vec![Chain {
            id: "ch".to_string(),
            steps: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            name: "n".to_string(),
            description: "d".to_string(),
        }];
        let mut app = App::new(
            entries,
            chains,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp"),
            vec![],
        );
        app.results = vec![0, 1, 2];
        app.list_state.select(Some(0));
        app.current_chain_index = 0;

        // Focus the chain: the cursor starts on the current command (a → 0).
        app.search_focus = SearchPane::Chain;
        init_chain_sel(&mut app);
        assert_eq!(app.chain_sel, 0);

        // Nav off + focus chain: j steps through the chain, clamped at the end.
        search_scroll_down(&mut app);
        assert_eq!(app.chain_sel, 1);
        search_scroll_down(&mut app);
        assert_eq!(app.chain_sel, 2);
        search_scroll_down(&mut app);
        assert_eq!(app.chain_sel, 2);

        // Enter presents the highlighted step (c) as the main selection.
        chain_present(&mut app);
        assert_eq!(app.list_state.selected(), Some(2));
        assert_eq!(app.search_focus, SearchPane::Results);
    }

    #[test]
    fn nav_switches_panels_without_stepping() {
        use super::search_nav_up;
        let entries = vec![mk_entry("a"), mk_entry("b"), mk_entry("c")];
        let chains = vec![Chain {
            id: "ch".to_string(),
            steps: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            name: "n".to_string(),
            description: "d".to_string(),
        }];
        let mut app = App::new(
            entries,
            chains,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp"),
            vec![],
        );
        app.results = vec![0, 1, 2];
        app.list_state.select(Some(0));

        // Description → Chain switches the panel, not the chain cursor.
        app.search_focus = SearchPane::Description;
        search_nav_down(&mut app);
        assert_eq!(app.search_focus, SearchPane::Chain);
        assert_eq!(app.chain_sel, 0);

        // In nav mode, another ↓ on the chain is a no-op (no stepping).
        search_nav_down(&mut app);
        assert_eq!(app.chain_sel, 0);
        assert_eq!(app.search_focus, SearchPane::Chain);

        // ↑ from the chain returns to the description.
        search_nav_up(&mut app);
        assert_eq!(app.search_focus, SearchPane::Description);
    }

    const BLOCK: &str = "--- TITLE ---\n\
Run Full Tenant Enumeration via AzurEnum\n\
--- HEADING_PATH ---\n\
Azure > AD Enumeration > Full Scan > Linux\n\
--- DESCRIPTION ---\n\
Runs AzurEnum for a comprehensive Entra ID tenant assessment.\n\
--- SOURCE-FILE ---\n\
OAOTC\n\
--- COMMANDS ---\n\
pipx install azurenum\n\
azurenum --interactive\n";

    #[test]
    fn parses_a_good_block() {
        let cmds_dir = Path::new("/tmp/JSONs/cmds");
        let e = parse_template_str("deadbeef", BLOCK, cmds_dir, false).unwrap();
        assert_eq!(e.id, "deadbeef");
        assert_eq!(e.title, "Run Full Tenant Enumeration via AzurEnum");
        assert_eq!(
            e.heading_path,
            vec!["Azure", "AD Enumeration", "Full Scan", "Linux"]
        );
        assert_eq!(e.cmd, "pipx install azurenum\nazurenum --interactive");
        assert_eq!(e.source_file, cmds_dir.join("OAOTC-CMDs.json"));
        assert!(!e.favorite);
    }

    #[test]
    fn file_filter_multiselect_and_reset() {
        let mut entries = vec![mk_entry("a"), mk_entry("b"), mk_entry("c")];
        entries[0].source_file = PathBuf::from("/x/CAPE-CMDs.json");
        entries[1].source_file = PathBuf::from("/x/OAOTC-CMDs.json");
        entries[2].source_file = PathBuf::from("/x/CAPE-CMDs.json");
        let mut app = App::new(
            entries,
            vec![],
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp"),
            vec![],
        );
        // file_filters = ["All", "CAPE-CMDs", "OAOTC-CMDs"] (sorted stems).
        assert_eq!(app.file_filters, vec!["All", "CAPE-CMDs", "OAOTC-CMDs"]);

        // Empty selection = everything passes.
        assert!(app.entry_passes_file(&app.entries[0]));

        // Toggle CAPE (idx 1) on: only CAPE entries pass; toggling OAOTC adds it.
        file_filter_toggle(&mut app, 1);
        assert!(app.entry_passes_file(&app.entries[0])); // CAPE
        assert!(!app.entry_passes_file(&app.entries[1])); // OAOTC excluded
        file_filter_toggle(&mut app, 2);
        assert!(app.entry_passes_file(&app.entries[1])); // now included
        assert_eq!(app.file_selected.len(), 2);

        // Toggling CAPE again removes it.
        file_filter_toggle(&mut app, 1);
        assert!(!app.entry_passes_file(&app.entries[0]));

        // "0" (All) clears the whole selection.
        file_filter_toggle(&mut app, 0);
        assert!(app.file_selected.is_empty());

        // Reset restores layout/view, stays in nav, keeps the filter.
        app.file_selected.insert("CAPE-CMDs".to_string());
        app.main_split_pct = 30;
        app.search_focus = SearchPane::Chain;
        app.search_nav = true;
        app.desc_scroll = 5;
        reset_search_view(&mut app);
        assert_eq!(app.main_split_pct, 60);
        assert_eq!(app.search_focus, SearchPane::Results);
        assert!(app.search_nav); // stays in nav
        assert_eq!(app.desc_scroll, 0);
        assert!(app.file_selected.contains("CAPE-CMDs")); // filter preserved
    }

    #[test]
    fn rejects_block_missing_commands() {
        let no_cmds = "--- TITLE ---\nX\n--- HEADING_PATH ---\nA > B\n\
                       --- DESCRIPTION ---\nd\n--- SOURCE-FILE ---\nOAOTC\n--- COMMANDS ---\n";
        let r = parse_template_str("00000000", no_cmds, Path::new("/tmp"), false);
        assert!(r.is_err());
    }
}
