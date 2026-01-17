use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Clear, Row, Table, TableState},
};
use crossterm::event::KeyCode;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use crate::app_state::AppState;
use rust_i18n::t;

pub struct MidiLearnTuiState {
    pub target_stop_index: usize,
    pub target_stop_name: String,
    
    // Navigation
    pub row_idx: usize, // 0..15 (Internal Channels)
    pub col_idx: usize, // 0 = Enable, 1 = Disable, 2 = Clear
    
    // Logic
    pub learning_slot: Option<(u8, bool)>, // If Some, waiting for MIDI
    pub last_interaction: Instant,
}

impl Default for MidiLearnTuiState {
    fn default() -> Self {
        Self {
            target_stop_index: 0,
            target_stop_name: String::new(),
            row_idx: 0,
            col_idx: 0,
            learning_slot: None,
            last_interaction: Instant::now(),
        }
    }
}

impl MidiLearnTuiState {
    pub fn reset(&mut self, stop_index: usize, stop_name: String) {
        self.target_stop_index = stop_index;
        self.target_stop_name = stop_name;
        self.row_idx = 0;
        self.col_idx = 0;
        self.learning_slot = None;
        self.last_interaction = Instant::now();
    }

    pub fn handle_input(&mut self, key: KeyCode, app_state: &Arc<Mutex<AppState>>) -> bool {
        // Returns true if the modal should stay open, false to close
        
        // If we are learning, any key cancels learning (except maybe we want to allow aborting)
        // But for TUI, usually Enter confirms, Esc cancels.
        if self.learning_slot.is_some() {
             if key == KeyCode::Esc {
                 self.learning_slot = None;
                 return true; 
             }
             // For now, block navigation while learning
             return true; 
        }

        match key {
            KeyCode::Esc => return false, // Close modal
            KeyCode::Up => self.row_idx = self.row_idx.saturating_sub(1),
            KeyCode::Down => self.row_idx = (self.row_idx + 1).min(15),
            KeyCode::Left => self.col_idx = self.col_idx.saturating_sub(1),
            KeyCode::Right => self.col_idx = (self.col_idx + 1).min(2),
            KeyCode::Enter => {
                match self.col_idx {
                    0 => { // Learn Enable
                        self.learning_slot = Some((self.row_idx as u8, true));
                        self.last_interaction = Instant::now();
                    },
                    1 => { // Learn Disable
                        self.learning_slot = Some((self.row_idx as u8, false));
                        self.last_interaction = Instant::now();
                    },
                    2 => { // Clear
                        let mut state = app_state.lock().unwrap();
                        state.midi_control_map.clear(self.target_stop_index, self.row_idx as u8);
                        let _ = state.midi_control_map.save(&state.organ.name);
                    },
                    _ => {}
                }
            }
            _ => {}
        }
        true
    }

    pub fn check_for_midi_input(&mut self, app_state: &Arc<Mutex<AppState>>) {
        if let Some((target_internal, is_enable)) = self.learning_slot {
            let mut state = app_state.lock().unwrap();
            
            // Check if a new event arrived since we pressed Enter
            if let Some((event, time)) = &state.last_midi_event_received {
                if *time > self.last_interaction {
                    // Map it
                    let event_clone = event.clone();
                    
                    state.midi_control_map.learn(
                        self.target_stop_index,
                        target_internal,
                        event_clone.clone(),
                        is_enable
                    );
                    let _ = state.midi_control_map.save(&state.organ.name);
                    
                    // Reset
                    self.learning_slot = None;

                    let action_text = if is_enable { 
                        t!("midi_learn.action_enable") 
                    } else { 
                        t!("midi_learn.action_disable") 
                    };
                    
                    state.add_midi_log(
                        t!("midi_learn.log_mapped_fmt", event = event_clone, action = action_text).to_string()
                    );
                }
            }
        }
    }
}

pub fn draw_midi_learn_modal(frame: &mut Frame, tui_state: &MidiLearnTuiState, app_state: &AppState) {
    let area = centered_rect(frame.area(), 80, 80);
    
    frame.render_widget(Clear, area); // Clear background

    let title = format!(" {} ", t!("midi_learn.window_title_fmt", name = tui_state.target_stop_name));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    // Get Data
    let control_map = app_state.midi_control_map.stops.get(&tui_state.target_stop_index).cloned().unwrap_or_default();

    // Prepare Rows
    let rows: Vec<Row> = (0..16u8).map(|channel| {
        let config = control_map.get(&channel);
        
        // Clone inner values because MidiEventSpec is not Copy
        let enable_evt = config.and_then(|c| c.enable_event.clone());
        let disable_evt = config.and_then(|c| c.disable_event.clone());

        // Determine styling for cells based on selection
        let is_row_selected = channel as usize == tui_state.row_idx;
        
        let get_style = |col_idx: usize| {
            if is_row_selected && tui_state.col_idx == col_idx {
                if tui_state.learning_slot.is_some() {
                     Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK)
                } else {
                     Style::default().fg(Color::Black).bg(Color::Cyan)
                }
            } else {
                Style::default()
            }
        };

        // Cell 0: Label
        let label = t!("tui_midi_learn.fmt_ch_short", num = channel + 1).to_string();
        
        // Cell 1: Enable
        let enable_text = if tui_state.learning_slot == Some((channel, true)) {
            t!("midi_learn.status_listening").to_string()
        } else if let Some(evt) = enable_evt {
            evt.to_string()
        } else {
            t!("tui_midi_learn.cell_dash").to_string()
        };

        // Cell 2: Disable
        let disable_text = if tui_state.learning_slot == Some((channel, false)) {
            t!("midi_learn.status_listening").to_string()
        } else if let Some(evt) = disable_evt {
            evt.to_string()
        } else {
            t!("tui_midi_learn.cell_dash").to_string()
        };

        // Cell 3: Clear
        let clear_text = t!("midi_learn.btn_clear").to_string();

        Row::new(vec![
            Cell::from(label),
            Cell::from(enable_text).style(get_style(0)),
            Cell::from(disable_text).style(get_style(1)),
            Cell::from(clear_text).style(get_style(2)),
        ])
    }).collect();

    let widths = [
        Constraint::Length(10),
        Constraint::Percentage(35),
        Constraint::Percentage(35),
        Constraint::Length(10),
    ];

    let header_row = Row::new(vec![
        t!("tui_midi_learn.hdr_internal").to_string(), 
        t!("midi_learn.col_enable_event").to_string(), 
        t!("midi_learn.col_disable_event").to_string(), 
        t!("tui_midi_learn.hdr_action").to_string()
    ]).style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow));

    let table = Table::new(rows, widths)
        .block(block)
        .header(header_row)
        .column_spacing(1);
    
    // We need to handle scrolling if the list is long, but 16 items usually fits.
    // However, to ensure the selected item is visible if the screen is small:
    let mut table_state = TableState::default();
    table_state.select(Some(tui_state.row_idx));

    frame.render_stateful_widget(table, area, &mut table_state);
    
    // Draw help footer inside modal
    let help_text = t!("tui_midi_learn.footer_help").to_string();
    let help_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area)[1];
        
    frame.render_widget(
        ratatui::widgets::Paragraph::new(help_text).alignment(Alignment::Center).style(Style::default().fg(Color::DarkGray)),
        help_area
    );
}

// Reuse the helper from tui.rs if possible, or duplicate/move to a shared util
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