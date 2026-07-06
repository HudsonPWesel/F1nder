use rand::RngExt;
use ratatui::widgets::Wrap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::stdout;
use std::path::{Path, PathBuf};

use crate::{App, Chain, Entry, SearchMode, TreeNode};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nucleo::Config;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs};
use ratatui::{DefaultTerminal, Frame};
use std::process::Command;
use std::sync::OnceLock;

const C_BORDER: Color = Color::Rgb(140, 150, 170); // muted blue-gray for borders
const C_DIM: Color = Color::Rgb(100, 110, 130); // dim text / breadcrumbs
const C_FG_BRIGHT: Color = Color::Rgb(220, 228, 245); // bright/primary text
const C_ACCENT: Color = Color::Rgb(92, 196, 255); // cyan accent (tabs, mode badge)
const C_ACCENT_BG: Color = Color::Rgb(14, 24, 38); // dark bg for accent badge text
const C_HIGHLIGHT_BG: Color = Color::Rgb(20, 30, 40); // list selection highlight
const C_TITLE: Color = Color::Rgb(175, 185, 209); // description / title text
const C_DESC: Color = Color::Rgb(140, 150, 170); // description body text

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

fn parse_template(entry_id: &str, app: &App) -> Result<Entry> {
    let contents = fs::read_to_string(get_editor_temp_path())?;
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
                let full_path = app.cmds_dir.join(filename);

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
    };
    Ok(new_entry)
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

