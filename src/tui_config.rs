use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use midir::MidiInputPort;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use midir::MidiInput;

use crate::config::{AppSettings, ConfigState, RuntimeConfig};
use crate::tui::{cleanup_terminal, setup_terminal};
use crate::tui_filepicker;
use crate::app::{PIPES, LOGO};

enum ConfigMode {
    Main,
    MidiSelection,
    TextInput(usize, String), // Holds (config_index, buffer)
}

struct TuiConfigState {
    config_state: ConfigState,
    _midi_input_arc: Arc<Mutex<Option<MidiInput>>>,
    list_state: ListState,
    midi_list_state: ListState,
    mode: ConfigMode,
}

// Helper to get the display string for a config item
fn get_item_display(idx: usize, state: &ConfigState) -> String {
    let settings = &state.settings;
    match idx {
        0 => format!("1. Organ File:       {}", path_to_str(settings.organ_file.as_deref())),
        1 => format!("2. MIDI File (Play): {}", path_to_str(state.midi_file.as_deref())),
        2 => format!("3. MIDI Device:      {}", state.selected_midi_port.as_ref().map_or("None", |(_, n)| n.as_str())),
        3 => format!("4. IR File:          {}", path_to_str(settings.ir_file.as_deref())),
        4 => format!("5. Reverb Mix:       {:.2}", settings.reverb_mix),
        5 => format!("6. Gain:             {:.2}", settings.gain),
        6 => format!("7. Audio Buffer:     {} frames", settings.audio_buffer_frames),
        7 => format!("8. Pre-cache:        {}", bool_to_str(settings.precache)),
        8 => format!("9. Convert to 16-bit:{}", bool_to_str(settings.convert_to_16bit)),
        9 => format!("0. Original Tuning:  {}", bool_to_str(settings.original_tuning)),
        10 => "S. Start Rusty Pipes".to_string(),
        11 => "Q. Quit".to_string(),
        _ => unreachable!(),
    }
}

fn bool_to_str(val: bool) -> &'static str {
    if val { "ON" } else { "OFF" }
}

fn path_to_str(path: Option<&std::path::Path>) -> String {
    path.and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map_or("None".to_string(), |s| s.to_string())
}

