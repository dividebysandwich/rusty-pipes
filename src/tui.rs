
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    symbols::Marker,
    widgets::{Block, Borders, canvas::{Canvas, Line as CanvasLine}, Clear, List, ListItem, ListState, Paragraph},
};
use std::{
    io::{stdout, Stdout},
    path::PathBuf,
    sync::{mpsc::{Sender, Receiver}, Arc},
    time::{Duration, Instant},
};

use midir::MidiInputConnection;
use crate::app::{AppMessage, TuiMessage};
use crate::organ::Organ;
// Import the new shared state and connection function
use crate::app_state::{AppState, connect_to_midi};

const NUM_COLUMNS: usize = 3; // Number of columns for the stop list

#[derive(Clone, PartialEq, Eq)]
enum AppMode {
    MidiSelection,
    MainApp,
    PresetSaveName(usize, String), // Holds (slot_index, current_name_buffer)
}

/// Holds the state specific to the TUI.
struct TuiState {
    mode: AppMode,
    shared_state: AppState, // The shared logic is now encapsulated here
    list_state: ListState, // TUI-specific selection state
    port_list_state: ListState, // TUI-specific selection state
    items_per_column: usize,
    stops_count: usize,
}

impl TuiState {
    fn new(organ: Arc<Organ>, is_file_playback: bool) -> Result<Self> {
        let shared_state = AppState::new(organ, is_file_playback)?;

        let mut port_list_state = ListState::default();
        if !shared_state.available_ports.is_empty() {
            port_list_state.select(Some(0));
        }

        let mode = if is_file_playback {
            AppMode::MainApp
        } else {
            AppMode::MidiSelection
        };

        let mut list_state = ListState::default();
        let stops_count = shared_state.organ.stops.len();
        if stops_count > 0 {
            list_state.select(Some(0)); // Select the first item
        }
        let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;

        Ok(Self {
            mode,
            shared_state,
            list_state,
            port_list_state,
            items_per_column,
            stops_count,
        })
    }
    
    // --- TUI-specific navigation ---

    fn next_midi_port(&mut self) {
        if self.shared_state.available_ports.is_empty() { return; }
        let i = match self.port_list_state.selected() {
            Some(i) => (i + 1) % self.shared_state.available_ports.len(),
            None => 0,
        };
        self.port_list_state.select(Some(i));
    }

    fn prev_midi_port(&mut self) {
        if self.shared_state.available_ports.is_empty() { return; }
        let i = match self.port_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.shared_state.available_ports.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.port_list_state.select(Some(i));
    }

