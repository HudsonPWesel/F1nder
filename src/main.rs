const SEARCH_HEIGHT: u16 = 3;
const SEARCH_HEIGHT_MIN: u16 = 1;
use arboard::Clipboard;
use color_eyre::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout, Position},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, List, ListDirection, ListItem, ListState, Paragraph},
};
use serde_json::Value;
use std::io::BufReader;
use std::{array, fs::File};

fn main() -> Result<()> {
    let file = File::open("cmds.json")?;
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
struct App {
    /// Current value of the input box
    input: String,
    /// Position of cursor in the editor area.
    clipboard: Clipboard,
    character_index: usize,
    selected: usize,
    entries: Vec<Entry>,
    // Current input mode
    //input_mode: InputMode,
    // History of recorded messages
    // messages: Vec<String>,
}

impl App {
    fn new(entries: Vec<Entry>) -> Self {
        Self {
            input: String::new(),
            clipboard: Clipboard::new().expect("Failed to init clipboard"),
            character_index: 0,
            selected: 0,
            entries,
        }
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char(c) => self.input.push(c),
                        KeyCode::Backspace => {
                            self.input.pop();
                        }
                        KeyCode::Esc => return Ok(()),
                        KeyCode::Up => {
                            self.selected = self.selected.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            self.selected =
                                (self.selected + 1).min(self.filtered_count().saturating_sub(1));
                        }
                        KeyCode::Enter => {
                            let cmd = self
                                .get_filtered()
                                .get(self.selected)
                                .map(|e| e.cmd.clone());
                            if let Some(text) = cmd {
                                self.clipboard.set_text(&text).ok();
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(SEARCH_HEIGHT),
            Constraint::Min(SEARCH_HEIGHT_MIN),
        ])
        .areas(frame.area());

        let [input_area, results_area] = chunks;

        let input = Paragraph::new(self.input.as_str()).block(Block::bordered().title("F1nder"));

        let filtered: Vec<&Entry> = self.get_filtered();

        let items: Vec<ListItem> = filtered
            .iter()
            .map(|e| ListItem::new(format!("{} - {}", e.cmd, e.desc)))
            .collect();

        let list = List::new(items)
            .block(Block::bordered().title("Commands"))
            .highlight_style(Style::new().reversed());

        let mut state = ListState::default();
        state.select(Some(self.selected));

        frame.render_widget(input, input_area);
        frame.render_stateful_widget(list, results_area, &mut state);
        // frame.render_widget(list, results_area);
    }

    fn get_filtered(&self) -> Vec<&Entry> {
        let query = self.input.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                query.is_empty() || e.cmd.to_lowercase().contains(&query)
                // || e.desc.to_lowercase().contains(&query)
                // || e.heading.to_lowercase().contains(&query)
            })
            .collect()
    }

    fn filtered_count(&self) -> usize {
        let query = self.input.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                query.is_empty() || e.cmd.to_lowercase().contains(&query)
                // || e.desc.to_lowercase().contains(&query)
                // || e.heading.to_lowercase().contains(&query)
            })
            .count()
    }
}
