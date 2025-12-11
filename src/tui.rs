use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, PushKeyboardEnhancementFlags, PopKeyboardEnhancementFlags, KeyboardEnhancementFlags},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    symbols::Marker,
    widgets::{Block, Borders, canvas::{Canvas, Line as CanvasLine}, Clear, List, ListItem, ListState, Paragraph},
};
use std::{
    thread,
    io::{stdout, Stdout},
    sync::{mpsc::Sender, Arc, Mutex},
    time::{Duration, Instant},
};

use crate::app::{AppMessage};
use crate::app_state::AppState;
use crate::input::MusicCommand;
use crate::tui_midi_learn::{MidiLearnTuiState, draw_midi_learn_modal};

const NUM_COLUMNS: usize = 3; // Number of columns for the stop list

#[derive(Clone, PartialEq, Eq)]
enum AppMode {
    MainApp,
    PresetSaveName(usize, String), // Holds (slot_index, current_name_buffer)
    MidiLearn,
}

/// Holds the state specific to the TUI.
struct TuiState {
    mode: AppMode,
    app_state: Arc<Mutex<AppState>>,
    list_state: ListState, // TUI-specific selection state
    items_per_column: usize,
    stops_count: usize,
    midi_learn_state: MidiLearnTuiState,
}

impl TuiState {
    fn new(app_state: Arc<Mutex<AppState>>) -> Result<Self> {
        let app_state_locked = app_state.lock().unwrap();

        let mut list_state = ListState::default();
        let stops_count = app_state_locked.organ.stops.len();
        if stops_count > 0 {
            list_state.select(Some(0)); // Select the first item
        }
        let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;

        drop(app_state_locked); // Explicitly drop the lock

        Ok(Self {
            mode: AppMode::MainApp, // Always start in MainApp
            app_state,
            list_state,
            items_per_column,
            stops_count,
            midi_learn_state: MidiLearnTuiState::default(),
        })
    }
    
    // --- TUI-specific navigation ---

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
    
    fn toggle_stop_channel(&mut self, channel: u8, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let Some(selected_index) = self.list_state.selected() {
            self.app_state.lock().unwrap().toggle_stop_channel(selected_index, channel, audio_tx)?;
        }
        Ok(())
    }

    fn select_all_channels_for_stop(&mut self) {
        if let Some(selected_index) = self.list_state.selected() {
            self.app_state.lock().unwrap().select_all_channels_for_stop(selected_index);
        }
    }

    fn select_none_channels_for_stop(&mut self, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let Some(selected_index) = self.list_state.selected() {
            self.app_state.lock().unwrap().select_none_channels_for_stop(selected_index, audio_tx)?;
        }
        Ok(())
    }
}

