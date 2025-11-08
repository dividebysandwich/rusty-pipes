use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    symbols::Marker,
    widgets::{Block, Borders, canvas::{Canvas, Line as CanvasLine}, List, ListItem, ListState, Paragraph},
};
use std::{
    io::{stdout, Stdout},
    sync::{mpsc::{Sender, Receiver}, Arc},
    time::{Duration, Instant},
    collections::{BTreeSet, HashMap, VecDeque},
};

use crate::{app::{AppMessage, TuiMessage}, organ::Organ};

const MIDI_LOG_CAPACITY: usize = 10; // Max log lines
const NUM_COLUMNS: usize = 3; // Number of columns for the stop list

#[derive(Debug, Clone, PartialEq)]
struct PlayedNote {
    note: u8,
    start_time: Instant,
    end_time: Option<Instant>, // None if still playing
}

/// Holds the state for the TUI.
struct TuiState {
    organ: Arc<Organ>,
    list_state: ListState,
    /// Maps stop_index -> set of active MIDI channels (0-9)
    stop_channels: HashMap<usize, BTreeSet<u8>>,
    midi_log: VecDeque<String>,
    error_msg: Option<String>,
    items_per_column: usize,
    stops_count: usize,
    // Currently active notes, mapping midi note -> PlayedNote instance
    currently_playing_notes: HashMap<u8, PlayedNote>, 
    // Notes that have finished playing, but are still within the display window
    finished_notes_display: VecDeque<PlayedNote>,
    // Time parameters for the scrolling window
    piano_roll_display_duration: Duration,
    /// Maps MIDI Channel (0-15) -> Set of active notes (0-127)
    channel_active_notes: HashMap<u8, BTreeSet<u8>>,
}

