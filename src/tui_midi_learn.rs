use crate::app_state::AppState;
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Clear, Row, Table, TableState},
};
use rust_i18n::t;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// --- Added LearnTarget Enum (same as gui_midi_learn.rs) ---
#[derive(Clone, PartialEq, Debug)]
pub enum LearnTarget {
    Stop(usize),
    Tremulant(String),
    #[allow(dead_code)]
    Preset(usize),
}

impl Default for LearnTarget {
    fn default() -> Self {
        LearnTarget::Stop(0)
    }
}

pub struct MidiLearnTuiState {
    pub target: LearnTarget, // Changed from target_stop_index
    pub target_name: String,

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
            target: LearnTarget::default(),
            target_name: String::new(),
            row_idx: 0,
            col_idx: 0,
            learning_slot: None,
            last_interaction: Instant::now(),
        }
    }
}

impl MidiLearnTuiState {
    pub fn reset_stop(&mut self, stop_index: usize, stop_name: String) {
        self.target = LearnTarget::Stop(stop_index);
        self.target_name = stop_name;
        self.common_reset();
    }

    #[allow(dead_code)]
    pub fn reset_tremulant(&mut self, trem_id: String, trem_name: String) {
        self.target = LearnTarget::Tremulant(trem_id);
        self.target_name = trem_name;
        self.common_reset();
    }

    #[allow(dead_code)]
    pub fn reset_preset(&mut self, slot: usize) {
        self.target = LearnTarget::Preset(slot);
        self.target_name = format!("Preset F{}", slot + 1);
        self.common_reset();
    }

    fn common_reset(&mut self) {
        self.row_idx = 0;
        self.col_idx = 0;
        self.learning_slot = None;
        self.last_interaction = Instant::now();
    }

