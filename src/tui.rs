use anyhow::Result;
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    symbols::Marker,
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph,
        canvas::{Canvas, Line as CanvasLine},
    },
};
use rust_i18n::t;
use std::{
    io::{Stdout, stdout},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex, mpsc::Sender},
    thread,
    time::{Duration, Instant},
};

use crate::app::{AppMessage, MainLoopAction};
use crate::app_state::AppState;
use crate::config::{MidiEventSpec, load_organ_library};
use crate::input::MusicCommand;
use crate::tui_midi_learn::{MidiLearnTuiState, draw_midi_learn_modal};
use crate::tui_organ_manager::{OrganManagerTuiState, draw_organ_manager};

const NUM_COLUMNS: usize = 3; // Number of columns for the stop list

#[derive(Clone, PartialEq, Eq)]
enum AppMode {
    MainApp,
    PresetSaveName(usize, String), // Holds (slot_index, current_name_buffer)
    MidiLearn,
    OrganManager,
}

#[derive(Clone, PartialEq, Eq, Default)]
enum MainViewMode {
    #[default]
    Stops,
    Tremulants,
    Presets,
}

impl MainViewMode {
    fn next(&self) -> Self {
        match self {
            MainViewMode::Stops => MainViewMode::Tremulants,
            MainViewMode::Tremulants => MainViewMode::Presets,
            MainViewMode::Presets => MainViewMode::Stops,
        }
    }
}

/// Holds the state specific to the TUI.
struct TuiState {
    mode: AppMode,
    app_state: Arc<Mutex<AppState>>,

    // Main View Mode (Stops, Tremulants, Presets)
    main_view_mode: MainViewMode,

    // List States for each view
    stop_list_state: ListState, // Renamed from list_state
    tremulant_list_state: ListState,
    preset_list_state: ListState,

    items_per_column: usize,
    stops_count: usize,
    midi_learn_state: MidiLearnTuiState,
    organ_manager_state: OrganManagerTuiState,
}

impl TuiState {
    fn new(app_state: Arc<Mutex<AppState>>) -> Result<Self> {
        let app_state_locked = app_state.lock().unwrap();

        let mut stop_list_state = ListState::default();
        let stops_count = app_state_locked.organ.stops.len();
        if stops_count > 0 {
            stop_list_state.select(Some(0)); // Select the first item
        }
        let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;

        // Tremulant list state
        let mut tremulant_list_state = ListState::default();
        if !app_state_locked.organ.tremulants.is_empty() {
            tremulant_list_state.select(Some(0));
        }

        // Preset list state
        let mut preset_list_state = ListState::default();
        preset_list_state.select(Some(0)); // Presets are always 12 slots

        drop(app_state_locked); // Explicitly drop the lock

        Ok(Self {
            mode: AppMode::MainApp, // Always start in MainApp
            main_view_mode: MainViewMode::Stops,
            app_state,
            stop_list_state,
            tremulant_list_state,
            preset_list_state,
            items_per_column,
            stops_count, // Keeping this cached for Stops view
            midi_learn_state: MidiLearnTuiState::default(),
            organ_manager_state: OrganManagerTuiState::new(),
        })
    }

    // --- TUI-specific navigation ---

    fn next_item(&mut self) {
        match self.main_view_mode {
            MainViewMode::Stops => {
                if self.stops_count == 0 {
                    return;
                }
                let i = match self.stop_list_state.selected() {
                    Some(i) => (i + 1) % self.stops_count,
                    None => 0,
                };
                self.stop_list_state.select(Some(i));
            }
            MainViewMode::Tremulants => {
                let count = self.app_state.lock().unwrap().organ.tremulants.len();
                if count == 0 {
                    return;
                }
                let i = match self.tremulant_list_state.selected() {
                    Some(i) => (i + 1) % count,
                    None => 0,
                };
                self.tremulant_list_state.select(Some(i));
            }
            MainViewMode::Presets => {
                let count = 12; // Fixed
                let i = match self.preset_list_state.selected() {
                    Some(i) => (i + 1) % count,
                    None => 0,
                };
                self.preset_list_state.select(Some(i));
            }
        }
    }