impl TuiState {
    fn new(organ: Arc<Organ>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0)); // Select the first item
        let stops_count = organ.stops.len();
        let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;
        Self {
            organ,
            list_state,
            stop_channels: HashMap::new(),
            midi_log: VecDeque::with_capacity(MIDI_LOG_CAPACITY),
            error_msg: None,
            items_per_column,
            stops_count,
            currently_playing_notes: HashMap::new(),
            finished_notes_display: VecDeque::new(),
            piano_roll_display_duration: Duration::from_secs(1), // Show 1 second of history
            channel_active_notes: HashMap::new(),
        }
    }

    fn next_item(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.organ.stops.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn prev_item(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.organ.stops.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }
    fn next_col(&mut self) {
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

    /// Toggles a specific channel (0-9) for the currently selected stop.
    fn toggle_stop_channel(&mut self, channel: u8, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let Some(selected_index) = self.list_state.selected() {
            let stop_set = self.stop_channels.entry(selected_index).or_default();
            
            if stop_set.contains(&channel) {
                stop_set.remove(&channel);
                
                // --- Send NoteOff for all active notes on this channel for this stop ---
                if let Some(notes_to_stop) = self.channel_active_notes.get(&channel) {
                    if let Some(stop) = self.organ.stops.get(selected_index) {
                        let stop_name = stop.name.clone();
                        for &note in notes_to_stop {
                            audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                        }
                    }
                }
            } else {
                stop_set.insert(channel);
            };
        }
        Ok(())
    }

    /// Activates all channels (0-9) for the selected stop.
    fn select_all_channels_for_stop(&mut self) {
        if let Some(selected_index) = self.list_state.selected() {
            let stop_set = self.stop_channels.entry(selected_index).or_default();
            for channel in 0..10 { // Channels 0-9
                stop_set.insert(channel);
            }
        }
    }

    /// Deactivates all channels (0-9) for the selected stop.
    fn select_none_channels_for_stop(&mut self, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let Some(selected_index) = self.list_state.selected() {
            if let Some(stop_set) = self.stop_channels.get_mut(&selected_index) {
                // Collect channels to deactivate
                let channels_to_deactivate: Vec<u8> = stop_set.iter().copied()
                    .filter(|&c| c < 10)
                    .collect();

                if !channels_to_deactivate.is_empty() {
                    if let Some(stop) = self.organ.stops.get(selected_index) {
                        let stop_name = stop.name.clone();
                        for channel in channels_to_deactivate {
                            // --- Send NoteOff for all active notes on this channel for this stop ---
                            if let Some(notes_to_stop) = self.channel_active_notes.get(&channel) {
                                for &note in notes_to_stop {
                                    audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                                }
                            }
                            // Now remove it from the state
                            stop_set.remove(&channel);
                        }
                    } else {
                        // Fallback (shouldn't happen)
                        for channel in channels_to_deactivate {
                            stop_set.remove(&channel);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn add_midi_log(&mut self, msg: String) {
        if self.midi_log.len() == MIDI_LOG_CAPACITY {
            self.midi_log.pop_front();
        }
        self.midi_log.push_back(msg);
    }

    fn handle_tui_note_on(&mut self, note: u8, start_time: Instant) {
        let played_note = PlayedNote {
            note,
            start_time,
            end_time: None,
        };
        self.currently_playing_notes.insert(note, played_note);
    }

    fn handle_tui_note_off(&mut self, note: u8, end_time: Instant) {
        if let Some(mut played_note) = self.currently_playing_notes.remove(&note) {
            played_note.end_time = Some(end_time);
            self.finished_notes_display.push_back(played_note);
        }
    }

    fn handle_tui_all_notes_off(&mut self) {
        let now = Instant::now();
        for (_, mut played_note) in self.currently_playing_notes.drain() {
            played_note.end_time = Some(now);
            self.finished_notes_display.push_back(played_note);
        }
    }

    fn update_piano_roll_state(&mut self) {
        let now = Instant::now();

        // Remove notes that are entirely off-screen
        let oldest_time_to_display = now.checked_sub(self.piano_roll_display_duration)
            .unwrap_or(Instant::now()); // Safely get the boundary

        while let Some(front_note) = self.finished_notes_display.front() {
            // A note is off-screen if its end_time is older than the oldest_time_to_display
            // OR if its start_time is older and it has no end_time (very long hanging note)
            let is_off_screen = front_note.end_time.map_or(
                front_note.start_time < oldest_time_to_display, // Still playing, but started too long ago
                |et| et < oldest_time_to_display, // Finished, and ended too long ago
            );

            if is_off_screen {
                self.finished_notes_display.pop_front();
            } else {
                break; // Stop when we find a note that's still on screen
            }
        }
    }
}

/// Runs the main TUI loop, blocking the main thread.
pub fn run_tui_loop(
    audio_tx: Sender<AppMessage>,
    tui_rx: Receiver<TuiMessage>,
    organ: Arc<Organ>,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app_state = TuiState::new(organ);

    loop {
        // Update piano roll state before drawing
        app_state.update_piano_roll_state();

        // Draw UI
        terminal.draw(|f| ui(f, &mut app_state))?;

        // Handle cross-thread messages (non-blocking)
        while let Ok(msg) = tui_rx.try_recv() {
            match msg {
                // --- Raw MIDI events ---
                TuiMessage::MidiNoteOn(note, vel, channel) => {
                    // Track the active note
                    app_state.channel_active_notes.entry(channel).or_default().insert(note);
                    // Find all stops mapped to this channel and send AppMessage
                    for (stop_index, active_channels) in &app_state.stop_channels {
                        if active_channels.contains(&channel) {
                            if let Some(stop) = app_state.organ.stops.get(*stop_index) {
                                let stop_name = stop.name.clone();
                                audio_tx.send(AppMessage::NoteOn(note, vel, stop_name))?;
                            }
                        }
                    }
                },
                TuiMessage::MidiNoteOff(note, channel) => {
                    // Stop tracking the active note
                    if let Some(notes) = app_state.channel_active_notes.get_mut(&channel) {
                        notes.remove(&note);
                    }
                    // Find all stops mapped to this channel and send AppMessage
                    for (stop_index, active_channels) in &app_state.stop_channels {
                        if active_channels.contains(&channel) {
                            if let Some(stop) = app_state.organ.stops.get(*stop_index) {
                                let stop_name = stop.name.clone();
                                audio_tx.send(AppMessage::NoteOff(note, stop_name))?;
                            }
                        }
                    }
                },
                TuiMessage::MidiChannelNotesOff(channel) => {
                    // Handle channel-specific all notes off
                    if let Some(notes_to_stop) = app_state.channel_active_notes.remove(&channel) {
                        // Find all stops mapped to this channel
                        for (stop_index, active_channels) in &app_state.stop_channels {
                            if active_channels.contains(&channel) {
                                if let Some(stop) = app_state.organ.stops.get(*stop_index) {
                                    let stop_name = stop.name.clone();
                                    // Send NoteOff for each note that was active on this channel
                                    for &note in &notes_to_stop {
                                        audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                                    }
                                }
                            }
                        }
                    }
                },
                
                // --- Other TUI messages ---
                TuiMessage::MidiLog(log) => app_state.add_midi_log(log),
                TuiMessage::Error(err) => app_state.error_msg = Some(err),
                TuiMessage::TuiNoteOn(note, start_time) => app_state.handle_tui_note_on(note, start_time),
                TuiMessage::TuiNoteOff(note, end_time) => app_state.handle_tui_note_off(note, end_time),
                TuiMessage::TuiAllNotesOff => app_state.handle_tui_all_notes_off(),
            }
        }

        // Handle input
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Handle channel toggles
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
                                // Send Quit message to audio thread
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
                                // Send "Panic" message (Global All Notes Off)
                                audio_tx.send(AppMessage::AllNotesOff)?;
                            }
                            // Remove Space/Enter bindings as they are replaced by 1-0
                            // KeyCode::Char(' ') | KeyCode::Enter => { ... }
                            KeyCode::Char('a') => {
                                // All channels for selected stop
                                app_state.select_all_channels_for_stop();
                            }
                            KeyCode::Char('n') => {
                                // No channels for selected stop
                                app_state.select_none_channels_for_stop(&audio_tx)?;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    cleanup_terminal()?;
    Ok(())
}

/// Renders the UI frame.
fn ui(frame: &mut Frame, state: &mut TuiState) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(80), // Stops
            Constraint::Percentage(20), // MIDI Log
            Constraint::Length(1),      // Footer
        ])
        .split(frame.area());

    // --- Footer Help Text / Error ---
    let footer_widget = if let Some(err) = &state.error_msg {
        Paragraph::new(err.as_str())
            .style(Style::default().fg(Color::White).bg(Color::Red))
    } else {
        let help_text = "Quit: q | Nav: ↑/k, ↓/j, ←/h, →/l | Toggle Chan: 1-0 | Panic: p | Stop All Chan: a | Stop No Chan: n";
        Paragraph::new(help_text).alignment(Alignment::Center)    };
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
    
    let selected_index = state.list_state.selected().unwrap_or(0);
    let stops_count = state.organ.stops.len();
    if stops_count == 0 {
        // Handle no stops
        let no_stops_msg = Paragraph::new("No stops loaded.")
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(state.organ.name.as_str()));
        frame.render_widget(no_stops_msg, stops_area);
    } else {
        // Calculate items per column
        let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;
        
        let all_stops: Vec<_> = state.organ.stops.iter().enumerate().collect();
        
        // Create a list for each column
        for (col_idx, rect) in column_layout.iter().enumerate() {
            let start_idx = col_idx * items_per_column;
            let end_idx = (start_idx + items_per_column).min(stops_count);

            if start_idx >= end_idx {
                continue; // No items for this column
            }

            let column_items: Vec<ListItem> = all_stops[start_idx..end_idx].iter()
                .map(|(global_idx, stop)| {
                    // Build the channel string
                    let channels_str = if let Some(channels) = state.stop_channels.get(global_idx) {
                        if channels.is_empty() {
                            "[ ]".to_string()
                        } else {
                            // Collect channel numbers (displaying 1-10)
                            let mut channel_nums: Vec<String> = channels.iter()
                                .map(|c| if *c == 9 { "10".to_string() } else { (c + 1).to_string() })
                                .collect();
                            // Sort them numerically for consistent display
                            channel_nums.sort_by_key(|a| a.parse::<u8>().unwrap_or(0));
                            format!("[{}]", channel_nums.join(" "))
                        }
                    } else {
                        "[ ]".to_string()
                    };

                    // Add padding to align stop names
                    let line = Line::from(format!("{:<15} {}", channels_str, stop.name));
                    
                    let style = if selected_index == *global_idx {
                        // Highlight selected
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else if state.stop_channels.get(global_idx).map_or(false, |s| !s.is_empty()) {
                        // Highlight if any channel is active
                        Style::default().fg(Color::Green)
                    } else {
                        // Normal
                        Style::default()
                    };
                    ListItem::new(line).style(style)
                })
                .collect();
            
            let title = if col_idx == 0 { state.organ.name.as_str() } else { "" };
            let list_widget = List::new(column_items)
                .block(Block::default().borders(Borders::ALL).title(title));
                
            // We don't use render_stateful_widget because we handle selection manually
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
    let log_items: Vec<ListItem> = state.midi_log.iter()
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

    // Adjust y-bounds to be based on time
    // x_bounds will be MIDI notes
    // The current time will be the "bottom" of the display
    let now = Instant::now();
    let display_start_time = now.checked_sub(state.piano_roll_display_duration)
        .unwrap_or(Instant::now());

    let piano_roll = Canvas::default()
        .block(Block::default().borders(Borders::ALL).title("Piano Roll"))
        .marker(Marker::Block)
        .x_bounds([
            PIANO_LOW_NOTE as f64,
            PIANO_HIGH_NOTE as f64 + 1.0,
        ])
        // Y-bounds: 0.0 at the oldest time (top), 1.0 at current time (bottom)
        .y_bounds([
            0.0, 
            state.piano_roll_display_duration.as_secs_f64()
        ])
        .paint(|ctx| {
            let area_height_coords = state.piano_roll_display_duration.as_secs_f64();

            // Draw the static keyboard background on the X-axis
            for note in PIANO_LOW_NOTE..=PIANO_HIGH_NOTE {
                let is_black_key = BLACK_KEY_MODS.contains(&(note % 12));
                let color = if is_black_key { Color::Rgb(50, 50, 50) } else { Color::Rgb(100, 100, 100) }; // Darker gray for white keys
                
                // Draw a full-height line for the key
                ctx.draw(&CanvasLine {
                    x1: note as f64,
                    y1: 0.0,
                    x2: note as f64, // Vertical line
                    y2: area_height_coords,
                    color,
                });
            }

            // Function to map a time Instant to a Y-coordinate in the canvas
            let map_time_to_y = |time: Instant| -> f64 {
                let time_elapsed_from_start = time.duration_since(display_start_time).as_secs_f64();
                time_elapsed_from_start // This maps directly to our y-bounds
            };

            // Draw finished notes
            for played_note in &state.finished_notes_display {
                let note_x = played_note.note as f64;
                let start_y = map_time_to_y(played_note.start_time);
                let end_y = played_note.end_time.map_or_else(
                    || map_time_to_y(now),
                    |et| map_time_to_y(et),
                );
                
                ctx.draw(&CanvasLine {
                    x1: note_x,
                    y1: start_y,
                    x2: note_x,
                    y2: end_y,
                    color: Color::Magenta, // Finished notes color
                });
            }

            // Draw currently playing notes
            for (_, played_note) in &state.currently_playing_notes {
                let note_x = played_note.note as f64;
                let start_y = map_time_to_y(played_note.start_time);
                let end_y = map_time_to_y(now); 
                
                ctx.draw(&CanvasLine {
                    x1: note_x,
                    y1: start_y,
                    x2: note_x,
                    y2: end_y,
                    color: Color::Green, // Active notes color
                });
            }
        });
    frame.render_widget(piano_roll, bottom_chunks[1]);
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

