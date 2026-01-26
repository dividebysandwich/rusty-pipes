use crate::config::{MidiDeviceConfig, MidiMappingMode};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
};
use rust_i18n::t;

/// Result of a keypress handling event.
pub enum MappingAction {
    /// No navigation change, stay on screen.
    None,
    /// User requested to return to the previous menu (Esc).
    Back,
}

/// Helper state for the MIDI TUI screen.
pub struct TuiMidiState {
    pub list_state: ListState,
    /// If `Some(index)`, the user is currently "editing" the value at that row index
    /// using Up/Down keys. If `None`, Up/Down keys navigate the list.
    pub editing_row: Option<usize>,
}

impl TuiMidiState {
    pub fn new() -> Self {
        let mut s = Self {
            list_state: ListState::default(),
            editing_row: None,
        };
        s.list_state.select(Some(0));
        s
    }
}

/// Handles keyboard input for the MIDI mapping screen.
/// Directly modifies the `MidiDeviceConfig`.
pub fn handle_input(
    event: KeyEvent,
    state: &mut TuiMidiState,
    device: &mut MidiDeviceConfig,
) -> MappingAction {
    let is_simple = matches!(device.mapping_mode, MidiMappingMode::Simple);

    // Calculate total rows for navigation wrapping
    // Row 0 is always "Mode Selection"
    // If Simple: Row 1 is "Target Channel" -> Total 2
    // If Complex: Rows 1-16 are input mappings -> Total 17
    let total_rows = if is_simple { 2 } else { 17 };

    match event.code {
        KeyCode::Esc => {
            // If editing, cancel edit. If not editing, go back.
            if state.editing_row.is_some() {
                state.editing_row = None;
                return MappingAction::None;
            }
            return MappingAction::Back;
        }

        KeyCode::Up | KeyCode::Char('k') => {
            if state.editing_row.is_none() {
                // --- Navigation Mode ---
                let i = state.list_state.selected().unwrap_or(0);
                let next = if i == 0 { total_rows - 1 } else { i - 1 };
                state.list_state.select(Some(next));
            } else {
                // --- Editing Mode ---
                // Which row are we editing?
                let idx = state.list_state.selected().unwrap_or(0);

                if idx == 1 && is_simple {
                    // Decrement Simple Target (wrap 0-15)
                    device.simple_target_channel = (device.simple_target_channel + 16 - 1) % 16;
                } else if idx > 0 && !is_simple {
                    // Decrement Complex Target for specific Input
                    let input_ch_idx = idx - 1; // Row 1 is Input 0
                    let current = device.complex_mapping[input_ch_idx];
                    device.complex_mapping[input_ch_idx] = (current + 16 - 1) % 16;
                }
            }
        }

        KeyCode::Down | KeyCode::Char('j') => {
            if state.editing_row.is_none() {
                // --- Navigation Mode ---
                let i = state.list_state.selected().unwrap_or(0);
                let next = (i + 1) % total_rows;
                state.list_state.select(Some(next));
            } else {
                // --- Editing Mode ---
                let idx = state.list_state.selected().unwrap_or(0);

                if idx == 1 && is_simple {
                    // Increment Simple Target
                    device.simple_target_channel = (device.simple_target_channel + 1) % 16;
                } else if idx > 0 && !is_simple {
                    // Increment Complex Target
                    let input_ch_idx = idx - 1;
                    let current = device.complex_mapping[input_ch_idx];
                    device.complex_mapping[input_ch_idx] = (current + 1) % 16;
                }
            }
        }

        KeyCode::Enter | KeyCode::Char(' ') => {
            let idx = state.list_state.selected().unwrap_or(0);

            if idx == 0 {
                // Row 0 is Mode Toggle (always toggles immediately)
                device.mapping_mode = match device.mapping_mode {
                    MidiMappingMode::Simple => MidiMappingMode::Complex,
                    MidiMappingMode::Complex => MidiMappingMode::Simple,
                };
                // If we switched modes, reset selection to avoid out of bounds
                state.list_state.select(Some(0));
                state.editing_row = None;
            } else {
                // Row > 0: Toggle Edit Mode
                if state.editing_row.is_some() {
                    state.editing_row = None; // Confirm/Exit Edit
                } else {
                    state.editing_row = Some(idx); // Enter Edit
                }
            }
        }
        _ => {}
    }
    MappingAction::None
}

/// Renders the MIDI mapping UI into the given area.
pub fn draw(frame: &mut Frame, area: Rect, state: &mut TuiMidiState, device: &MidiDeviceConfig) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(t!("tui_midi.title_fmt", name = device.name).to_string())
        .title_bottom(t!("tui_midi.footer").to_string());

    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    let mut items = Vec::new();

    // -- Row 0: Mode --
    let mode_str = match device.mapping_mode {
        MidiMappingMode::Simple => t!("tui_midi.mode_simple"),
        MidiMappingMode::Complex => t!("tui_midi.mode_complex"),
    };
    items.push(ListItem::new(
        t!("tui_midi.lbl_mode", val = mode_str).to_string(),
    ));

    // -- Dynamic Rows --
    match device.mapping_mode {
        MidiMappingMode::Simple => {
            // Row 1: Target
            let is_editing = state.editing_row == Some(1);
            let val_str =
                t!("tui_midi.lbl_target", ch = device.simple_target_channel + 1).to_string();
            items.push(create_list_item(val_str, is_editing));
        }
        MidiMappingMode::Complex => {
            // Rows 1-16: Complex Map
            for i in 0..16 {
                let is_editing = state.editing_row == Some(i + 1);
                let target = device.complex_mapping[i];
                let in_ch = format!("{:02}", i + 1);
                let out_ch = format!("{:02}", target + 1);
                let val_str = t!("tui_midi.lbl_complex", input = in_ch, out = out_ch).to_string();

                items.push(create_list_item(val_str, is_editing));
            }
        }
    }

    // Highlighting logic
    // If we are editing, we usually want the background to look different or the text to be yellow
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    frame.render_stateful_widget(list, inner_area, &mut state.list_state);
}

/// Helper to create a list item with specific styling if it's currently being edited.
fn create_list_item(text: String, is_editing: bool) -> ListItem<'static> {
    if is_editing {
        let edit_text = t!("tui_midi.suffix_editing", val = text).to_string();
        ListItem::new(edit_text).style(Style::default().fg(Color::Yellow))
    } else {
        ListItem::new(text)
    }
}