    fn prev_item(&mut self) {
        match self.main_view_mode {
            MainViewMode::Stops => {
                if self.stops_count == 0 {
                    return;
                }
                let i = match self.stop_list_state.selected() {
                    Some(i) => {
                        if i == 0 {
                            self.stops_count - 1
                        } else {
                            i - 1
                        }
                    }
                    None => 0,
                };
                self.stop_list_state.select(Some(i));
            }
            MainViewMode::Tremulants => {
                let count = self.app_state.lock().unwrap().organ.tremulants.len();
                if count == 0 {
                    return;
                }
                let i = match self.tremulant_list_state.selected() {
                    Some(i) => {
                        if i == 0 {
                            count - 1
                        } else {
                            i - 1
                        }
                    }
                    None => 0,
                };
                self.tremulant_list_state.select(Some(i));
            }
            MainViewMode::Presets => {
                let count = 12;
                let i = match self.preset_list_state.selected() {
                    Some(i) => {
                        if i == 0 {
                            count - 1
                        } else {
                            i - 1
                        }
                    }
                    None => 0,
                };
                self.preset_list_state.select(Some(i));
            }
        }
    }

    fn next_col(&mut self) {
        if let MainViewMode::Stops = self.main_view_mode {
            if self.stops_count == 0 {
                return;
            }
            let i = match self.stop_list_state.selected() {
                Some(i) => (i + self.items_per_column).min(self.stops_count - 1),
                None => 0,
            };
            self.stop_list_state.select(Some(i));
        }
    }

    fn prev_col(&mut self) {
        if let MainViewMode::Stops = self.main_view_mode {
            let i = match self.stop_list_state.selected() {
                Some(i) => i.saturating_sub(self.items_per_column),
                None => 0,
            };
            self.stop_list_state.select(Some(i));
        }
    }

    fn toggle_stop_channel(&mut self, channel: u8, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let MainViewMode::Stops = self.main_view_mode {
            if let Some(selected_index) = self.stop_list_state.selected() {
                self.app_state.lock().unwrap().toggle_stop_channel(
                    selected_index,
                    channel,
                    audio_tx,
                )?;
            }
        }
        Ok(())
    }

    fn select_all_channels_for_stop(&mut self) {
        if let MainViewMode::Stops = self.main_view_mode {
            if let Some(selected_index) = self.stop_list_state.selected() {
                self.app_state
                    .lock()
                    .unwrap()
                    .select_all_channels_for_stop(selected_index);
            }
        }
    }

    fn select_none_channels_for_stop(&mut self, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if let MainViewMode::Stops = self.main_view_mode {
            if let Some(selected_index) = self.stop_list_state.selected() {
                self.app_state
                    .lock()
                    .unwrap()
                    .select_none_channels_for_stop(selected_index, audio_tx)?;
            }
        }
        Ok(())
    }
}