/// Runs the main TUI loop, blocking the main thread.
pub fn run_tui_loop(
    audio_tx: Sender<AppMessage>,
    app_state: Arc<Mutex<AppState>>,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut tui_state = TuiState::new(app_state)?;

    loop {
        thread::sleep(Duration::from_millis(10));

        // Check for incoming MIDI if in Learn Mode
        if tui_state.mode == AppMode::MidiLearn {
            tui_state.midi_learn_state.check_for_midi_input(&tui_state.app_state);
        }

        // Update piano roll state before drawing
        tui_state.app_state.lock().unwrap().update_piano_roll_state();

        // Draw UI (which now dispatches based on mode)
        terminal.draw(|f| ui(f, &mut tui_state))?;

        // Handle input
        if event::poll(Duration::from_millis(50))? {
            let event = event::read()?;


            if let Event::Key(key) = event {

                // 1. Process Music Input via the shared helper
                let command = {
                    let state = tui_state.app_state.lock().unwrap();
                    state.keyboard_layout.map_crossterm(key.code)
                };

                match command {
                    MusicCommand::OctaveUp => {
                        let mut state = tui_state.app_state.lock().unwrap();
                        state.octave_offset = state.octave_offset.saturating_add(1);
                    }
                    MusicCommand::OctaveDown => {
                        let mut state = tui_state.app_state.lock().unwrap();
                        state.octave_offset = state.octave_offset.saturating_sub(1);
                    }
                    MusicCommand::PlayNote(semitone) => {
                        let mut state = tui_state.app_state.lock().unwrap();
                        let note = state.get_keyboard_midi_note(semitone);

                        match key.kind {
                            KeyEventKind::Press => state.handle_keyboard_note(note, 100, &audio_tx),
                            KeyEventKind::Release => state.handle_keyboard_note(note, 0, &audio_tx),
                            _ => {}
                        }

                    }
                    MusicCommand::None => {
                        // Handle UI Keys (Only on Press)
                        if key.kind == KeyEventKind::Press {
                
                            match &mut tui_state.mode {
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
                                        tui_state.toggle_stop_channel(channel, &audio_tx)?;
                                    } else {
                                        // Handle other keys if no channel key was pressed
                                        match key.code {
                                            KeyCode::Char('q') | KeyCode::Esc => {
                                                audio_tx.send(AppMessage::Quit)?;
                                                break; // Exit TUI loop
                                            }
                                            KeyCode::Down => {
                                                tui_state.next_item();
                                            }
                                            KeyCode::Up => {
                                                tui_state.prev_item();
                                            }
                                            KeyCode::Right => tui_state.next_col(),
                                            KeyCode::Left => tui_state.prev_col(),
                                            KeyCode::Char('p') => {
                                                audio_tx.send(AppMessage::AllNotesOff)?;
                                            }
                                            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                                let mut state = tui_state.app_state.lock().unwrap();
                                                state.is_recording_midi = !state.is_recording_midi;
                                                if state.is_recording_midi {
                                                    audio_tx.send(AppMessage::StartMidiRecording)?;
                                                } else {
                                                    audio_tx.send(AppMessage::StopMidiRecording)?;
                                                }
                                            },
                                            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                                let mut state = tui_state.app_state.lock().unwrap();
                                                state.is_recording_audio = !state.is_recording_audio;
                                                if state.is_recording_audio {
                                                    audio_tx.send(AppMessage::StartAudioRecording)?;
                                                } else {
                                                    audio_tx.send(AppMessage::StopAudioRecording)?;
                                                }
                                            },
                                            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                                tui_state.select_all_channels_for_stop();
                                            }
                                            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                                tui_state.select_none_channels_for_stop(&audio_tx)?;
                                            }
                                            // Open MIDI learn dialog
                                            KeyCode::Char('i') => {
                                                if let Some(idx) = tui_state.list_state.selected() {
                                                    let stop_name = {
                                                        let state = tui_state.app_state.lock().unwrap();
                                                        state.organ.stops[idx].name.clone()
                                                    };
                                                    tui_state.midi_learn_state.reset(idx, stop_name);
                                                    tui_state.mode = AppMode::MidiLearn;
                                                }
                                            }
                                            KeyCode::F(n) if (1..=12).contains(&n) && key.modifiers.contains(KeyModifiers::SHIFT) => {
                                                let slot = (n - 1) as usize;
                                                let current_name = tui_state.app_state.lock().unwrap().presets[slot]
                                                    .as_ref()
                                                    .map_or_else(
                                                        || format!("Preset F{}", slot + 1),
                                                        |p| p.name.clone()
                                                    );
                                                tui_state.mode = AppMode::PresetSaveName(slot, current_name);
                                            }
                                            KeyCode::F(n) if (1..=12).contains(&n) && key.modifiers.is_empty() => {
                                                if let Err(e) = tui_state.app_state.lock().unwrap().recall_preset((n - 1) as usize, &audio_tx) {
                                                    tui_state.app_state.lock().unwrap().add_midi_log(format!("ERROR recalling preset: {}", e));
                                                }
                                            }
                                            // Gain
                                            KeyCode::Char('+') | KeyCode::Char('=') => {
                                                tui_state.app_state.lock().unwrap().modify_gain(0.05, &audio_tx);
                                            }
                                            KeyCode::Char('-') => {
                                                tui_state.app_state.lock().unwrap().modify_gain(-0.05, &audio_tx);
                                            }
                                            // Polyphony
                                            KeyCode::Char(']') => {
                                                tui_state.app_state.lock().unwrap().modify_polyphony(16, &audio_tx);
                                            }
                                            KeyCode::Char('[') => {
                                                tui_state.app_state.lock().unwrap().modify_polyphony(-16, &audio_tx);
                                            }
                                        _ => {}
                                        }
                                    }
                                },
                                AppMode::PresetSaveName(slot, name_buffer) => {
                                    match key.code {
                                        KeyCode::Enter => {
                                            if !name_buffer.is_empty() {
                                                tui_state.app_state.lock().unwrap().save_preset(*slot, name_buffer.clone());
                                            }
                                            tui_state.mode = AppMode::MainApp;
                                        }
                                        KeyCode::Char(c) => {
                                            name_buffer.push(c);
                                        }
                                        KeyCode::Backspace => {
                                            name_buffer.pop();
                                        }
                                        KeyCode::Esc => {
                                            tui_state.mode = AppMode::MainApp;
                                        }
                                        _ => {} // Ignore other keys
                                    }
                                },
                                // Handle MIDI Learn Input
                                AppMode::MidiLearn => {
                                    let keep_open = tui_state.midi_learn_state.handle_input(key.code, &tui_state.app_state);
                                    if !keep_open {
                                        tui_state.mode = AppMode::MainApp;
                                    }
                                }
                            }
                        }
                    } // non-note keyboard commmands
                }

            }
        }
    }

    cleanup_terminal()?;
    Ok(())
}

