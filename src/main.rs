const SEARCH_HEIGHT: u16 = 3;
const SEARCH_HEIGHT_MIN: u16 = 1;
use arboard::Clipboard;
use color_eyre::Result;
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
                        KeyCode::Char(c) => {
                            self.input.push(c);
                            self.selected = 0;
                        }
                        KeyCode::Backspace => {
                            self.input.pop();
                            self.selected = 0;
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

        let [input_area, content_area] = chunks;

        let [list_area, desc_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(content_area);

        let input = Paragraph::new(self.input.as_str()).block(Block::bordered().title("F1nder"));

        let filtered: Vec<&Entry> = self.get_filtered();

        let items: Vec<ListItem> = filtered
            .iter()
            .map(|e| ListItem::new(format!("{}", e.cmd)))
            .collect();

        let list = List::new(items)
            .block(Block::bordered().title("Commands"))
            .highlight_style(Style::new().reversed());

        let mut state = ListState::default();
        state.select(Some(self.selected));

        let desc_text = filtered
            .get(self.selected)
            .map(|e| e.desc.as_str())
            .unwrap_or("");

        let desc = Paragraph::new(desc_text)
            .block(Block::bordered().title("Description"))
            .wrap(ratatui::widgets::Wrap { trim: true });

        frame.render_widget(input, input_area);
        frame.render_stateful_widget(list, list_area, &mut state);
        frame.render_widget(desc, desc_area);
    }

    fn get_filtered(&self) -> Vec<&Entry> {
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
                let mut buf = Vec::new();
                let haystack = nucleo_matcher::Utf32Str::new(&e.cmd, &mut buf);
                pattern
                    .score(haystack, &mut matcher)
                    .map(|score| (score, e))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.iter().map(|(_, e)| *e).collect()

        // self.entries
        //     .iter()
        //     .filter(|e| {
        //         query.is_empty() || e.cmd.to_lowercase().contains(&query)
        //         // || e.desc.to_lowercase().contains(&query)
        //         // || e.heading.to_lowercase().contains(&query)
        //     })
        //     .collect()
    }

    fn filtered_count(&self) -> usize {
        self.get_filtered().len()
    }
}