/// Runs the main TUI loop, blocking the main thread.
pub fn run_tui_loop(
    audio_tx: Sender<AppMessage>,
    app_state: Arc<Mutex<AppState>>,
    is_running: Arc<AtomicBool>,
    exit_action: Arc<Mutex<MainLoopAction>>,
) -> Result<MainLoopAction> {
    let mut terminal = setup_terminal()?;
    let mut tui_state = TuiState::new(app_state)?;
    let organ_library = load_organ_library().unwrap_or_default();

    loop {
        if !is_running.load(Ordering::Relaxed) {
            break;
        }

        thread::sleep(Duration::from_millis(10));

        // Check for incoming MIDI if in Learn Mode
        if tui_state.mode == AppMode::MidiLearn {
            tui_state
                .midi_learn_state
                .check_for_midi_input(&tui_state.app_state);
        }

        if tui_state.mode == AppMode::OrganManager {
            tui_state
                .organ_manager_state
                .check_for_midi_input(&tui_state.app_state);
        }

        if tui_state.mode == AppMode::MainApp {
            let switch_target = {
                let mut state = tui_state.app_state.lock().unwrap();
                let current_name = state.organ.name.clone();
                // Take the event to consume it (preventing double processing)
                if let Some((event, _time)) = state.last_midi_event_received.take() {
                    // Check against library
                    organ_library
                        .organs
                        .iter()
                        .find(|o| {
                            o.name != current_name
                                && o.activation_trigger.as_ref().map_or(false, |t| t == &event)
                        }) // Check Name
                        .map(|o| o.path.clone())
                } else if let Some(sysex) = state.last_sysex.take() {
                    // SysEx check
                    let event = MidiEventSpec::SysEx(sysex);
                    organ_library
                        .organs
                        .iter()
                        .find(|o| {
                            o.name != current_name
                                && o.activation_trigger.as_ref().map_or(false, |t| t == &event)
                        }) // Check Name
                        .map(|o| o.path.clone())
                } else {
                    None
                }
            };

            if let Some(path) = switch_target {
                // Found a trigger! Set reload action
                *exit_action.lock().unwrap() = MainLoopAction::ReloadOrgan { file: path };

                // Signal application to quit (shuts down audio/logic threads)
                audio_tx.send(AppMessage::Quit)?;

                // Break TUI loop immediately
                break;
            }
        }
        // Update piano roll state before drawing
        tui_state
            .app_state
            .lock()
            .unwrap()
            .update_piano_roll_state();

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
                                    match key.code {
                                        KeyCode::Tab => {
                                            tui_state.main_view_mode =
                                                tui_state.main_view_mode.next();
                                        }
                                        KeyCode::Char('O')
                                            if key.modifiers.contains(KeyModifiers::SHIFT) =>
                                        {
                                            tui_state.mode = AppMode::OrganManager;
                                        }
                                        KeyCode::Char('i') => {
                                            match tui_state.main_view_mode {
                                                MainViewMode::Stops => {
                                                    if let Some(idx) =
                                                        tui_state.stop_list_state.selected()
                                                    {
                                                        let stop_name = {
                                                            let state =
                                                                tui_state.app_state.lock().unwrap();
                                                            state.organ.stops[idx].name.clone()
                                                        };
                                                        tui_state
                                                            .midi_learn_state
                                                            .reset_stop(idx, stop_name);
                                                        tui_state.mode = AppMode::MidiLearn;
                                                    }
                                                }
                                                MainViewMode::Tremulants => {
                                                    // Need to get Tremulant ID by index
                                                    if let Some(idx) =
                                                        tui_state.tremulant_list_state.selected()
                                                    {
                                                        let (id, name) = {
                                                            let state =
                                                                tui_state.app_state.lock().unwrap();
                                                            let mut trems: Vec<_> = state
                                                                .organ
                                                                .tremulants
                                                                .values()
                                                                .collect();
                                                            trems.sort_by_key(|t| &t.name);
                                                            if let Some(t) = trems.get(idx) {
                                                                (t.id_str.clone(), t.name.clone())
                                                            } else {
                                                                (String::new(), String::new())
                                                            }
                                                        };
                                                        if !id.is_empty() {
                                                            tui_state
                                                                .midi_learn_state
                                                                .reset_tremulant(id, name);
                                                            tui_state.mode = AppMode::MidiLearn;
                                                        }
                                                    }
                                                }
                                                MainViewMode::Presets => {
                                                    if let Some(slot) =
                                                        tui_state.preset_list_state.selected()
                                                    {
                                                        tui_state
                                                            .midi_learn_state
                                                            .reset_preset(slot);
                                                        tui_state.mode = AppMode::MidiLearn;
                                                    }
                                                }
                                            }
                                        }
                                        // Space toggle for Tremulants
                                        KeyCode::Char(' ') | KeyCode::Enter => {
                                            #[allow(clippy::single_match)]
                                            match tui_state.main_view_mode {
                                                MainViewMode::Tremulants => {
                                                    if let Some(idx) =
                                                        tui_state.tremulant_list_state.selected()
                                                    {
                                                        let id = {
                                                            let state =
                                                                tui_state.app_state.lock().unwrap();
                                                            let mut trems: Vec<_> = state
                                                                .organ
                                                                .tremulants
                                                                .values()
                                                                .collect();
                                                            trems.sort_by_key(|t| &t.name);
                                                            trems.get(idx).map(|t| t.id_str.clone())
                                                        };

                                                        if let Some(id) = id {
                                                            let mut state =
                                                                tui_state.app_state.lock().unwrap();
                                                            let active = state
                                                                .active_tremulants
                                                                .contains(&id);
                                                            state.set_tremulant_active(
                                                                id, !active, &audio_tx,
                                                            );
                                                        }
                                                    }
                                                }
                                                MainViewMode::Presets => {
                                                    if let Some(slot) =
                                                        tui_state.preset_list_state.selected()
                                                    {
                                                        let _ = tui_state
                                                            .app_state
                                                            .lock()
                                                            .unwrap()
                                                            .recall_preset(slot, &audio_tx);
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }

                                        // Standard Stop Toggles (Channels 1-9)
                                        KeyCode::Char(c)
                                            if c.is_ascii_digit()
                                                && c != '0'
                                                && matches!(
                                                    tui_state.main_view_mode,
                                                    MainViewMode::Stops
                                                ) =>
                                        {
                                            let channel = c as u8 - b'1';
                                            tui_state.toggle_stop_channel(channel, &audio_tx)?;
                                        }
                                        KeyCode::Char('0')
                                            if matches!(
                                                tui_state.main_view_mode,
                                                MainViewMode::Stops
                                            ) =>
                                        {
                                            tui_state.toggle_stop_channel(9, &audio_tx)?;
                                        }

                                        // Passthrough to other keys
                                        _ => {
                                            match key.code {
                                                KeyCode::Char('q') | KeyCode::Esc => {
                                                    audio_tx.send(AppMessage::Quit)?;
                                                    *exit_action.lock().unwrap() =
                                                        MainLoopAction::Exit;
                                                    break; // Exit TUI loop
                                                }
                                                KeyCode::Down => tui_state.next_item(),
                                                KeyCode::Up => tui_state.prev_item(),
                                                KeyCode::Right => tui_state.next_col(),
                                                KeyCode::Left => tui_state.prev_col(),
                                                KeyCode::Char('p') => {
                                                    audio_tx.send(AppMessage::AllNotesOff)?;
                                                }
                                                KeyCode::Char('m')
                                                    if key
                                                        .modifiers
                                                        .contains(KeyModifiers::SHIFT) =>
                                                {
                                                    let mut state =
                                                        tui_state.app_state.lock().unwrap();
                                                    state.is_recording_midi =
                                                        !state.is_recording_midi;
                                                    if state.is_recording_midi {
                                                        audio_tx
                                                            .send(AppMessage::StartMidiRecording)?;
                                                    } else {
                                                        audio_tx
                                                            .send(AppMessage::StopMidiRecording)?;
                                                    }
                                                }
                                                KeyCode::Char('r')
                                                    if key
                                                        .modifiers
                                                        .contains(KeyModifiers::SHIFT) =>
                                                {
                                                    let mut state =
                                                        tui_state.app_state.lock().unwrap();
                                                    state.is_recording_audio =
                                                        !state.is_recording_audio;
                                                    if state.is_recording_audio {
                                                        audio_tx.send(
                                                            AppMessage::StartAudioRecording,
                                                        )?;
                                                    } else {
                                                        audio_tx
                                                            .send(AppMessage::StopAudioRecording)?;
                                                    }
                                                }
                                                KeyCode::Char('a')
                                                    if key
                                                        .modifiers
                                                        .contains(KeyModifiers::SHIFT) =>
                                                {
                                                    tui_state.select_all_channels_for_stop();
                                                }
                                                KeyCode::Char('n')
                                                    if key
                                                        .modifiers
                                                        .contains(KeyModifiers::SHIFT) =>
                                                {
                                                    tui_state
                                                        .select_none_channels_for_stop(&audio_tx)?;
                                                }
                                                KeyCode::F(n)
                                                    if (1..=12).contains(&n)
                                                        && key
                                                            .modifiers
                                                            .contains(KeyModifiers::SHIFT) =>
                                                {
                                                    let slot = (n - 1) as usize;
                                                    let current_name =
                                                        tui_state.app_state.lock().unwrap().presets
                                                            [slot]
                                                            .as_ref()
                                                            .map_or_else(
                                                                || {
                                                                    t!(
                                                                    "gui.default_preset_name_fmt",
                                                                    num = slot + 1
                                                                )
                                                                    .to_string()
                                                                },
                                                                |p| p.name.clone(),
                                                            );
                                                    tui_state.mode =
                                                        AppMode::PresetSaveName(slot, current_name);
                                                }
                                                KeyCode::F(n)
                                                    if (1..=12).contains(&n)
                                                        && key.modifiers.is_empty() =>
                                                {
                                                    if let Err(e) = tui_state
                                                        .app_state
                                                        .lock()
                                                        .unwrap()
                                                        .recall_preset((n - 1) as usize, &audio_tx)
                                                    {
                                                        tui_state
                                                            .app_state
                                                            .lock()
                                                            .unwrap()
                                                            .add_midi_log(
                                                                t!(
                                                                    "errors.recall_preset_fail",
                                                                    err = e
                                                                )
                                                                .to_string(),
                                                            );
                                                    }
                                                }
                                                // Gain
                                                KeyCode::Char('+') | KeyCode::Char('=') => {
                                                    tui_state
                                                        .app_state
                                                        .lock()
                                                        .unwrap()
                                                        .modify_gain(0.05, &audio_tx);
                                                }
                                                KeyCode::Char('-') => {
                                                    tui_state
                                                        .app_state
                                                        .lock()
                                                        .unwrap()
                                                        .modify_gain(-0.05, &audio_tx);
                                                }
                                                // Polyphony
                                                KeyCode::Char(']') => {
                                                    tui_state
                                                        .app_state
                                                        .lock()
                                                        .unwrap()
                                                        .modify_polyphony(16, &audio_tx);
                                                }
                                                KeyCode::Char('[') => {
                                                    tui_state
                                                        .app_state
                                                        .lock()
                                                        .unwrap()
                                                        .modify_polyphony(-16, &audio_tx);
                                                }
                                                _ => {}
                                            }
                                        } // end passthrough
                                    }
                                }
                                AppMode::PresetSaveName(slot, name_buffer) => {
                                    match key.code {
                                        KeyCode::Enter => {
                                            if !name_buffer.is_empty() {
                                                tui_state
                                                    .app_state
                                                    .lock()
                                                    .unwrap()
                                                    .save_preset(*slot, name_buffer.clone());
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
                                }
                                // Handle MIDI Learn Input
                                AppMode::MidiLearn => {
                                    let keep_open = tui_state
                                        .midi_learn_state
                                        .handle_input(key.code, &tui_state.app_state);
                                    if !keep_open {
                                        tui_state.mode = AppMode::MainApp;
                                    }
                                }
                                AppMode::OrganManager => {
                                    if tui_state.organ_manager_state.handle_input(
                                        key.code,
                                        &mut terminal,
                                        &exit_action,
                                        &tui_state.app_state,
                                    ) {
                                        // Check if we need to reload (handled via exit_action) or just switch back
                                        // If exit_action is Reload, run_tui_loop logic at end will catch it.
                                        // If just closed, switch mode back.
                                        if let MainLoopAction::ReloadOrgan { .. } =
                                            *exit_action.lock().unwrap()
                                        {
                                            break;
                                        }
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
    let action = exit_action.lock().unwrap().clone();
    Ok(action)
}

// Main App UI function
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn draw_main_app_ui(frame: &mut Frame, app_state: &mut AppState, tui_state: &mut TuiState) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),      // Tabs
            Constraint::Percentage(70), // Content (Stops/Trems/Presets)
            Constraint::Percentage(30), // MIDI Log
            Constraint::Length(1),      // Footer
        ])
        .split(frame.area());

    // --- Tabs ---
    let titles = vec!["Stops", "Tremulants", "Presets"];
    let selected_tab = match tui_state.main_view_mode {
        MainViewMode::Stops => 0,
        MainViewMode::Tremulants => 1,
        MainViewMode::Presets => 2,
    };

    let tabs = ratatui::widgets::Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(app_state.organ.name.as_str()),
        )
        .select(selected_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(tabs, main_layout[0]);

    // --- Content ---
    let content_area = main_layout[1];
    match tui_state.main_view_mode {
        MainViewMode::Stops => draw_stops_view(
            frame,
            content_area,
            app_state,
            &mut tui_state.stop_list_state,
        ),
        MainViewMode::Tremulants => draw_tremulants_view(
            frame,
            content_area,
            app_state,
            &mut tui_state.tremulant_list_state,
        ),
        MainViewMode::Presets => draw_presets_view(
            frame,
            content_area,
            app_state,
            &mut tui_state.preset_list_state,
        ),
    }

    // --- Bottom Area (MIDI Log + Piano Roll) ---
    // Note: main_layout indices shifted by 1 due to Tabs
    let bottom_area = main_layout[2];
    let bottom_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20), // MIDI Log
            Constraint::Percentage(80), // Piano Roll
        ])
        .split(bottom_area);

    let log_items: Vec<ListItem> = app_state
        .midi_log
        .iter()
        .map(|msg| ListItem::new(Line::from(msg.clone())))
        .collect();

    let log_widget = List::new(log_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(t!("tui.midi_log_title").to_string()),
        )
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(log_widget, bottom_chunks[0]);

    const PIANO_LOW_NOTE: u8 = 21;
    const PIANO_HIGH_NOTE: u8 = 108;
    const BLACK_KEY_MODS: [u8; 5] = [1, 3, 6, 8, 10];

    let now = Instant::now();
    let display_start_time = now
        .checked_sub(app_state.piano_roll_display_duration)
        .unwrap_or(Instant::now());

    let piano_roll = Canvas::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(t!("tui.piano_roll_title").to_string()),
        )
        .marker(Marker::Block)
        .x_bounds([PIANO_LOW_NOTE as f64, PIANO_HIGH_NOTE as f64 + 1.0])
        .y_bounds([0.0, app_state.piano_roll_display_duration.as_secs_f64()])
        .paint(|ctx| {
            let area_height_coords = app_state.piano_roll_display_duration.as_secs_f64();

            for note in PIANO_LOW_NOTE..=PIANO_HIGH_NOTE {
                let is_black_key = BLACK_KEY_MODS.contains(&(note % 12));
                let color = if is_black_key {
                    Color::Rgb(50, 50, 50)
                } else {
                    Color::Rgb(100, 100, 100)
                };

                ctx.draw(&CanvasLine {
                    x1: note as f64,
                    y1: 0.0,
                    x2: note as f64,
                    y2: area_height_coords,
                    color,
                });
            }

            let map_time_to_y =
                |time: Instant| -> f64 { time.duration_since(display_start_time).as_secs_f64() };

            for played_note in &app_state.finished_notes_display {
                let note_x = played_note.note as f64;
                let start_y = map_time_to_y(played_note.start_time);
                let end_y = played_note
                    .end_time
                    .map_or_else(|| map_time_to_y(now), |et| map_time_to_y(et));

                ctx.draw(&CanvasLine {
                    x1: note_x,
                    y1: start_y,
                    x2: note_x,
                    y2: end_y,
                    color: Color::Magenta,
                });
            }

            for (_, played_note) in &app_state.currently_playing_notes {
                let note_x = played_note.note as f64;
                let start_y = map_time_to_y(played_note.start_time);
                let end_y = map_time_to_y(now);

                ctx.draw(&CanvasLine {
                    x1: note_x,
                    y1: start_y,
                    x2: note_x,
                    y2: end_y,
                    color: Color::Green,
                });
            }
        });
    frame.render_widget(piano_roll, bottom_chunks[1]);

    let is_underrun = {
        if let Some(last) = app_state.last_underrun {
            last.elapsed() < Duration::from_millis(200)
        } else {
            false
        }
    };

    // --- Footer Help Text / Error ---

    let rec_status = if app_state.is_recording_midi && app_state.is_recording_audio {
        t!("tui.status_rec_midi_wav").to_string()
    } else if app_state.is_recording_midi {
        t!("tui.status_rec_midi").to_string()
    } else if app_state.is_recording_audio {
        t!("tui.status_rec_wav").to_string()
    } else {
        "".to_string()
    };

    let footer_widget = if let Some(err) = &app_state.error_msg {
        Paragraph::new(err.as_str()).style(Style::default().fg(Color::White).bg(Color::Red))
    } else if is_underrun {
        Paragraph::new(t!("tui.err_underrun").to_string())
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )
    } else {
        let status = t!(
            "tui.status_bar_fmt",
            rec = rec_status,
            cpu = format!("{:.1}", app_state.cpu_load * 100.0),
            gain = format!("{:.0}", app_state.gain * 100.0),
            active = app_state.active_voice_count,
            poly = app_state.polyphony
        )
        .to_string();

        Paragraph::new(status).alignment(Alignment::Center)
    };
    frame.render_widget(footer_widget, main_layout[3]);
}