/// Runs the TUI configuration loop.
pub fn run_config_ui(
    settings: AppSettings,
    midi_input_arc: Arc<Mutex<Option<MidiInput>>>
) -> Result<Option<RuntimeConfig>> {
    let mut terminal = setup_terminal()?;

    let config_state = ConfigState::new(settings, &midi_input_arc)?;

    let initial_midi_index = config_state.selected_midi_port.as_ref()
        .and_then(|(selected_port, _)| {
            config_state.available_ports.iter().position(|(port, _)| port == selected_port)
        });
    
    let mut midi_list_state = ListState::default();
    midi_list_state.select(initial_midi_index); // Select the found index (or None)

    let mut state = TuiConfigState {
        config_state,
        _midi_input_arc: midi_input_arc, // Store the arc
        list_state: ListState::default(),
        midi_list_state: ListState::default(),
        mode: ConfigMode::Main,
    };
    state.list_state.select(Some(0));

    let mut final_config: Option<RuntimeConfig> = None;

    'config_loop: loop {
        terminal.draw(|f| draw_config_ui(f, &mut state))?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        let event = event::read()?;
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match &mut state.mode {
                ConfigMode::Main => {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break 'config_loop,
                        KeyCode::Down | KeyCode::Char('j') => {
                            let i = state.list_state.selected().map_or(0, |i| (i + 1) % 12);
                            state.list_state.select(Some(i));
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let i = state.list_state.selected().map_or(11, |i| (i + 11) % 12);
                            state.list_state.select(Some(i));
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = state.list_state.selected() {
                                match idx {
                                    0 => { // Organ File
                                        let path = tui_filepicker::run_file_picker(
                                            &mut terminal,
                                            "Select Organ File",
                                            &["organ", "Organ_Hauptwerk_xml"],
                                        )?;
                                        if let Some(p) = path {
                                            state.config_state.settings.organ_file = Some(p);
                                        }
                                    }
                                    1 => { // MIDI File
                                        let path = tui_filepicker::run_file_picker(
                                            &mut terminal,
                                            "Select MIDI File (Optional)",
                                            &["mid", "midi"],
                                        )?;
                                        state.config_state.midi_file = path;
                                    }
                                    2 => { // MIDI Device
                                        state.mode = ConfigMode::MidiSelection;
                                    }
                                    3 => { // IR File
                                        let path = tui_filepicker::run_file_picker(
                                            &mut terminal,
                                            "Select IR File (Optional)",
                                            &["wav", "flac"],
                                        )?;
                                        state.config_state.settings.ir_file = path;
                                    }
                                    4 => { // Reverb Mix
                                        let buffer = state.config_state.settings.reverb_mix.to_string();
                                        state.mode = ConfigMode::TextInput(idx, buffer);
                                    }
                                    5 => { // Gain
                                        let gain = state.config_state.settings.gain.to_string();
                                        state.mode = ConfigMode::TextInput(idx, gain);
                                    }
                                    6 => { // Audio Buffer
                                        let buffer = state.config_state.settings.audio_buffer_frames.to_string();
                                        state.mode = ConfigMode::TextInput(idx, buffer);
                                    }
                                    7 => state.config_state.settings.precache = !state.config_state.settings.precache,
                                    8 => state.config_state.settings.convert_to_16bit = !state.config_state.settings.convert_to_16bit,
                                    9 => state.config_state.settings.original_tuning = !state.config_state.settings.original_tuning,
                                    10 => { // Start
                                        if state.config_state.settings.organ_file.is_none() {
                                            state.config_state.error_msg = Some("Please select an Organ File to start.".to_string());
                                        } else {
                                            let s = &state.config_state.settings;
                                            final_config = Some(RuntimeConfig {
                                                organ_file: s.organ_file.clone().unwrap(),
                                                ir_file: s.ir_file.clone(),
                                                reverb_mix: s.reverb_mix,
                                                audio_buffer_frames: s.audio_buffer_frames,
                                                precache: s.precache,
                                                convert_to_16bit: s.convert_to_16bit,
                                                original_tuning: s.original_tuning,
                                                midi_file: state.config_state.midi_file.clone(),
                                                midi_port: state.config_state.selected_midi_port.as_ref().map(|(p, _)| p.clone()),
                                                midi_port_name: state.config_state.selected_midi_port.as_ref().map(|(_, n)| n.clone()),
                                                gain: s.gain,
                                            });
                                            break 'config_loop;
                                        }
                                    }
                                    11 => break 'config_loop, // Quit
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                ConfigMode::MidiSelection => {
                    match key.code {
                        KeyCode::Esc => state.mode = ConfigMode::Main,
                        KeyCode::Down | KeyCode::Char('j') => {
                            let len = state.config_state.available_ports.len();
                            if len > 0 {
                                let i = state.midi_list_state.selected().map_or(0, |i| (i + 1) % len);
                                state.midi_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let len = state.config_state.available_ports.len();
                            if len > 0 {
                                let i = state.midi_list_state.selected().map_or(len - 1, |i| (i + len - 1) % len);
                                state.midi_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = state.midi_list_state.selected() {
                                state.config_state.selected_midi_port = state.config_state.available_ports.get(idx).cloned();
                            }
                            state.mode = ConfigMode::Main;
                        }
                        _ => {}
                    }
                }
                ConfigMode::TextInput(idx, buffer) => {
                    match key.code {
                        KeyCode::Char(c) => buffer.push(c),
                        KeyCode::Backspace => { buffer.pop(); }
                        KeyCode::Esc => state.mode = ConfigMode::Main,
                        KeyCode::Enter => {
                            match *idx {
                                4 => { // Reverb Mix
                                    if let Ok(val) = buffer.parse::<f32>() {
                                        state.config_state.settings.reverb_mix = val.clamp(0.0, 1.0);
                                    }
                                }
                                5 => { // Audio Buffer
                                    if let Ok(val) = buffer.parse::<usize>() {
                                        state.config_state.settings.audio_buffer_frames = val;
                                    }
                                }
                                _ => {}
                            }
                            state.mode = ConfigMode::Main;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    cleanup_terminal()?;
    Ok(final_config)
}

/// Renders the Configuration UI.
fn draw_config_ui(frame: &mut Frame, state: &mut TuiConfigState) {

    let area = frame.area();

    // --- Calculate new header height ---
    let pipes_lines = PIPES.lines().count(); // 7
    let logo_lines_count = LOGO.lines().count(); // 5
    // 7(pipes) + 5(logo) + 1(indicia) + 1(blank) + 1(title) + 2(borders) = 17
    let header_height = (pipes_lines + logo_lines_count + 5) as u16;

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // Logo + Title
            Constraint::Min(0),                // Config List
            Constraint::Length(3),             // Help/Error
        ])
        .split(area);

    // --- Build the logo header ---
    let orange_style = Style::default().fg(Color::Rgb(255, 165, 0));
    let gray_style = Style::default().fg(Color::Gray);
    let white_style = Style::default().fg(Color::White);

    let mut logo_lines_vec: Vec<Line> = PIPES.lines()
        .map(|line| Line::from(Span::styled(line, gray_style)))
        .collect();
    for line in LOGO.lines() {
        logo_lines_vec.push(Line::from(Span::styled(line, orange_style)));
    }
    logo_lines_vec.push(Line::from(Span::styled("Indicia MMXXV", orange_style)));
    logo_lines_vec.push(Line::from("")); // Blank line
    logo_lines_vec.push(Line::from(Span::styled(
        "Configuration",
        white_style.add_modifier(Modifier::BOLD)
    )));

    let title_widget = Paragraph::new(logo_lines_vec)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    
    frame.render_widget(title_widget, main_layout[0]);


    // Build config items
    let num_config_items = 11;
    let items: Vec<ListItem> = (0..num_config_items)
        .map(|i| {
            let text = get_item_display(i, &state.config_state);
            let mut list_item = ListItem::new(text.clone());
            
            // Index 10 is "S. Start Rusty Pipes"
            if text.contains("Start Rusty Pipes") {
                list_item = list_item.style(
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                );
            }
            list_item
        })
        .collect();

    let list_widget = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Settings"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("» ");
    frame.render_stateful_widget(list_widget, main_layout[1], &mut state.list_state);

    // Footer
    let footer_text = if let Some(err) = &state.config_state.error_msg {
        Line::styled(err.clone(), Style::default().fg(Color::Red))
    } else {
        Line::from("Nav: ↑/↓ | Enter: Select/Toggle | S: Start | Q: Quit")
    };
    let footer = Paragraph::new(footer_text)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, main_layout[2]);

    // Handle Modals
    match &state.mode {
        ConfigMode::MidiSelection => {
            draw_modal_list(
                frame,
                "Select MIDI Device (↑/↓, Enter, Esc)",
                &state.config_state.available_ports,
                &mut state.midi_list_state,
            );
        }
        ConfigMode::TextInput(idx, buffer) => {
            let title = match *idx {
                4 => "Enter Reverb Mix (0.0 - 1.0)",
                5 => "Enter Audio Buffer Size",
                _ => "Enter Value",
            };
            draw_text_input_modal(frame, title, buffer, 40, 3);
        }
        _ => {}
    }
}

fn draw_modal_list(
    frame: &mut Frame,
    title: &str,
    ports: &[(MidiInputPort, String)],
    list_state: &mut ListState,
) {
    let area = centered_rect(frame.area(), 60, 50);
    let items: Vec<ListItem> = ports.iter().map(|(_, name)| ListItem::new(name.clone())).collect();

    let list_widget = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("» ");

    frame.render_widget(Clear, area); // Clear background
    frame.render_stateful_widget(list_widget, area, list_state);
}

fn draw_text_input_modal(frame: &mut Frame, title: &str, buffer: &str, width_percent: u16, height_lines: u16) {
    // Manually calculate the centered rect with fixed line height
    let area = {
        let r = frame.area();
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((r.height.saturating_sub(height_lines)) / 2),
                Constraint::Length(height_lines),
                Constraint::Length((r.height.saturating_sub(height_lines)) / 2),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - width_percent) / 2),
                Constraint::Percentage(width_percent),
                Constraint::Percentage((100 - width_percent) / 2),
            ])
            .split(popup_layout[1])[1]
    };
    
    let text = format!("{}▋", buffer); // Show buffer with a "cursor"

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title(title));

    frame.render_widget(Clear, area); // Clear background
    frame.render_widget(paragraph, area);
}

/// Helper to create a centered rectangle.
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