    fn next_item(&mut self) {
        if self.stops_count == 0 { return; }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.stops_count,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn prev_item(&mut self) {
        if self.stops_count == 0 { return; }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.stops_count - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }
    fn next_col(&mut self) {
        if self.stops_count == 0 { return; }
        let i = match self.list_state.selected() {
            Some(i) => (i + self.items_per_column).min(self.stops_count - 1),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn prev_col(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => i.saturating_sub(self.items_per_column),
            None => 0,
        };
        self.list_state.select(Some(i));
    }
    
    // --- Logic functions now delegate to shared_state ---

    /// Toggles a specific channel (0-9) for the currently selected stop.
    fn toggle_stop_channel(&mut self, channel: u8, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let Some(selected_index) = self.list_state.selected() {
            self.shared_state.toggle_stop_channel(selected_index, channel, audio_tx)?;
        }
        Ok(())
    }

    /// Activates all channels (0-9) for the selected stop.
    fn select_all_channels_for_stop(&mut self) {
        if let Some(selected_index) = self.list_state.selected() {
            self.shared_state.select_all_channels_for_stop(selected_index);
        }
    }

    /// Deactivates all channels (0-9) for the selected stop.
    fn select_none_channels_for_stop(&mut self, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let Some(selected_index) = self.list_state.selected() {
            self.shared_state.select_none_channels_for_stop(selected_index, audio_tx)?;
        }
        Ok(())
    }
}

/// Runs the main TUI loop, blocking the main thread.
pub fn run_tui_loop(
    audio_tx: Sender<AppMessage>,
    tui_rx: Receiver<TuiMessage>,
    tui_tx: Sender<TuiMessage>,
    organ: Arc<Organ>,
    ir_file_path: Option<PathBuf>,
    reverb_mix: f32,
    is_file_playback: bool,
    preselected_device_name: Option<String>,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut _midi_connection: Option<MidiInputConnection<()>> = None;
    let mut app_state = TuiState::new(organ, is_file_playback)?;

    // Handle preselected MIDI device if provided
    if !is_file_playback {
        if let Some(device_name) = preselected_device_name {
            // Try to find the port by name from the state
            let found_port = app_state.shared_state.available_ports.iter()
                .find(|(_, name)| *name == device_name)
                .map(|(port, _)| port.clone());

            if let Some(port) = found_port {
                // Found it! Now connect.
                if let Some(midi_input) = app_state.shared_state.midi_input.take() {
                    let conn = connect_to_midi(
                        midi_input,
                        &port,
                        &device_name,
                        &tui_tx,
                    )?;
                    _midi_connection = Some(conn);
                    app_state.mode = AppMode::MainApp; // Switch mode
                    app_state.shared_state.add_midi_log(format!("Connected to: {}", device_name));
                    app_state.shared_state.available_ports.clear(); // Clean up
                }
            } else {
                // Error: Device name not found
                let err_msg = format!("ERROR: MIDI device not found: '{}'", device_name);
                app_state.shared_state.error_msg = Some(err_msg);
            }
        }
    }

    if let Some(path) = ir_file_path {
        if path.exists() {
            let log_msg = format!("Loading IR file: {:?}", path.file_name().unwrap());
            app_state.shared_state.add_midi_log(log_msg);
            // Send the message to the audio thread
            audio_tx.send(AppMessage::SetReverbIr(path))?;
            audio_tx.send(AppMessage::SetReverbWetDry(reverb_mix))?;
        } else {
            // Log an error to the TUI, but don't crash
            let log_msg = format!("ERROR: IR file not found: {}", path.display());
            app_state.shared_state.add_midi_log(log_msg);
        }
    }
    loop {
        // Update piano roll state before drawing (only if in main app mode)
        if matches!(app_state.mode, AppMode::MainApp) {
            app_state.shared_state.update_piano_roll_state();
        }

        // Draw UI (which now dispatches based on mode)
        terminal.draw(|f| ui(f, &mut app_state))?;

        // Handle cross-thread messages (non-blocking)
        // This now uses the shared message handler
        while let Ok(msg) = tui_rx.try_recv() {
            if let Err(e) = app_state.shared_state.handle_tui_message(msg, &audio_tx) {
                app_state.shared_state.add_midi_log(format!("Error: {}", e));
            }
        }

        // Handle input
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                
                    match app_state.mode {
                        AppMode::MidiSelection => {
                            // --- Handle MIDI Selection Input ---
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    audio_tx.send(AppMessage::Quit)?;
                                    break; // Exit TUI loop
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    app_state.next_midi_port();
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    app_state.prev_midi_port();
                                }
                                KeyCode::Enter => {
                                    // --- CONNECT TO MIDI DEVICE ---
                                    if let Some(selected_idx) = app_state.port_list_state.selected() {

                                        let (port_to_connect, port_name) = 
                                            match app_state.shared_state.available_ports.get(selected_idx) {
                                                Some((port, name)) => (port.clone(), name.clone()),
                                                None => continue,
                                            };

                                        app_state.shared_state.add_midi_log(format!("Connecting to: {}", port_name));
                                        
                                        if let Some(midi_input) = app_state.shared_state.midi_input.take() {
                                            
                                            let conn = connect_to_midi(
                                                midi_input,
                                                &port_to_connect,
                                                &port_name,
                                                &tui_tx,
                                            )?;
                                        
                                            // Store the connection to keep it alive
                                            _midi_connection = Some(conn);
                                            
                                            // Transition to the main app
                                            app_state.mode = AppMode::MainApp;
                                            
                                            // Free this memory as we don't need it anymore
                                            app_state.shared_state.available_ports.clear(); 
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        AppMode::MainApp => {
                            // --- Handle Main App Input ---
                            let channel_to_toggle = match key.code {
                                KeyCode::Char('1') => Some(0),
                                KeyCode::Char('2') => Some(1),
                                KeyCode::Char('3') => Some(2),
                                KeyCode::Char('4') => Some(3),
                                KeyCode::Char('5') => Some(4),
                                KeyCode::Char('6') => Some(5),
                                KeyCode::Char('7') => Some(6),
                                KeyCode::Char('8') => Some(7),
                                KeyCode::Char('9') => Some(8),
                                KeyCode::Char('0') => Some(9),
                                _ => None,
                            };
                            if let Some(channel) = channel_to_toggle {
                                app_state.toggle_stop_channel(channel, &audio_tx)?;
                            } else {
                                // Handle other keys if no channel key was pressed
                                match key.code {
                                    KeyCode::Char('q') | KeyCode::Esc => {
                                        audio_tx.send(AppMessage::Quit)?;
                                        break; // Exit TUI loop
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        app_state.next_item();
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        app_state.prev_item();
                                    }
                                    KeyCode::Char('l') | KeyCode::Right => app_state.next_col(),
                                    KeyCode::Char('h') | KeyCode::Left => app_state.prev_col(),
                                    KeyCode::Char('p') => {
                                        audio_tx.send(AppMessage::AllNotesOff)?;
                                    }
                                    KeyCode::Char('a') => {
                                        app_state.select_all_channels_for_stop();
                                    }
                                    KeyCode::Char('n') => {
                                        app_state.select_none_channels_for_stop(&audio_tx)?;
                                    }
                                    KeyCode::F(n) if (1..=12).contains(&n) && key.modifiers.contains(KeyModifiers::SHIFT) => {
                                        let slot = (n - 1) as usize;
                                        // Get existing name or create default
                                        let current_name = app_state.shared_state.presets[slot]
                                            .as_ref()
                                            .map_or_else(
                                                || format!("Preset F{}", slot + 1),
                                                |p| p.name.clone()
                                            );
                                        // Switch mode to ask for name
                                        app_state.mode = AppMode::PresetSaveName(slot, current_name);
                                    }
                                    // Recall (F1-F12, no modifier)
                                    KeyCode::F(n) if (1..=12).contains(&n) && key.modifiers.is_empty() => {
                                        if let Err(e) = app_state.shared_state.recall_preset((n - 1) as usize, &audio_tx) {
                                            app_state.shared_state.add_midi_log(format!("ERROR recalling preset: {}", e));
                                        }
                                    }                                    _ => {}
                                }
                            }
                        },
                        AppMode::PresetSaveName(slot, ref mut name_buffer) => {
                            match key.code {
                                KeyCode::Enter => {
                                    // Save the preset
                                    if !name_buffer.is_empty() {
                                        app_state.shared_state.save_preset(slot, name_buffer.clone());
                                    }
                                    // Return to main app
                                    app_state.mode = AppMode::MainApp;
                                }
                                KeyCode::Char(c) => {
                                    // Add char to buffer
                                    name_buffer.push(c);
                                }
                                KeyCode::Backspace => {
                                    // Remove char from buffer
                                    name_buffer.pop();
                                }
                                KeyCode::Esc => {
                                    // Cancel
                                    app_state.mode = AppMode::MainApp;
                                }
                                _ => {} // Ignore other keys
                            }
                        }
                    }
                }
            }
        }
    }

    cleanup_terminal()?;
    Ok(())
}

// ... (LOGO, PIPES constants remain the same) ...
const PIPES: &str = r"        ███         
      ▐█▋ ███ ▐█▋      
  ▐█▋ ▐█▋ ███ ▐█▋ ▐█▋   
  ▐█▋ ▐█▋ ███ ▐█▋ ▐█▋   
  ▐█▋ ▐█▋ ███ ▐█▋ ▐█▋   
  ▐▅▋ ▐▅▋ ▐▄▋ ▐▅▋ ▐▅▋   
   ▀   ▀   █   ▀   ▀   
█████████████████████
        ▀▀▀▀▀         
";

const LOGO: &str = r"██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗    ██████╗ ██╗██████╗ ███████╗███████╗
██╔══██╗██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝    ██╔══██╗██║██╔══██╗██╔════╝██╔════╝
██████╔╝██║   ██║███████╗   ██║    ╚████╔╝     ██████╔╝██║██████╔╝█████╗  ███████╗
██╔══██╗██║   ██║╚════██║   ██║     ╚██╔╝      ██╔═══╝ ██║██╔═══╝ ██╔══╝  ╚════██║
██║  ██║╚██████╔╝███████║   ██║      ██║       ██║     ██║██║     ███████╗███████║
╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝      ╚═╝       ╚═╝     ╚═╝╚═╝     ╚══════╝╚══════╝
";

// MIDI Selection UI function
fn draw_midi_selection_ui(frame: &mut Frame, state: &mut TuiState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50), // Logo
            Constraint::Percentage(50), // List
        ])
        .split(frame.area());

    // --- Logo and Version ---
    let version = env!("CARGO_PKG_VERSION");
    let description_text = env!("CARGO_PKG_DESCRIPTION");
    let orange_style = Style::default().fg(Color::Rgb(255, 165, 0));
    let white_style = Style::default().fg(Color::White);
    let gray_style = Style::default().fg(Color::Gray);
    // Create a vector of Lines for the logo, one for each line in the ASCII art
    let mut logo_lines: Vec<Line> = PIPES.lines() // This splits the string by newlines
        .map(|line| Line::from(Span::styled(line, gray_style)))
        .collect();
    // Append lines from the LOGO constant
    for line in LOGO.lines() {
        logo_lines.push(Line::from(Span::styled(line, orange_style)));
    }


    logo_lines.push(Line::from(Span::styled("Indicia MMXXV", orange_style)));
    logo_lines.push(Line::from(""));
    logo_lines.push(Line::from(Span::styled(
        description_text,
        white_style,
    )));
    logo_lines.push(Line::from(Span::styled(
        format!("Version {}", version),
        white_style,
    )));

    // Pass the Vec<Line> to the Paragraph
    let logo_widget = Paragraph::new(logo_lines)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::NONE));


    frame.render_widget(logo_widget, layout[0]);

    // --- MIDI Device List ---
    // Data now comes from shared_state
    let items: Vec<ListItem> = state.shared_state.available_ports.iter()
        .map(|(_, name)| {
            ListItem::new(name.clone())
        })
        .collect();

    let title = if items.is_empty() {
        "No MIDI Input Devices Found! (Press 'q' to quit)"
    } else {
        "Select a MIDI Input Device (Use ↑/↓ and Enter)"
    };
        
    let list_widget = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("» ");

    frame.render_stateful_widget(list_widget, layout[1], &mut state.port_list_state);
}