fn draw_stops_view(
    frame: &mut Frame,
    area: Rect,
    app_state: &AppState,
    list_state: &mut ListState,
) {
    let stops_count = app_state.organ.stops.len();
    if stops_count == 0 {
        let msg = Paragraph::new("No Stops").alignment(Alignment::Center);
        frame.render_widget(msg, area);
        return;
    }

    let column_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);

    let selected_index = list_state.selected().unwrap_or(0);
    let items_per_column = (stops_count + NUM_COLUMNS - 1) / NUM_COLUMNS;
    let all_stops: Vec<_> = app_state.organ.stops.iter().enumerate().collect();

    for (col_idx, rect) in column_layout.iter().enumerate() {
        let start_idx = col_idx * items_per_column;
        let end_idx = (start_idx + items_per_column).min(stops_count);

        if start_idx >= end_idx {
            continue;
        }

        let column_items: Vec<ListItem> = all_stops[start_idx..end_idx]
            .iter()
            .map(|(global_idx, stop)| {
                let active_channels = app_state
                    .stop_channels
                    .get(global_idx)
                    .cloned()
                    .unwrap_or_default();

                let mut channel_spans: Vec<Span> = Vec::with_capacity(22);

                for i in 0..10u8 {
                    if active_channels.contains(&i) {
                        let display_num = if i == 9 {
                            "0".to_string()
                        } else {
                            format!("{}", i + 1)
                        };
                        channel_spans.push(Span::styled(
                            display_num,
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        channel_spans.push(Span::styled("■", Style::default().fg(Color::DarkGray)));
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
        frame.render_widget(
            List::new(column_items).block(Block::default().borders(Borders::LEFT)),
            *rect,
        );
    }
}

fn draw_tremulants_view(
    frame: &mut Frame,
    area: Rect,
    app_state: &AppState,
    list_state: &mut ListState,
) {
    // Sort tremulants to match navigation order
    let mut trems: Vec<_> = app_state.organ.tremulants.values().collect();
    trems.sort_by_key(|t| &t.name);

    if trems.is_empty() {
        let msg = Paragraph::new("No Tremulants").alignment(Alignment::Center);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = trems
        .iter()
        .map(|trem| {
            let active = app_state.active_tremulants.contains(&trem.id_str);
            let status = if active { "[ON] " } else { "[   ] " };
            let content = format!("{}{}", status, trem.name);

            // Check if midi learned
            let learned = app_state
                .midi_control_map
                .tremulants
                .contains_key(&trem.id_str);
            let learned_mark = if learned { " (M)" } else { "" };

            let style = if active {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };

            ListItem::new(format!("{}{}", content, learned_mark)).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Tremulants"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));

    frame.render_stateful_widget(list, area, list_state);
}

fn draw_presets_view(
    frame: &mut Frame,
    area: Rect,
    app_state: &AppState,
    list_state: &mut ListState,
) {
    let items: Vec<ListItem> = (0..12)
        .map(|i| {
            let preset_name = app_state.presets[i]
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "Empty".to_string());

            let learned = app_state.midi_control_map.presets.contains_key(&i);
            let learned_mark = if learned { " (M)" } else { "" };

            let content = format!("F{:<2}: {}{}", i + 1, preset_name, learned_mark);
            ListItem::new(content)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Presets"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));

    frame.render_stateful_widget(list, area, list_state);
}

/// Renders the UI frame.
fn ui(frame: &mut Frame, state: &mut TuiState) {
    let app_state_arc = state.app_state.clone();
    let mut app_state_locked = app_state_arc.lock().unwrap();
    let mode = state.mode.clone(); // Clone mode to avoid borrowing state during match
    match mode {
        AppMode::MainApp => draw_main_app_ui(frame, &mut app_state_locked, state),
        AppMode::PresetSaveName(slot, name_buffer) => {
            // Draw the main app in the background
            draw_main_app_ui(frame, &mut app_state_locked, state);
            // Draw the modal on top
            draw_preset_save_modal(frame, slot, &name_buffer);
        }
        AppMode::MidiLearn => {
            draw_midi_learn_modal(frame, &state.midi_learn_state, &app_state_locked);
        }
        AppMode::OrganManager => {
            draw_organ_manager(
                frame,
                &mut state.organ_manager_state,
                &app_state_locked.organ.name,
            );
        }
    }
}

fn draw_preset_save_modal(frame: &mut Frame, slot: usize, name_buffer: &str) {
    let area = centered_rect(frame.area(), 60, 20);
    let slot_display = slot + 1;

    let text = vec![
        Line::from(Span::styled(
            t!("tui.save_header_fmt", num = slot_display).to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(t!("tui.save_prompt").to_string()),
        Line::from(""),
        Line::from(Span::styled(
            format!("{}▋", name_buffer),
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(Span::styled(
            t!("tui.save_footer").to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let modal_block = Block::default()
        .title(t!("tui.save_title").to_string())
        .borders(Borders::ALL);
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
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
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