    pub fn handle_input(&mut self, key: KeyCode, app_state: &Arc<Mutex<AppState>>) -> bool {
        // If learning, block everything except Esc
        if self.learning_slot.is_some() {
            if key == KeyCode::Esc {
                self.learning_slot = None;
                return true;
            }
            return true;
        }

        match key {
            KeyCode::Esc => return false, // Close modal

            // Navigation depends on target type
            KeyCode::Up => {
                if let LearnTarget::Stop(_) = self.target {
                    self.row_idx = self.row_idx.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let LearnTarget::Stop(_) = self.target {
                    self.row_idx = (self.row_idx + 1).min(15);
                }
            }
            KeyCode::Left => self.col_idx = self.col_idx.saturating_sub(1),
            KeyCode::Right => self.col_idx = (self.col_idx + 1).min(2),

            KeyCode::Enter => {
                // For Tremulants, we only have one "row" (index 0)
                let effective_row = if let LearnTarget::Tremulant(_) = self.target {
                    0
                } else {
                    self.row_idx
                };

                match self.col_idx {
                    0 => {
                        // Learn Enable
                        self.learning_slot = Some((effective_row as u8, true));
                        self.last_interaction = Instant::now();
                    }
                    1 => {
                        // Learn Disable
                        self.learning_slot = Some((effective_row as u8, false));
                        self.last_interaction = Instant::now();
                    }
                    2 => {
                        // Clear
                        let mut state = app_state.lock().unwrap();
                        match &self.target {
                            LearnTarget::Stop(idx) => {
                                state.midi_control_map.clear_stop(*idx, effective_row as u8);
                            }
                            LearnTarget::Tremulant(id) => {
                                state.midi_control_map.clear_tremulant(id);
                            }
                            LearnTarget::Preset(slot) => {
                                state.midi_control_map.clear_preset(*slot);
                            }
                        }
                        let _ = state.midi_control_map.save(&state.organ.name);
                    }
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

            if let Some((event, time)) = &state.last_midi_event_received {
                if *time > self.last_interaction {
                    let event_clone = event.clone();

                    match &self.target {
                        LearnTarget::Stop(idx) => {
                            state.midi_control_map.learn_stop(
                                *idx,
                                target_internal,
                                event_clone.clone(),
                                is_enable,
                            );
                        }
                        LearnTarget::Tremulant(id) => {
                            state.midi_control_map.learn_tremulant(
                                id.clone(),
                                event_clone.clone(),
                                is_enable,
                            );
                        }
                        LearnTarget::Preset(slot) => {
                            if is_enable {
                                state
                                    .midi_control_map
                                    .learn_preset(*slot, event_clone.clone());
                            }
                        }
                    }

                    let _ = state.midi_control_map.save(&state.organ.name);
                    self.learning_slot = None;

                    let action_text = if is_enable {
                        t!("midi_learn.action_enable")
                    } else {
                        t!("midi_learn.action_disable")
                    };

                    state.add_midi_log(
                        t!(
                            "midi_learn.log_mapped_fmt",
                            event = event_clone,
                            action = action_text
                        )
                        .to_string(),
                    );
                }
            }
        }
    }
}

pub fn draw_midi_learn_modal(
    frame: &mut Frame,
    tui_state: &MidiLearnTuiState,
    app_state: &AppState,
) {
    let area = centered_rect(frame.area(), 80, 80);
    frame.render_widget(Clear, area);

    let title = format!(
        " {} ",
        t!("midi_learn.window_title_fmt", name = tui_state.target_name)
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    // Determine rows based on target type
    let rows: Vec<Row> = match &tui_state.target {
        LearnTarget::Stop(idx) => {
            let control_map = app_state
                .midi_control_map
                .stops
                .get(idx)
                .cloned()
                .unwrap_or_default();
            (0..16u8)
                .map(|channel| {
                    let config = control_map.get(&channel);
                    let enable_evt = config.and_then(|c| c.enable_event.clone());
                    let disable_evt = config.and_then(|c| c.disable_event.clone());
                    let label = t!("tui_midi_learn.fmt_ch_short", num = channel + 1).to_string();
                    build_row(channel as usize, label, enable_evt, disable_evt, tui_state)
                })
                .collect()
        }
        LearnTarget::Tremulant(id) => {
            let control = app_state
                .midi_control_map
                .tremulants
                .get(id)
                .cloned()
                .unwrap_or_default();
            let label = "Tremulant".to_string();
            // Single row, index 0
            vec![build_row(
                0,
                label,
                control.enable_event.clone(),
                control.disable_event.clone(),
                tui_state,
            )]
        }
        LearnTarget::Preset(slot) => {
            let trigger = app_state
                .midi_control_map
                .presets
                .get(slot)
                .cloned()
                .flatten();
            let label = "Preset".to_string();
            // Single row, Enable column is used for Trigger, Disable is N/A
            vec![build_row(0, label, trigger, None, tui_state)]
        }
    };

    let widths = [
        Constraint::Length(15),
        Constraint::Percentage(30),
        Constraint::Percentage(30),
        Constraint::Length(10),
    ];

    let header_row = Row::new(vec![
        t!("tui_midi_learn.hdr_internal").to_string(),
        t!("midi_learn.col_enable_event").to_string(),
        t!("midi_learn.col_disable_event").to_string(),
        t!("tui_midi_learn.hdr_action").to_string(),
    ])
    .style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Yellow),
    );

    let table = Table::new(rows, widths)
        .block(block)
        .header(header_row)
        .column_spacing(1);

    let mut table_state = TableState::default();
    table_state.select(Some(tui_state.row_idx));

    frame.render_stateful_widget(table, area, &mut table_state);

    let help_text = t!("tui_midi_learn.footer_help").to_string();
    let help_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area)[1];

    frame.render_widget(
        ratatui::widgets::Paragraph::new(help_text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray)),
        help_area,
    );
}

fn build_row(
    row_idx: usize,
    label: String,
    enable_evt: Option<crate::config::MidiEventSpec>,
    disable_evt: Option<crate::config::MidiEventSpec>,
    tui_state: &MidiLearnTuiState,
) -> Row<'static> {
    let get_style = |col_idx: usize| {
        if row_idx == tui_state.row_idx && tui_state.col_idx == col_idx {
            if tui_state.learning_slot.is_some() {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK)
            } else {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            }
        } else {
            Style::default()
        }
    };

    // Cell 1: Enable
    let enable_text = if tui_state.learning_slot == Some((row_idx as u8, true)) {
        t!("midi_learn.status_listening").to_string()
    } else if let Some(evt) = enable_evt {
        evt.to_string()
    } else {
        t!("tui_midi_learn.cell_dash").to_string()
    };

    // Cell 2: Disable
    let disable_text = if tui_state.learning_slot == Some((row_idx as u8, false)) {
        t!("midi_learn.status_listening").to_string()
    } else if let Some(evt) = disable_evt {
        evt.to_string()
    } else {
        t!("tui_midi_learn.cell_dash").to_string()
    };

    let clear_text = t!("midi_learn.btn_clear").to_string();

    Row::new(vec![
        Cell::from(label),
        Cell::from(enable_text).style(get_style(0)),
        Cell::from(disable_text).style(get_style(1)),
        Cell::from(clear_text).style(get_style(2)),
    ])
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
