use crate::app::MainLoopAction;
use crate::app_state::AppState;
use crate::config::{OrganLibrary, OrganProfile, load_organ_library, save_organ_library};
use crate::tui_filepicker::run_file_picker;
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use rust_i18n::t;
use std::sync::{Arc, Mutex};
use std::time::Instant;

type TuiTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

pub struct OrganManagerTuiState {
    library: OrganLibrary,
    list_state: ListState,
    pub learning_index: Option<usize>,
    pub last_interaction: Instant,
    // If Some, we are confirming removal of the organ at this index
    pub confirm_remove_index: Option<usize>,
}

impl Default for OrganManagerTuiState {
    fn default() -> Self {
        Self::new()
    }
}

impl OrganManagerTuiState {
    pub fn new() -> Self {
        let library = load_organ_library().unwrap_or_default();
        let mut list_state = ListState::default();
        if !library.organs.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            library,
            list_state,
            learning_index: None,
            last_interaction: Instant::now(),
            confirm_remove_index: None,
        }
    }

    pub fn navigation_up(&mut self) {
        if self.library.organs.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn navigation_down(&mut self) {
        if self.library.organs.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1).min(self.library.organs.len() - 1),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Handles input for the organ manager.
    /// Returns true if the TUI should exit the organ manager (e.g. Esc).
    /// Returns the MainLoopAction if an action requires it (e.g. ReloadOrgan).
    pub fn handle_input(
        &mut self,
        key: KeyCode,
        terminal: &mut TuiTerminal, // Needed for file picker
        exit_action: &Arc<Mutex<MainLoopAction>>,
        _app_state: &Arc<Mutex<AppState>>,
    ) -> bool {
        // If we are confirming removal
        if let Some(remove_idx) = self.confirm_remove_index {
            match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.library.organs.remove(remove_idx);
                    if let Err(e) = save_organ_library(&self.library) {
                        log::error!("Failed to save organ library: {}", e);
                    }
                    // Adjust selection
                    if self.library.organs.is_empty() {
                        self.list_state.select(None);
                    } else if let Some(sel) = self.list_state.selected() {
                        if sel >= self.library.organs.len() {
                            self.list_state.select(Some(self.library.organs.len() - 1));
                        }
                    }
                    self.confirm_remove_index = None;
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_remove_index = None;
                }
                _ => {}
            }
            return false;
        }

        // If we are learning, consume only Escape to cancel
        if self.learning_index.is_some() {
            if key == KeyCode::Esc {
                self.learning_index = None;
            }
            return false;
        }

        match key {
            KeyCode::Esc => return true, // Exit Manager
            KeyCode::Up => self.navigation_up(),
            KeyCode::Down => self.navigation_down(),

            // Load Organ
            KeyCode::Enter => {
                if let Some(idx) = self.list_state.selected() {
                    if let Some(organ) = self.library.organs.get(idx) {
                        // Trigger Reload
                        let mut action = exit_action.lock().unwrap();
                        *action = MainLoopAction::ReloadOrgan {
                            file: organ.path.clone(),
                        };
                        return true;
                    }
                }
            }

            // ADD Organ
            KeyCode::Char('a') => {
                match run_file_picker(
                    terminal,
                    &t!("organ_manager.add_organ"),
                    &["organ", "json", "xml", "Organ_Hauptwerk_xml"],
                ) {
                    Ok(Some(path)) => {
                        // Guess name
                        let name = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Unknown Organ".to_string());

                        self.library.organs.push(OrganProfile {
                            name,
                            path,
                            activation_trigger: None,
                        });
                        if let Err(e) = save_organ_library(&self.library) {
                            log::error!("Failed to save organ library: {}", e);
                        }
                        // Select the new one
                        self.list_state.select(Some(self.library.organs.len() - 1));
                    }
                    Ok(None) => {} // Cancelled
                    Err(e) => {
                        log::error!("File picker failed: {}", e);
                    }
                }
            }

            // Remove Organ
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(idx) = self.list_state.selected() {
                    self.confirm_remove_index = Some(idx);
                }
            }

            // Learn Trigger
            KeyCode::Char('i') => {
                if let Some(idx) = self.list_state.selected() {
                    self.learning_index = Some(idx);
                    self.last_interaction = Instant::now();
                }
            }

            // Clear Trigger
            KeyCode::Char('c') => {
                if let Some(idx) = self.list_state.selected() {
                    if let Some(organ) = self.library.organs.get_mut(idx) {
                        organ.activation_trigger = None;
                        if let Err(e) = save_organ_library(&self.library) {
                            log::error!("Failed to save organ library: {}", e);
                        }
                    }
                }
            }

            _ => {}
        }

        false
    }

    pub fn check_for_midi_input(&mut self, app_state: &Arc<Mutex<AppState>>) {
        if let Some(idx) = self.learning_index {
            let state = app_state.lock().unwrap();
            if let Some((event, time)) = &state.last_midi_event_received {
                if *time > self.last_interaction {
                    // Captured!
                    let event_clone = event.clone();
                    drop(state); // Drop lock before mutating self (though self isn't locked, but good practice)

                    if let Some(organ) = self.library.organs.get_mut(idx) {
                        organ.activation_trigger = Some(event_clone);
                        if let Err(e) = save_organ_library(&self.library) {
                            log::error!("Failed to save organ library: {}", e);
                        }
                    }
                    self.learning_index = None;
                }
            }
        }
    }
}

pub fn draw_organ_manager(
    frame: &mut Frame,
    state: &mut OrganManagerTuiState,
    current_organ_name: &str,
) {
    let area = centered_rect(frame.area(), 80, 80);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(" {} ", t!("organ_manager.title")))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(block.clone(), area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Min(5),    // List
            Constraint::Length(6), // Info/Help
        ])
        .split(area);

    // --- Organ List ---
    let items: Vec<ListItem> = state
        .library
        .organs
        .iter()
        .map(|organ| {
            let is_active = organ.name == current_organ_name;

            let marker = if is_active { ">> " } else { "   " };
            let name = &organ.name;
            let file = organ.path.file_name().unwrap_or_default().to_string_lossy();

            let trigger_str = organ
                .activation_trigger
                .as_ref()
                .map(|t| t.to_string())
                .unwrap_or_else(|| "-".to_string());

            let content = format!(
                "{} {} \n      File: {}\n      Trigger: {}",
                marker, name, file, trigger_str
            );

            ListItem::new(content)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("");

    frame.render_stateful_widget(list, layout[0], &mut state.list_state);

    // --- Footer / Status ---
    let footer_area = layout[1];
    let footer_text = if let Some(idx) = state.confirm_remove_index {
        let organ_name = &state.library.organs[idx].name;
        format!(
            "remove_confirm_fmt: Are you sure you want to remove '{}'? (y/n)",
            organ_name
        )
    } else if let Some(idx) = state.learning_index {
        let organ_name = &state.library.organs[idx].name;
        format!("{} '{}'...", t!("midi_learn.status_listening"), organ_name)
    } else {
        // Help
        format!(
            "Enter: {} | a: {} | d: Remove | i: {} | c: {} | Esc: Close",
            t!("organ_manager.load"),
            t!("organ_manager.add_organ"),
            t!("midi_learn.btn_learn"),
            t!("midi_learn.btn_clear")
        )
    };

    let p = Paragraph::new(footer_text)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(
            if state.learning_index.is_some() || state.confirm_remove_index.is_some() {
                Color::Yellow
            } else {
                Color::White
            },
        ))
        .block(Block::default().borders(Borders::TOP));

    frame.render_widget(p, footer_area);
}

fn centered_rect(r: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