// Main App UI function
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn draw_main_app_ui(
    frame: &mut Frame, 
    app_state: &mut AppState,
    list_state: &mut ListState,
) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(70), // Stops
            Constraint::Percentage(30), // MIDI Log
            Constraint::Length(1),     // Footer
        ])
        .split(frame.area());

    let is_underrun = {
         if let Some(last) = app_state.last_underrun {
             last.elapsed() < Duration::from_millis(200)
         } else {
             false
         }
    };

    // --- Footer Help Text / Error ---

    let rec_status = if app_state.is_recording_midi && app_state.is_recording_audio {
        " [REC MIDI+WAV] "
    } else if app_state.is_recording_midi {
        " [REC MIDI] "
    } else if app_state.is_recording_audio {
        " [REC WAV] "
    } else {
        ""
    };

    let footer_widget = if let Some(err) = &app_state.error_msg {
        Paragraph::new(err.as_str())
            .style(Style::default().fg(Color::White).bg(Color::Red))
    } else if is_underrun {
        Paragraph::new("⚠ AUDIO BUFFER UNDERRUN ⚠")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD))
    } else {
        let status = format!(
            "{}CPU: {:.1}% | Gain: {:.0}% | Voices: {}/{} | [Q]uit [P]anic +/-:Gain E/R:Octave [/]:Poly F1-12:Recall Shift+F1-12:Save [I]:MIDI Learn", 
            rec_status,
            app_state.cpu_load * 100.0,
            app_state.gain * 100.0, 
            app_state.active_voice_count,
            app_state.polyphony
        );
        Paragraph::new(status).alignment(Alignment::Center)
    };
    frame.render_widget(footer_widget, main_layout[2]);

    // --- Stop List (Multi-column) ---
    const NUM_COLUMNS: usize = 3;
    let stops_area = main_layout[0];

    let column_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(stops_area);
    
    let selected_index = list_state.selected().unwrap_or(0);
    let stops_count = app_state.organ.stops.len();
    if stops_count == 0 {
        let no_stops_msg = Paragraph::new("No stops loaded.")
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(app_state.organ.name.as_str()));
        frame.render_widget(no_stops_msg, stops_area);
    } else {
        let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;
        
        let all_stops: Vec<_> = app_state.organ.stops.iter().enumerate().collect();
        
        for (col_idx, rect) in column_layout.iter().enumerate() {
            let start_idx = col_idx * items_per_column;
            let end_idx = (start_idx + items_per_column).min(stops_count);

            if start_idx >= end_idx {
                continue;
            }

            let column_items: Vec<ListItem> = all_stops[start_idx..end_idx].iter()
                .map(|(global_idx, stop)| {
                    let active_channels = app_state
                        .stop_channels
                        .get(global_idx)
                        .cloned()
                        .unwrap_or_default();

                    let mut channel_spans: Vec<Span> = Vec::with_capacity(22);

                    for i in 0..10u8 {
                        if active_channels.contains(&i) {
                            let display_num = if i == 9 { "0".to_string() } else { format!("{}", i + 1) };
                            channel_spans.push(Span::styled(
                                display_num,
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                            ));
                        } else {
                            channel_spans.push(Span::styled(
                                "■", 
                                Style::default().fg(Color::DarkGray),
                            ));
                        }
                    }

                    channel_spans.push(Span::raw(format!("    {}", stop.name)));
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
            
            let title = if col_idx == 0 { app_state.organ.name.as_str() } else { "" };
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

    let log_items: Vec<ListItem> = app_state.midi_log.iter()
        .map(|msg| ListItem::new(Line::from(msg.clone())))
        .collect();

    let log_widget = List::new(log_items)
        .block(Block::default().borders(Borders::ALL).title("MIDI Log"))
        .style(Style::default().fg(Color::Cyan));
    
    frame.render_widget(log_widget, bottom_chunks[0]);

    const PIANO_LOW_NOTE: u8 = 21;
    const PIANO_HIGH_NOTE: u8 = 108;
    const BLACK_KEY_MODS: [u8; 5] = [1, 3, 6, 8, 10];

    let now = Instant::now();
    let display_start_time = now.checked_sub(app_state.piano_roll_display_duration)
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
            app_state.piano_roll_display_duration.as_secs_f64()
        ])
        .paint(|ctx| {
            let area_height_coords = app_state.piano_roll_display_duration.as_secs_f64();

            for note in PIANO_LOW_NOTE..=PIANO_HIGH_NOTE {
                let is_black_key = BLACK_KEY_MODS.contains(&(note % 12));
                let color = if is_black_key { Color::Rgb(50, 50, 50) } else { Color::Rgb(100, 100, 100) };
                
                ctx.draw(&CanvasLine {
                    x1: note as f64,
                    y1: 0.0,
                    x2: note as f64,
                    y2: area_height_coords,
                    color,
                });
            }

            let map_time_to_y = |time: Instant| -> f64 {
                time.duration_since(display_start_time).as_secs_f64()
            };

            for played_note in &app_state.finished_notes_display {
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

            for (_, played_note) in &app_state.currently_playing_notes {
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
    let mut app_state_locked = state.app_state.lock().unwrap();
    let mode = state.mode.clone();
    match mode {
        AppMode::MainApp => draw_main_app_ui(
            frame, 
            &mut app_state_locked, 
            &mut state.list_state, 
        ),
        AppMode::PresetSaveName(slot, name_buffer) => {
            // Draw the main app in the background
            draw_main_app_ui(
                frame, 
                &mut app_state_locked, 
                &mut state.list_state, 
            );
            // Draw the modal on top
            draw_preset_save_modal(frame, slot, &name_buffer);
        },
        AppMode::MidiLearn => {
            draw_midi_learn_modal(frame, &state.midi_learn_state, &app_state_locked);
        },
    }
}

fn draw_preset_save_modal(frame: &mut Frame, slot: usize, name_buffer: &str) {
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
            format!("{}▋", name_buffer),
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

    frame.render_widget(Clear, area);
    frame.render_widget(modal_paragraph, area);
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


/// Helper to set up the terminal for TUI mode.
pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    let mut stdout = stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    // Enable Keyboard Enhancements
    // REPORT_EVENT_TYPES is required to distinguish Press vs Release
    let supports_keyboard_enhancement = matches!(
        crossterm::terminal::supports_keyboard_enhancement(), 
        Ok(true)
    );
    if supports_keyboard_enhancement {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::REPORT_EVENT_TYPES 
            )
        )?;
    }
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Helper to clean up the terminal.
pub fn cleanup_terminal() -> Result<()> {
    let mut stdout = stdout();
    
    // Disable Keyboard Enhancements
    // If we don't do this, the user's shell might act weirdly after exit.
    let supports_keyboard_enhancement = matches!(
        crossterm::terminal::supports_keyboard_enhancement(), 
        Ok(true)
    );

    if supports_keyboard_enhancement {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    execute!(stdout, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}