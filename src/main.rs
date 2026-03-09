const SEARCH_HEIGHT: u16 = 3;
const SEARCH_HEIGHT_MIN: u16 = 1;
use color_eyre::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout, Position},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, List, ListItem, Paragraph},
};
use serde_json::Value;
use std::fs::File;
use std::io::BufReader;

fn main() -> Result<()> {
    let file = File::open("cmds.json")?;
    let reader = BufReader::new(file);
    let data: Value = serde_json::from_reader(reader)?;
    let entries = data["entries"].as_array();

    println!("{:#?}", entries);
    color_eyre::install()?;
    let terminal = ratatui::init();
    let app_result = App::new().run(terminal);
    ratatui::restore();
    app_result
}

struct App {
    /// Current value of the input box
    input: String,
    /// Position of cursor in the editor area.
    character_index: usize,
    // Current input mode
    //input_mode: InputMode,
    // History of recorded messages
    // messages: Vec<String>,
}

impl App {
    fn new() -> Self {
        Self {
            input: String::new(),
            character_index: 0,
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

        frame.render_widget(input, input_area);
    }
}