fn handle_key_event(app: &mut App, terminal: &mut DefaultTerminal) -> Result<bool> {
    if let Event::Key(key) = event::read()? {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        // The Browse tab has its own key handling. Esc (quit) and `[`/`]` (tab
        // switching) still fall through to the shared match below.
        if app.top_tab == 1
            && key.code != KeyCode::Esc
            && !matches!(key.code, KeyCode::Char('[') | KeyCode::Char(']'))
        {
            return handle_browse_key(app, terminal, key);
        }
        match key.code {
            KeyCode::Esc => return Ok(true),
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
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = app.selected_entry() {
                    let Some(selected_index) = app.selected_entry_index() else {
                        return Ok(false);
                    };

                    // Disable raw mode and leave alternate screen
                    disable_raw_mode()?;
                    execute!(stdout(), LeaveAlternateScreen, Show)?;
                    let out = entry_to_template(&entry);
                    fs::write(get_editor_temp_path(), out)?;

                    let _ = open_editor(get_editor_temp_path());

                    let updated_entry = parse_template(&entry.id, &app)?;
                    app.entries[selected_index] = updated_entry;
                    app.dirty = true;

                    fs::remove_file(get_editor_temp_path())?;

                    // Re-enable raw mode and re-enter alternate screen
                    enable_raw_mode()?;
                    execute!(stdout(), EnterAlternateScreen, Hide)?;
                    terminal.clear()?;
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
                app.mode = match app.mode {
                    SearchMode::CMD => SearchMode::HEADING,
                    SearchMode::HEADING => SearchMode::TITLE,
                    SearchMode::TITLE => SearchMode::ALL,
                    SearchMode::ALL => SearchMode::CMD,
                };
                search(app, true);
            }

            KeyCode::Char('[') => {
                app.top_tab = if app.top_tab == 0 { 1 } else { 0 };
            }
            KeyCode::Char(']') => {
                app.top_tab = (app.top_tab + 1) % 2;
            }

            KeyCode::Down => {
                let len = app.results.len();
                if len > 0 {
                    let i = app
                        .list_state
                        .selected()
                        .map(|i| if i == len - 1 { len - 1 } else { i + 1 })
                        .unwrap_or(0);
                    app.list_state.select(Some(i));
                }
                app.current_chain_index = 0;
            }
            KeyCode::Up => {
                let len = app.results.len();
                if len > 0 {
                    let i = app
                        .list_state
                        .selected()
                        .map(|i| if i == 0 { 0 } else { i - 1 })
                        .unwrap_or(0);
                    app.list_state.select(Some(i));
                }
                app.current_chain_index = 0;
            }
            KeyCode::Left => {
                app.cursor_index = app.cursor_index.saturating_sub(1);
            }
            KeyCode::Right => {
                if app.cursor_index < app.query.len() {
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
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
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

    match app.top_tab {
        0 => {
            render_search_input(frame, chunks[2], app);
            render_main(frame, chunks[3], app);
        }
        _ => {
            render_folder_view(frame, chunks[3], app);
        }
    }
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

fn count_leaves(node: &TreeNode) -> usize {
    if node.entry_index.is_some() {
        return 1;
    }
    node.children.iter().map(count_leaves).sum()
}

/// Walk the tree, emitting only rows whose ancestor folders are all expanded.
/// A folder's key is its path (ancestor texts joined by NUL) so it stays stable
/// across rebuilds.
fn flatten(
    nodes: &[TreeNode],
    depth: usize,
    prefix: &str,
    expanded: &HashSet<String>,
    out: &mut Vec<BrowseRow>,
) {
    for node in nodes {
        let is_folder = node.entry_index.is_none();
        let key = if prefix.is_empty() {
            node.text.clone()
        } else {
            format!("{}\u{0}{}", prefix, node.text)
        };
        let is_expanded = expanded.contains(&key);
        out.push(BrowseRow {
            depth,
            text: node.text.clone(),
            key: key.clone(),
            entry_index: node.entry_index,
            is_folder,
            expanded: is_expanded,
            count: if is_folder { count_leaves(node) } else { 1 },
        });
        if is_folder && is_expanded {
            flatten(&node.children, depth + 1, &key, expanded, out);
        }
    }
}

/// The Browse tree flattened to its currently-visible rows.
fn browse_rows(app: &App) -> Vec<BrowseRow> {
    let roots = build_tree(&app.entries);
    let mut out = Vec::new();
    flatten(&roots, 0, "", &app.expanded, &mut out);
    out
}

fn browse_selected_entry_index(app: &App) -> Option<usize> {
    let sel = app.browse_state.selected()?;
    browse_rows(app).get(sel).and_then(|r| r.entry_index)
}

fn handle_browse_key(app: &mut App, terminal: &mut DefaultTerminal, key: KeyEvent) -> Result<bool> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Down => {
            let len = browse_rows(app).len();
            if len > 0 {
                let i = app
                    .browse_state
                    .selected()
                    .map(|i| (i + 1).min(len - 1))
                    .unwrap_or(0);
                app.browse_state.select(Some(i));
            }
        }
        KeyCode::Up => {
            let len = browse_rows(app).len();
            if len > 0 {
                let i = app
                    .browse_state
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.browse_state.select(Some(i));
            }
        }
        // Enter/Right: expand a folder, or copy + exit on a command.
        KeyCode::Enter | KeyCode::Right => {
            let rows = browse_rows(app);
            if let Some(row) = app.browse_state.selected().and_then(|s| rows.get(s)) {
                if row.is_folder {
                    if row.expanded {
                        app.expanded.remove(&row.key);
                    } else {
                        app.expanded.insert(row.key.clone());
                    }
                } else if key.code == KeyCode::Enter {
                    if let Some(idx) = row.entry_index {
                        copy_to_clipboard(&app.entries[idx].cmd);
                    }
                    return Ok(true);
                }
            }
        }
        // Left: collapse an expanded folder, else jump to the parent row.
        KeyCode::Left => {
            let rows = browse_rows(app);
            if let Some(sel) = app.browse_state.selected() {
                if let Some(row) = rows.get(sel) {
                    if row.is_folder && row.expanded {
                        app.expanded.remove(&row.key);
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
        // Edit the selected command, reusing the existing editor template flow.
        KeyCode::Char('e') if ctrl => {
            if let Some(idx) = browse_selected_entry_index(app) {
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
    roots
}
fn render_folder_view(frame: &mut Frame, area: Rect, app: &mut App) {
    let cols =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);

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
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(
                        format!("{} {}", marker, r.text),
                        Style::default()
                            .fg(C_FG_BRIGHT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  ({})", r.count), Style::default().fg(C_DIM)),
                ]))
            } else {
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled("  ", Style::default().fg(C_DIM)),
                    Span::styled(r.text.clone(), Style::default().fg(C_TITLE)),
                ]))
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title_bottom(format!(" BROWSE  {} entries ", app.entries.len()))
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
        .border_style(Style::default().fg(C_BORDER))
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

    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(block);
    frame.render_widget(p, area);
}
fn render_top_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let tabs = Tabs::new(vec!["Search", "Browse"])
        .select(app.top_tab)
        .style(Style::default().fg(C_DIM))
        .highlight_style(
            Style::default()
                .fg(C_ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(" ")
        .padding("  ", "  ");

    frame.render_widget(tabs, area);
}

fn render_search_input(frame: &mut Frame, area: Rect, app: &App) {
    let mut mode_spans = vec![
        Span::raw(" "),
        Span::styled(
            format!(" {} ", app.mode.to_string()),
            Style::default()
                .bg(C_ACCENT)
                .fg(C_ACCENT_BG)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    // Show an active file: filter as a badge next to the mode badge.
    if let (Some(filter), _) = parse_query(&app.query) {
        mode_spans.push(Span::raw(" "));
        mode_spans.push(Span::styled(
            format!(" file:{} ", filter),
            Style::default()
                .bg(C_DIM)
                .fg(C_FG_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ));
    }
    mode_spans.push(Span::raw(" "));
    let mode_title = Line::from(mode_spans);

    let mut block = Block::default()
        .title_top(mode_title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(C_BORDER));

    if app.is_chain_edit_mode {
        block = block.title_bottom(Line::from("CHAIN_EDIT_MODE").left_aligned());
    }

    let line = if app.query.is_empty() {
        Line::from(vec![Span::raw("  ")])
    } else {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(app.query.as_str(), Style::default().fg(C_FG_BRIGHT)),
        ])
    };

    let input = Paragraph::new(line).block(block);

    frame.set_cursor_position(Position::new(
        area.x + 1 + 2 + app.cursor_index as u16, // border + padding + index
        area.y + 1,                               // border
    ));
    frame.render_widget(input, area);
}

fn render_main(frame: &mut Frame, area: Rect, app: &mut App) {
    let cols =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);

    render_results(frame, cols[0], app);

    let right_rows =
        Layout::vertical([Constraint::Percentage(60), Constraint::Min(0)]).split(cols[1]);

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

    render_chain(frame, right_rows[1], current_chain, &entry_id);
}

/// Split a raw query into an optional `file:<stem>` filter and the remaining
/// fuzzy query. Only the first `file:` token is honored; the rest is the query.
pub fn parse_query(raw: &str) -> (Option<String>, String) {
    let mut file_filter: Option<String> = None;
    let mut rest: Vec<&str> = Vec::new();

    for tok in raw.split_whitespace() {
        let lower = tok.to_lowercase();
        if let Some(value) = lower.strip_prefix("file:") {
            if file_filter.is_none() && !value.is_empty() {
                // Preserve the original-case value ("file:" is 5 ASCII bytes).
                file_filter = Some(tok[5..].to_string());
                continue;
            }
        }
        rest.push(tok);
    }

    (file_filter, rest.join(" "))
}

/// Whether an entry's source-file stem contains the (lowercased) filter.
fn entry_matches_file(entry: &Entry, filter_lc: &Option<String>) -> bool {
    match filter_lc {
        None => true,
        Some(f) => entry
            .source_file
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase().contains(f))
            .unwrap_or(false),
    }
}

fn search(app: &mut App, reset_selection: bool) {
    app.current_chain_index = 0;
    let previous_selection = app.list_state.selected();

    let (file_filter, raw_query) = parse_query(&app.query);
    let filter_lc = file_filter.as_ref().map(|s| s.to_lowercase());
    let query = raw_query.trim();

    if query.is_empty() {
        // No fuzzy query: list every entry that passes the file filter.
        app.results = (0..app.entries.len())
            .filter(|&i| entry_matches_file(&app.entries[i], &filter_lc))
            .collect();
    } else {
        let mut matcher = nucleo::Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let query_lower = query.to_lowercase();

        let has_substr = |text: &str| text.to_lowercase().contains(&query_lower);
        let title_bonus = |t: &str| if has_substr(t) { 512 } else { 0 };
        let cmd_bonus = |t: &str| if has_substr(t) { 256 } else { 0 };
        let heading_bonus = |t: &str| if has_substr(t) { 128 } else { 0 };

        let mut scored: Vec<(usize, u32)> = Vec::new();

        for (i, entry) in app.entries.iter().enumerate() {
            if !entry_matches_file(entry, &filter_lc) {
                continue;
            }
            match app.mode {
                SearchMode::CMD => {
                    let mut buf = Vec::new();
                    let haystack = nucleo::Utf32Str::new(entry.cmd.as_str(), &mut buf);
                    if let Some(score) = pattern.score(haystack, &mut matcher) {
                        scored.push((i, score.saturating_add(cmd_bonus(&entry.cmd))));
                    }
                }
                SearchMode::TITLE => {
                    let mut buf = Vec::new();
                    let haystack = nucleo::Utf32Str::new(entry.title.as_str(), &mut buf);
                    if let Some(score) = pattern.score(haystack, &mut matcher) {
                        scored.push((i, score.saturating_add(title_bonus(&entry.title))));
                    }
                }
                SearchMode::HEADING => {
                    let temp_string = entry.heading_path.join(" > ");
                    let mut buf = Vec::new();
                    let haystack = nucleo::Utf32Str::new(&temp_string, &mut buf);
                    if let Some(score) = pattern.score(haystack, &mut matcher) {
                        scored.push((i, score.saturating_add(heading_bonus(&temp_string))));
                    }
                }
                SearchMode::ALL => {
                    let heading_str = entry.heading_path.join(" > ");

                    let mut h_buf = Vec::new();
                    let h_hay = nucleo::Utf32Str::new(&heading_str, &mut h_buf);
                    let h_score = pattern.score(h_hay, &mut matcher).unwrap_or(0);

                    let mut t_buf = Vec::new();
                    let t_hay = nucleo::Utf32Str::new(entry.title.as_str(), &mut t_buf);
                    let t_score = pattern.score(t_hay, &mut matcher).unwrap_or(0);

                    let mut c_buf = Vec::new();
                    let c_hay = nucleo::Utf32Str::new(entry.cmd.as_str(), &mut c_buf);
                    let c_score = pattern.score(c_hay, &mut matcher).unwrap_or(0);

                    let combined = (h_score.saturating_mul(2))
                        .saturating_add(t_score.saturating_mul(3))
                        .saturating_add(c_score);

                    if combined > 0 {
                        let bonus = heading_bonus(&heading_str)
                            .max(title_bonus(&entry.title))
                            .max(cmd_bonus(&entry.cmd));
                        scored.push((i, combined.saturating_add(bonus)));
                    }
                }
            }
        }

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        app.results = scored.into_iter().map(|(i, _)| i).collect();
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title_bottom(format!(" RESULTS  {} entries ", app.entries.len()))
        .border_style(Style::default().fg(C_BORDER));

    let inner_width = area.width.saturating_sub(2) as usize;
    let cmd_width = inner_width.saturating_sub(4);

    let items: Vec<ListItem> = app
        .results
        .iter()
        .filter_map(|&i| app.entries.get(i))
        .map(|e| {
            let breadcrumb = e.heading_path.join(" › ");

            let mut lines: Vec<Line> = Vec::new();

            for chunk in textwrap::wrap(&breadcrumb, inner_width.max(1)) {
                lines.push(Line::from(Span::styled(
                    chunk.into_owned(),
                    Style::default().fg(C_DIM),
                )));
            }

            let wrapped = textwrap::wrap(&e.cmd, cmd_width.max(1));
            for (idx, chunk) in wrapped.iter().enumerate() {
                let prefix = if idx == 0 { "  $ " } else { "    " };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(C_DIM)),
                    Span::styled(chunk.to_string(), Style::default().fg(C_FG_BRIGHT)),
                ]));
            }

            lines.push(Line::from(""));

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(C_HIGHLIGHT_BG)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, area, &mut app.list_state);
}
fn render_chain(frame: &mut Frame, area: Rect, chain_entries: &[&Entry], selected_entry_id: &str) {
    if chain_entries.is_empty() {
        let p = Paragraph::new("No chain for this command")
            .style(Style::default().fg(C_DIM))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(C_BORDER))
                    .title(" ATTACK CHAIN ")
                    .title_alignment(Alignment::Center),
            );

        frame.render_widget(p, area);
        return;
    };

    let lines: Vec<Line> = chain_entries
        .iter()
        .flat_map(|chain_entry| {
            let style = if selected_entry_id == chain_entry.id {
                Style::default()
                    .fg(C_FG_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(C_DIM)
            };
            vec![
                Line::from(""),
                Line::from(Span::styled(chain_entry.cmd.as_str(), style)),
                Line::from(""),
            ]
        })
        .collect();

    let chain_widget: Paragraph<'_> = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title_top(" ATTACK CHAIN ")
            .title_alignment(Alignment::Center)
            .border_style(Style::default().fg(C_BORDER)),
    );

    frame.render_widget(chain_widget, area);
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let selected = app.selected_entry();

    let Some(entry) = selected else {
        let p = Paragraph::new(vec![Line::from(""), Line::from("Select an entry")])
            .style(Style::default().fg(C_DIM))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(C_BORDER))
                    .title(" DESCRIPTION ")
                    .title_alignment(Alignment::Center),
            );

        frame.render_widget(p, area);
        return;
    };

    let lines_iter = entry
        .description
        .lines()
        .map(|l| Line::from(Span::styled(l, Style::default().fg(C_DESC))));

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            entry.title.as_str(),
            Style::default().fg(C_TITLE),
        )),
    ];

    lines.extend(lines_iter);

    let top = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(C_BORDER))
            .title(" DESCRIPTION ")
            .title_alignment(Alignment::Center),
    );
    frame.render_widget(top, area);
}