// Main App UI function
fn draw_main_app_ui(frame: &mut Frame, state: &mut TuiState) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(70), // Stops
            Constraint::Percentage(30), // MIDI Log
            Constraint::Length(1),      // Footer
        ])
        .split(frame.area());

    // --- Footer Help Text / Error ---
    // Data now comes from shared_state
    let footer_widget = if let Some(err) = &state.shared_state.error_msg {
        Paragraph::new(err.as_str())
            .style(Style::default().fg(Color::White).bg(Color::Red))
    } else {
        let help_text = "Q:Quit | Nav:↑↓←→/jkli | Ch:1-0 | A:All | N:None | P:Panic | F1-12:Recall | Shift+F1-12:Save";
        Paragraph::new(help_text).alignment(Alignment::Center)
    };
    frame.render_widget(footer_widget, main_layout[2]);

    // --- Stop List (Multi-column) ---
    const NUM_COLUMNS: usize = 3;
    let stops_area = main_layout[0];

    // Create 3 columns
    let column_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34), // Col 1
            Constraint::Percentage(33), // Col 2
            Constraint::Percentage(33), // Col 3
        ])
        .split(stops_area);
    
    // Use TUI-specific state for selection
    let selected_index = state.list_state.selected().unwrap_or(0);
    // Use shared_state for data
    let stops_count = state.shared_state.organ.stops.len();
    if stops_count == 0 {
        // Handle no stops
        let no_stops_msg = Paragraph::new("No stops loaded.")
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(state.shared_state.organ.name.as_str()));
        frame.render_widget(no_stops_msg, stops_area);
    } else {
        // Calculate items per column
        let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;
        
        let all_stops: Vec<_> = state.shared_state.organ.stops.iter().enumerate().collect();
        
        // Create a list for each column
        for (col_idx, rect) in column_layout.iter().enumerate() {
            let start_idx = col_idx * items_per_column;
            let end_idx = (start_idx + items_per_column).min(stops_count);

            if start_idx >= end_idx {
                continue; // No items for this column
            }

            let column_items: Vec<ListItem> = all_stops[start_idx..end_idx].iter()
                .map(|(global_idx, stop)| {
                    // Get the set of active channels for this stop (from shared_state)
                    let active_channels = state
                        .shared_state
                        .stop_channels
                        .get(global_idx)
                        .cloned()
                        .unwrap_or_default();

                    // Build the Vec<Span> for the 10 channel slots
                    let mut channel_spans: Vec<Span> = Vec::with_capacity(22);

                    for i in 0..10u8 { // 0..=9, representing channels 1-10
                        if active_channels.contains(&i) {
                            // Channel is active: Display number
                            let display_num = if i == 9 {
                                "0".to_string()
                            } else {
                                format!("{}", i + 1)
                            };
                            channel_spans.push(Span::styled(
                                display_num,
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                            ));
                        } else {
                            // Channel is inactive: Display gray block "■"
                            channel_spans.push(Span::styled(
                                "■",    
                                Style::default().fg(Color::DarkGray),
                            ));
                        }
                    }

                    // Add padding and the stop name
                    channel_spans.push(Span::raw(format!("    {}", stop.name))); // 4 spaces for padding
                    let line = Line::from(channel_spans);

                    let style = if selected_index == *global_idx {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else if !active_channels.is_empty() {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default()
                    };
                    ListItem::new(line).style(style)
                })
                .collect();
            
            let title = if col_idx == 0 { state.shared_state.organ.name.as_str() } else { "" };
            let list_widget = List::new(column_items)
                .block(Block::default().borders(Borders::ALL).title(title));
            frame.render_widget(list_widget, *rect);
        }
    }

    // --- Bottom Area (MIDI Log + Piano Roll) ---
    let bottom_area = main_layout[1];
    let bottom_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20), // MIDI Log
            Constraint::Percentage(80), // Piano Roll
        ])
        .split(bottom_area);

    // --- MIDI Log (in bottom_chunks[0]) ---
    // Data from shared_state
    let log_items: Vec<ListItem> = state.shared_state.midi_log.iter()
        .map(|msg| ListItem::new(Line::from(msg.clone())))
        .collect();

    let log_widget = List::new(log_items)
        .block(Block::default().borders(Borders::ALL).title("MIDI Log"))
        .style(Style::default().fg(Color::Cyan));
    
    frame.render_widget(log_widget, bottom_chunks[0]);

    // --- Piano Roll (in bottom_chunks[1]) ---
    const PIANO_LOW_NOTE: u8 = 21;  // A0
    const PIANO_HIGH_NOTE: u8 = 108; // C8
    const BLACK_KEY_MODS: [u8; 5] = [1, 3, 6, 8, 10]; // C#, D#, F#, G#, A#

    let now = Instant::now();
    let display_start_time = now.checked_sub(state.shared_state.piano_roll_display_duration)
        .unwrap_or(Instant::now());

    let piano_roll = Canvas::default()
        .block(Block::default().borders(Borders::ALL).title("Piano Roll"))
        .marker(Marker::Block)
        .x_bounds([
            PIANO_LOW_NOTE as f64,
            PIANO_HIGH_NOTE as f64 + 1.0,
        ])
        .y_bounds([
            0.0,    
            state.shared_state.piano_roll_display_duration.as_secs_f64()
        ])
        .paint(|ctx| {
            let area_height_coords = state.shared_state.piano_roll_display_duration.as_secs_f64();

            // Draw the static keyboard background
            for note in PIANO_LOW_NOTE..=PIANO_HIGH_NOTE {
                let is_black_key = BLACK_KEY_MODS.contains(&(note % 12));
                let color = if is_black_key { Color::Rgb(50, 50, 50) } else { Color::Rgb(100, 100, 100) };
                
                ctx.draw(&CanvasLine {
                    x1: note as f64,
                    y1: 0.0,
                    x2: note as f64, // Vertical line
                    y2: area_height_coords,
                    color,
                });
            }

            // Function to map a time Instant to a Y-coordinate
            let map_time_to_y = |time: Instant| -> f64 {
                let time_elapsed_from_start = time.duration_since(display_start_time).as_secs_f64();
                time_elapsed_from_start
            };

            // Draw finished notes (from shared_state)
            for played_note in &state.shared_state.finished_notes_display {
                let note_x = played_note.note as f64;
                let start_y = map_time_to_y(played_note.start_time);
                let end_y = played_note.end_time.map_or_else(
                    || map_time_to_y(now),
                    |et| map_time_to_y(et),
                );
                
                ctx.draw(&CanvasLine {
                    x1: note_x, y1: start_y,
                    x2: note_x, y2: end_y,
                    color: Color::Magenta,
                });
            }

            // Draw currently playing notes (from shared_state)
            for (_, played_note) in &state.shared_state.currently_playing_notes {
                let note_x = played_note.note as f64;
                let start_y = map_time_to_y(played_note.start_time);
                let end_y = map_time_to_y(now);    
                
                ctx.draw(&CanvasLine {
                    x1: note_x, y1: start_y,
                    x2: note_x, y2: end_y,
                    color: Color::Green,
                });
            }
        });
    frame.render_widget(piano_roll, bottom_chunks[1]);
}

