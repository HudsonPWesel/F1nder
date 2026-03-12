const SEARCH_HEIGHT: u16 = 3;
const SEARCH_HEIGHT_MIN: u16 = 1;
use arboard::Clipboard;
use base64::{Engine, engine::general_purpose};
use color_eyre::Result;
use crossterm::{event::KeyModifiers, terminal};
use nucleo::{self, Utf32Str};
use nucleo_matcher;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout, Position},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, List, ListDirection, ListItem, ListState, Paragraph},
};
use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::{array, fs::File};
use std::{
    env::{self, consts::OS},
    io::BufRead,
};
use std::{io::BufReader, os::unix::raw::mode_t};
use strum_macros::{Display, EnumIter, EnumString};

fn main() -> Result<()> {
    let file = File::open("out.json")?;
    // let file = File::open("/home/p1erce/PentestingTools/F1nder/cmds.json")?;
    let reader = BufReader::new(file);
    let data: Value = serde_json::from_reader(reader)?;
    let entries = data["entries"].as_array().expect("Entries should be Arrs");
    let entries: Vec<Entry> = entries
        .iter()
        .map(|e| Entry {
            cmd: e["cmd"].as_str().unwrap_or("").to_string(),
            desc: e["desc"].as_str().unwrap_or("").to_string(),
            heading: e["heading"].as_str().unwrap_or("").to_string(),
        })
        .collect();

    color_eyre::install()?;
    let terminal = ratatui::init();
    let app_result = App::new(entries).run(terminal);
    ratatui::restore();
    app_result
}
struct Entry {
    cmd: String,
    desc: String,
    heading: String,
}

#[derive(Display, EnumString, EnumIter)]
enum SearchMode {
    Cmd,
    Desc,
    Heading,
    All,
}

struct App {
    /// Current value of the input box
    input: String,
    /// Position of cursor in the editor area.
    clipboard: Clipboard,
    character_index: usize,
    selected: usize,
    entries: Vec<Entry>,
    aliases: Vec<String>,
    search_mode: SearchMode, // Current input mode
                             //input_mode: InputMode,
                             // History of recorded messages
                             // messages: Vec<String>,
}

