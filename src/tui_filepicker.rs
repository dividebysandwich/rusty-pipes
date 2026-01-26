use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use rust_i18n::t;
use std::{
    fs,
    io::Stdout,
    path::{Path, PathBuf},
    time::Duration,
};

// Define the terminal type alias for convenience
type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Holds state for the TUI file picker.
struct TuiFilePickerState<'a> {
    current_path: PathBuf,
    entries: Vec<PathBuf>,
    list_state: ListState,
    error_msg: Option<String>,
    allowed_extensions: &'a [&'a str],
}

impl<'a> TuiFilePickerState<'a> {
    fn new(allowed_extensions: &'a [&'a str]) -> Result<Self> {
        let current_path = std::env::current_dir()?;
        let mut state = Self {
            current_path,
            entries: Vec::new(),
            list_state: ListState::default(),
            error_msg: None,
            allowed_extensions,
        };
        state.load_entries()?;
        Ok(state)
    }

    fn is_allowed_file(&self, path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        if self.allowed_extensions.is_empty() {
            return true;
        }

        let ext = path.extension().and_then(|s| s.to_str());
        if let Some(ext) = ext {
            return self.allowed_extensions.contains(&ext);
        }
        false
    }

    fn load_entries(&mut self) -> Result<()> {
        self.entries.clear();
        self.list_state.select(None);
        self.error_msg = None;

        match fs::read_dir(&self.current_path) {
            Ok(entries) => {
                let mut paths: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() || self.is_allowed_file(p))
                    .collect();

                paths.sort_by(|a, b| {
                    if a.is_dir() && !b.is_dir() {
                        std::cmp::Ordering::Less
                    } else if !a.is_dir() && b.is_dir() {
                        std::cmp::Ordering::Greater
                    } else {
                        a.file_name().cmp(&b.file_name())
                    }
                });

                self.entries = paths;

                if !self.entries.is_empty() {
                    self.list_state.select(Some(0));
                }
            }
            Err(e) => {
                self.error_msg = Some(t!("tui_picker.err_read_dir", err = e).to_string());
            }
        }
        Ok(())
    }

    fn get_selected_path(&self) -> Option<&PathBuf> {
        self.list_state.selected().and_then(|i| self.entries.get(i))
    }

    fn next_item(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| (i + 1) % self.entries.len());
        self.list_state.select(Some(i));
    }

    fn prev_item(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len();
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| (i + len - 1) % len);
        self.list_state.select(Some(i));
    }

    fn activate_selected(&mut self) -> Result<Option<PathBuf>> {
        if let Some(path) = self.get_selected_path().cloned() {
            if path.is_dir() {
                self.current_path = path;
                self.load_entries()?;
            } else if self.is_allowed_file(&path) {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    fn go_up(&mut self) -> Result<()> {
        if let Some(parent) = self.current_path.parent() {
            self.current_path = parent.to_path_buf();
            self.load_entries()?;
        }
        Ok(())
    }
}

/// Runs a TUI loop to browse for a file.
/// Returns the path if selected, or None if the user quits.
pub fn run_file_picker(
    terminal: &mut TuiTerminal,
    title: &str,
    allowed_extensions: &[&str],
) -> Result<Option<PathBuf>> {
    let mut state = TuiFilePickerState::new(allowed_extensions)?;

    let result: Option<PathBuf> = loop {
        terminal.draw(|f| draw_file_picker_ui(f, &mut state, title))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break None,
                        KeyCode::Down | KeyCode::Char('j') => state.next_item(),
                        KeyCode::Up | KeyCode::Char('k') => state.prev_item(),
                        KeyCode::PageDown => {
                            for _ in 0..5 {
                                state.next_item();
                            }
                        }
                        KeyCode::PageUp => {
                            for _ in 0..5 {
                                state.prev_item();
                            }
                        }
                        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                            if let Err(e) = state.go_up() {
                                state.error_msg =
                                    Some(t!("tui_picker.err_generic", err = e).to_string());
                            }
                        }
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                            match state.activate_selected() {
                                Ok(Some(file_path)) => break Some(file_path),
                                Ok(None) => {}
                                Err(e) => {
                                    state.error_msg =
                                        Some(t!("tui_picker.err_generic", err = e).to_string())
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    };

    Ok(result)
}

/// Renders the File Picker UI.
fn draw_file_picker_ui(frame: &mut Frame, state: &mut TuiFilePickerState, title: &str) {
    // This clears the screen, removing the config menu from underneath
    frame.render_widget(Clear, frame.area());

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // File list
            Constraint::Length(1), // Footer
            Constraint::Length(1), // Error
        ])
        .split(frame.area());

    // Header
    let header_block = Block::default()
        .borders(Borders::ALL)
        .title(t!("tui_picker.header_title_fmt", title = title).to_string());
    let header_text = Paragraph::new(
        t!(
            "tui_picker.current_path_fmt",
            path = state.current_path.display()
        )
        .to_string(),
    )
    .block(header_block);
    frame.render_widget(header_text, layout[0]);

    // File List
    let items: Vec<ListItem> = state
        .entries
        .iter()
        .map(|path| {
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
            let line = if path.is_dir() {
                Line::styled(
                    format!("[{}/]", file_name),
                    Style::default().fg(Color::Cyan),
                )
            } else {
                Line::from(file_name.into_owned())
            };
            ListItem::new(line)
        })
        .collect();

    let list_widget = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(t!("tui_picker.entries_title")),
        )
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("» ");

    frame.render_stateful_widget(list_widget, layout[1], &mut state.list_state);

    // Footer
    let footer_text = t!("tui_picker.footer_nav").to_string();
    frame.render_widget(
        Paragraph::new(footer_text).alignment(Alignment::Center),
        layout[2],
    );

    // Error
    if let Some(err) = &state.error_msg {
        frame.render_widget(
            Paragraph::new(err.as_str()).style(Style::default().fg(Color::Red)),
            layout[3],
        );
    }
}