/// Renders the UI frame.
fn ui(frame: &mut Frame, state: &mut TuiState) {
    let mode = state.mode.clone();
    match mode {
        AppMode::MidiSelection => draw_midi_selection_ui(frame, state),
        AppMode::MainApp => draw_main_app_ui(frame, state),
        // Draw modal on top
        AppMode::PresetSaveName(slot, name_buffer) => {
            // Draw the main app in the background
            draw_main_app_ui(frame, state);
            // Draw the modal on top
            draw_preset_save_modal(frame, slot, &name_buffer);
        }
    }
}

/// Renders a modal window for saving a preset.
fn draw_preset_save_modal(frame: &mut Frame, slot: usize, name_buffer: &str) {
    // 60% width, 20% height
    let area = centered_rect(frame.area(), 60, 20); 
    let slot_display = slot + 1;
    
    let text = vec![
        Line::from(Span::styled(
            format!("Save Preset F{}", slot_display),
            Style::default().add_modifier(Modifier::BOLD)
        )),
        Line::from(""),
        Line::from("Enter a name:"),
        Line::from(""),
        Line::from(Span::styled(
            format!("{}▋", name_buffer), // Show buffer with a "cursor"
            Style::default().fg(Color::Yellow)
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press [Enter] to save, [Esc] to cancel",
            Style::default().fg(Color::DarkGray)
        )),
    ];

    let modal_block = Block::default().title("Save Preset").borders(Borders::ALL);
    let modal_paragraph = Paragraph::new(text)
        .block(modal_block)
        .alignment(Alignment::Center);

    frame.render_widget(Clear, area); // Clear the area behind the modal
    frame.render_widget(modal_paragraph, area); // Render the modal
}

/// Helper to create a centered rectangle for the modal.
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

/// Helper to set up the terminal for TUI mode.
fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    let mut stdout = stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Helper to clean up the terminal.
fn cleanup_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}