impl App {
    fn new(entries: Vec<Entry>) -> Self {
        Self {
            input: String::new(),
            clipboard: Clipboard::new().expect("Failed to init clipboard"),
            character_index: 1,
            selected: 0,
            entries,
            aliases: get_aliases(),
            search_mode: SearchMode::All,
        }
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame: &mut Frame<'_>| {
                self.draw(frame);
            })?;

            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('[') => {
                            self.search_mode = match self.search_mode {
                                SearchMode::All => SearchMode::Heading,
                                SearchMode::Heading => SearchMode::Desc,
                                SearchMode::Desc => SearchMode::Cmd,
                                SearchMode::Cmd => SearchMode::All,
                            };
                            self.selected = 0;
                        }
                        KeyCode::Char(']') => {
                            self.search_mode = match self.search_mode {
                                SearchMode::All => SearchMode::Cmd,
                                SearchMode::Cmd => SearchMode::Desc,
                                SearchMode::Desc => SearchMode::Heading,
                                SearchMode::Heading => SearchMode::All,
                            };
                            self.selected = 0;
                        }
                        KeyCode::Char(c) => {
                            if c == 'e' && key.modifiers.contains(KeyModifiers::CONTROL) {
                                ratatui::restore();

                                let filtered_entries = self.get_filtered_entries();
                                let old_cmd = filtered_entries[self.selected].cmd.clone();

                                let _ = fs::write("tempfile.txt", &old_cmd);

                                std::process::Command::new("nvim")
                                    .arg("tempfile.txt")
                                    .status()
                                    .expect("Failed to launch editor");

                                let new_cmd =
                                    fs::read_to_string("tempfile.txt")?.trim().to_string();

                                if let Some(entry) =
                                    self.entries.iter_mut().find(|e| e.cmd == old_cmd)
                                {
                                    entry.cmd = new_cmd;
                                }
                                self.save_entries()?;

                                terminal = ratatui::init();
                            } else if c == 'a' && key.modifiers.contains(KeyModifiers::CONTROL) {
                                ratatui::restore();

                                let _ = fs::write(
                                    "tempfile.txt",
                                    "command here\n---\ndescription here\n---\nHeading > Subheading",
                                )?;

                                std::process::Command::new("nvim")
                                    .arg("tempfile.txt")
                                    .status()
                                    .expect("Failed to launch editor");

                                let content = fs::read_to_string("tempfile.txt")?;

                                let parts: Vec<&str> = content.split("---").collect();
                                let new_entry = Entry {
                                    cmd: parts.get(0).unwrap_or(&"").trim().to_string(),
                                    desc: parts.get(1).unwrap_or(&"").trim().to_string(),
                                    heading: parts.get(2).unwrap_or(&"").trim().to_string(),
                                };

                                self.entries.push(new_entry);
                                self.save_entries()?;
                                terminal = ratatui::init();
                            }
                            if c == 'd' && key.modifiers.contains(KeyModifiers::CONTROL) {
                                ratatui::restore();

                                let filtered_entries = self.get_filtered_entries();
                                let old_cmd = filtered_entries[self.selected].cmd.clone();
                                self.entries.retain(|e| e.cmd != old_cmd);

                                self.save_entries()?;

                                terminal = ratatui::init();
                            } else {
                                self.input.push(c);
                                self.selected = 0;
                                self.move_cursor_right();
                            }
                        }
                        KeyCode::Backspace => {
                            if self.character_index != 1 {
                                self.input.remove(self.character_index - 2);
                                self.selected = 0;
                                self.move_cursor_left();
                            }
                        }
                        KeyCode::Esc => return Ok(()),
                        KeyCode::Up => {
                            self.selected = self.selected.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            self.selected = (self.selected + 1)
                                .min(self.get_filtered_entries().len().saturating_sub(1));
                        }
                        KeyCode::Enter => {
                            let cmd = self
                                .get_filtered_entries()
                                .get(self.selected)
                                .map(|e| e.cmd.clone());
                            if let Some(text) = cmd {
                                if OS == "macos" {
                                    copy_osc52(&text);
                                } else if OS == "linux" {
                                    copy_to_linux_clipboard(&text);
                                }
                                return Ok(());
                            }
                        }
                        KeyCode::Left => self.move_cursor_left(),
                        KeyCode::Right => self.move_cursor_right(),
                        _ => {}
                    }
                }
            }
        }
    }

    fn draw(&self, frame: &mut Frame) -> Result<(), Box<dyn std::error::Error>> {
        frame.set_cursor_position(Position::new(self.character_index.try_into().unwrap(), 1));
        let chunks = Layout::vertical([
            Constraint::Length(SEARCH_HEIGHT),
            Constraint::Min(SEARCH_HEIGHT_MIN),
        ])
        .areas(frame.area());

        let [input_area, content_area] = chunks;

        let [list_area, desc_area] =
            Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)])
                .areas(content_area);

        let [description_area, environment_area] =
            Layout::vertical([Constraint::Percentage(30), Constraint::Percentage(70)])
                .areas(desc_area);

        let input = Paragraph::new(self.input.as_str())
            .block(Block::bordered().title(format!("F1nder [{}]", self.search_mode.to_string())));

        let cmd_strings: Vec<String> = self
            .get_filtered_entries()
            .iter()
            .map(|e| e.cmd.clone())
            .collect();
        let alias_strings: Vec<String> = self
            .get_filtered_aliases()
            .iter()
            .map(|a| a.to_string())
            .collect();

        let cmd_list = App::build_list(&cmd_strings, "Commands");
        let alias_list = App::build_list(&alias_strings, "Aliases");

        let mut cmd_state = ListState::default();
        cmd_state.select(Some(self.selected));

        let mut alias_state = ListState::default();
        alias_state.select(Some(self.selected));

        let desc_text = self
            .get_filtered_entries()
            .get(self.selected)
            .map(|e| e.desc.as_str())
            .unwrap_or("");

        let desc_widget = Paragraph::new(desc_text)
            .block(Block::bordered().title("Description"))
            .wrap(ratatui::widgets::Wrap { trim: true });

        frame.render_widget(input, input_area);
        frame.render_stateful_widget(cmd_list, list_area, &mut cmd_state);
        frame.render_stateful_widget(alias_list, environment_area, &mut alias_state);
        frame.render_widget(desc_widget, desc_area);
        // frame.render_widget(env_widget, environment_area);
        Ok(())
    }

    fn build_list<'a>(items: &'a [String], title: &'a str) -> List<'a> {
        let list_items: Vec<ListItem> = items.iter().map(|s| ListItem::new(s.as_str())).collect();
        List::new(list_items)
            .block(Block::bordered().title(title))
            .highlight_style(Style::new().reversed())
    }

    fn get_filtered_entries(&self) -> Vec<&Entry> {
        if self.input.is_empty() {
            return self.entries.iter().collect();
        }
        let config = nucleo::Config::DEFAULT;
        let mut matcher = nucleo::Matcher::new(config);
        let pattern = nucleo_matcher::pattern::Pattern::parse(
            &self.input,
            nucleo_matcher::pattern::CaseMatching::Ignore,
            nucleo_matcher::pattern::Normalization::Smart,
        );

        let mut scored: Vec<(u32, &Entry)> = self
            .entries
            .iter()
            .filter_map(|e| {
                let fields: Vec<&String> = match self.search_mode {
                    SearchMode::Cmd => vec![&e.cmd],
                    SearchMode::Desc => vec![&e.desc],
                    SearchMode::Heading => vec![&e.heading],
                    SearchMode::All => vec![&e.cmd, &e.desc, &e.heading],
                };
                fields
                    .iter()
                    .filter_map(|f| self.fuzzy_score(f, &pattern, &mut matcher))
                    .max()
                    .map(|score| (score, e))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.iter().map(|(_, e)| *e).collect()
    }

    fn get_filtered_aliases(&self) -> Vec<&String> {
        if self.input.is_empty() {
            return self.aliases.iter().collect();
        }
        let config = nucleo::Config::DEFAULT;
        let mut matcher = nucleo::Matcher::new(config);
        let pattern = nucleo_matcher::pattern::Pattern::parse(
            &self.input,
            nucleo_matcher::pattern::CaseMatching::Ignore,
            nucleo_matcher::pattern::Normalization::Smart,
        );

        let mut scored: Vec<(u32, &String)> = self
            .aliases
            .iter()
            .filter_map(|a| {
                self.fuzzy_score(a, &pattern, &mut matcher)
                    .map(|score| (score, a))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.iter().map(|(_, a)| *a).collect()
    }
    fn fuzzy_score(
        &self,
        text: &str,
        pattern: &nucleo_matcher::pattern::Pattern,
        matcher: &mut nucleo::Matcher,
    ) -> Option<u32> {
        let mut buf = Vec::new();
        let haystack = Utf32Str::new(text, &mut buf);
        pattern.score(haystack, matcher)
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.input.chars().count() + 1)
    }
    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.character_index.saturating_sub(1);
        self.character_index = self.clamp_cursor(cursor_moved_left);
    }

    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.character_index.saturating_add(1);
        self.character_index = self.clamp_cursor(cursor_moved_right);
    }

    fn save_entries(&self) -> Result<()> {
        let output = serde_json::json!({
            "source": "combined",
            "entries": self.entries.iter().map(|e| {
                serde_json::json!({
                    "cmd": e.cmd,
                    "desc": e.desc,
                    "heading": e.heading,
                })
            }).collect::<Vec<_>>()
        });
        fs::write("out.json", serde_json::to_string_pretty(&output)?)?;
        Ok(())
    }
}

fn get_aliases() -> Vec<String> {
    let file = File::open("/Users/wyld7k/.zshrc").expect("Failed to laod file zshrc");
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .filter_map(|line| line.ok())
        .filter(|line| line.starts_with("alias"))
        .collect();
    lines
}
fn copy_osc52(text: &str) {
    let encoded = general_purpose::STANDARD.encode(text);
    // \x1b]52;c; is the escape sequence for the system clipboard
    let sequence = format!("\x1b]52;c;{}\x07", encoded);
    let mut stderr = io::stderr(); // Using stderr is often safer in TUIs to avoid rendering artifacts
    let _ = stderr.write_all(sequence.as_bytes());
    let _ = stderr.flush();

    // Crucial: Give the terminal a split second to ingest the sequence
    std::thread::sleep(std::time::Duration::from_millis(50));
}

use std::process::{Command, Stdio};

fn copy_to_linux_clipboard(text: &str) {
    {
        use std::process::Command;
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "echo -n '{}' | nohup xclip -selection clipboard >/dev/null 2>&1 &",
                text
            ))
            .spawn();
    }
}
