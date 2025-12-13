use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use std::time::Duration;
use std::sync::{Arc, Mutex};
use midir::MidiInput;

use crate::config::{AppSettings, ConfigState, RuntimeConfig};
use crate::tui::{cleanup_terminal, setup_terminal};
use crate::tui_filepicker;
use crate::tui_midi;
use crate::app::{PIPES, LOGO};
use crate::audio::get_supported_sample_rates;

#[derive(Clone)]
enum ConfigMode {
    Main,
    AudioSelection,
    SampleRateSelection,
    IrSelection, // Reverb Impulse Response File selection
    TextInput(usize, String), // Holds (config_index, buffer)
    MidiDeviceList,         // List of detected devices
    MidiMapping(usize),     // Editing device at specific index in settings.midi_devices
}

struct TuiConfigState {
    config_state: ConfigState,
    list_state: ListState,
    audio_list_state: ListState,
    sample_rate_list_state: ListState,
    ir_list_state: ListState,
    midi_dev_list_state: ListState,
    midi_mapping_state: tui_midi::TuiMidiState,
    mode: ConfigMode,
}

#[derive(Copy, Clone, PartialEq)]
enum SettingRow {
    OrganFile = 0,
    MidiDevices = 1,
    MidiFile = 2,
    AudioDevice = 3,
    SampleRate = 4,
    ReverbIRFile = 5,
    ReverbMix = 6,
    Gain = 7,
    Polyphony = 8,
    AudioBuffer = 9,
    PreloadFrames = 10,
    Precache = 11,
    ConvertTo16Bit = 12,
    OriginalTuning = 13,
    Start = 14,
    Quit = 15,
}

impl SettingRow {
    // The "Safe Helper"
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::OrganFile),
            1 => Some(Self::MidiDevices),
            2 => Some(Self::MidiFile),
            3 => Some(Self::AudioDevice),
            4 => Some(Self::SampleRate),
            5 => Some(Self::ReverbIRFile),
            6 => Some(Self::ReverbMix),
            7 => Some(Self::Gain),
            8 => Some(Self::Polyphony),
            9 => Some(Self::AudioBuffer),
            10 => Some(Self::PreloadFrames),
            11 => Some(Self::Precache),
            12 => Some(Self::ConvertTo16Bit),
            13 => Some(Self::OriginalTuning),
            14 => Some(Self::Start),
            15 => Some(Self::Quit),
            _ => None,
        }
    }
}

// Helper to get the display string for a config item
fn get_item_display(idx: usize, state: &ConfigState) -> String {
    let settings = &state.settings;
    let row = SettingRow::from_index(idx).unwrap(); // Safe unwrap
    match row {
        SettingRow::OrganFile => format!("Organ File:       {}", path_to_str(settings.organ_file.as_deref())),
        SettingRow::AudioDevice => format!("Audio Device:     {}", state.selected_audio_device_name.as_deref().unwrap_or("Default")),
        SettingRow::SampleRate => format!("Sample Rate:      {} Hz", settings.sample_rate),
        SettingRow::MidiDevices => format!("MIDI Input Devices:   {} enabled", settings.midi_devices.iter().filter(|d| d.enabled).count()),
        SettingRow::MidiFile => format!("MIDI File (Play): {}", path_to_str(state.midi_file.as_deref())),
        SettingRow::ReverbIRFile => format!("Reverb IR File:   {}", path_to_str(settings.ir_file.as_deref())),
        SettingRow::ReverbMix => format!("Reverb Mix:       {:.2}", settings.reverb_mix),
        SettingRow::Gain => format!("Gain:             {:.2}", settings.gain),
        SettingRow::Polyphony => format!("Polyphony:        {}", settings.polyphony),
        SettingRow::AudioBuffer => format!("Audio Buffer:     {} frames", settings.audio_buffer_frames),
        SettingRow::PreloadFrames => format!("Preload Frames:   {} frames", settings.preload_frames),
        SettingRow::Precache => format!("Pre-cache:        {}", bool_to_str(settings.precache)),
        SettingRow::ConvertTo16Bit => format!("Convert to 16-bit:{}", bool_to_str(settings.convert_to_16bit)),
        SettingRow::OriginalTuning => format!("Original Tuning:  {}", bool_to_str(settings.original_tuning)),
        SettingRow::Start => "Start Rusty Pipes".to_string(),
        SettingRow::Quit => "Quit".to_string(),
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

    let initial_audio_index = config_state.selected_audio_device_name.as_ref()
        .and_then(|selected_name| {
            config_state.available_audio_devices.iter().position(|name| name == selected_name)
        })
        .map_or(0, |i| i + 1); // 0 is "[ Default ]"
    
    let mut audio_list_state = ListState::default();
    audio_list_state.select(Some(initial_audio_index));

    // Setup Reverb IR list state
    let initial_ir_index = config_state.settings.ir_file.as_ref()
        .and_then(|current_path| {
             config_state.available_ir_files.iter().position(|(_, path)| path == current_path)
        })
        .map(|i| i + 1) // +1 because 0 is "None"
        .unwrap_or(0);

    let mut ir_list_state = ListState::default();
    ir_list_state.select(Some(initial_ir_index));

    let mut state = TuiConfigState {
        config_state,
        list_state: ListState::default(),
        audio_list_state,
        midi_dev_list_state: ListState::default(),
        midi_mapping_state: tui_midi::TuiMidiState::new(),
        sample_rate_list_state: ListState::default(),
        ir_list_state, // New
        mode: ConfigMode::Main,
    };
    state.list_state.select(Some(0));
    state.midi_dev_list_state.select(Some(0));

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

            let current_mode = state.mode.clone();

            match current_mode {
                ConfigMode::Main => {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break 'config_loop,
                        KeyCode::Down | KeyCode::Char('j') => {
                            let i = state.list_state.selected().map_or(0, |i| (i + 1) % 16);
                            state.list_state.select(Some(i));
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let i = state.list_state.selected().map_or(15, |i| (i + 15) % 16);
                            state.list_state.select(Some(i));
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = state.list_state.selected() {
                                match SettingRow::from_index(idx).unwrap() {
                                    SettingRow::OrganFile => { // Organ File
                                        let path = tui_filepicker::run_file_picker(
                                            &mut terminal,
                                            "Select Organ File",
                                            &["organ", "Organ_Hauptwerk_xml", "xml"],
                                        )?;
                                        if let Some(p) = path {
                                            state.config_state.settings.organ_file = Some(p);
                                        }
                                    }
                                    SettingRow::AudioDevice => { // Audio Device
                                        state.mode = ConfigMode::AudioSelection;
                                    }
                                    SettingRow::SampleRate => { 
                                        state.mode = ConfigMode::SampleRateSelection; 
                                    }
                                    SettingRow::MidiDevices => {
                                        state.mode = ConfigMode::MidiDeviceList;
                                    },
                                    SettingRow::MidiFile => { // MIDI File
                                        let path = tui_filepicker::run_file_picker(
                                            &mut terminal,
                                            "Select MIDI File (Optional)",
                                            &["mid", "midi"],
                                        )?;
                                        state.config_state.midi_file = path;
                                    }
                                    SettingRow::ReverbIRFile => { // IR File
                                        state.mode = ConfigMode::IrSelection;
                                    }
                                    SettingRow::ReverbMix => { // Reverb Mix
                                        let buffer = state.config_state.settings.reverb_mix.to_string();
                                        state.mode = ConfigMode::TextInput(idx, buffer);
                                    }
                                    SettingRow::Gain => { // Gain
                                        let gain = state.config_state.settings.gain.to_string();
                                        state.mode = ConfigMode::TextInput(idx, gain);
                                    }
                                    SettingRow::Polyphony => { // Polyphony
                                        let polyphony = state.config_state.settings.polyphony.to_string();
                                        state.mode = ConfigMode::TextInput(idx, polyphony);
                                    }
                                    SettingRow::AudioBuffer => { // Audio Buffer
                                        let buffer = state.config_state.settings.audio_buffer_frames.to_string();
                                        state.mode = ConfigMode::TextInput(idx, buffer);
                                    }
                                    SettingRow::PreloadFrames => { // Preload Frames
                                        let buffer = state.config_state.settings.preload_frames.to_string();
                                        state.mode = ConfigMode::TextInput(idx, buffer);
                                    }
                                    SettingRow::Precache => state.config_state.settings.precache = !state.config_state.settings.precache,
                                    SettingRow::ConvertTo16Bit => state.config_state.settings.convert_to_16bit = !state.config_state.settings.convert_to_16bit,
                                    SettingRow::OriginalTuning => state.config_state.settings.original_tuning = !state.config_state.settings.original_tuning,
                                    SettingRow::Start => { // Start
                                        if state.config_state.settings.organ_file.is_none() {
                                            state.config_state.error_msg = Some("Please select an Organ File to start.".to_string());
                                        } else {
                                            let s = &state.config_state.settings;
                                            // Collect active devices
                                            let mut active_devices = Vec::new();
                                            for (port, name) in &state.config_state.system_midi_ports {
                                                if let Some(cfg) = s.midi_devices.iter().find(|d| d.name == *name) {
                                                    if cfg.enabled {
                                                        active_devices.push((port.clone(), cfg.clone()));
                                                    }
                                                }
                                            }
                                            final_config = Some(RuntimeConfig {
                                                organ_file: s.organ_file.clone().unwrap(),
                                                ir_file: s.ir_file.clone(),
                                                reverb_mix: s.reverb_mix,
                                                audio_buffer_frames: s.audio_buffer_frames,
                                                preload_frames: s.preload_frames,
                                                precache: s.precache,
                                                convert_to_16bit: s.convert_to_16bit,
                                                original_tuning: s.original_tuning,
                                                midi_file: state.config_state.midi_file.clone(),
                                                active_midi_devices: active_devices,
                                                gain: s.gain,
                                                polyphony: s.polyphony,
                                                audio_device_name: state.config_state.selected_audio_device_name.clone(),
                                                sample_rate: s.sample_rate,
                                            });
                                            break 'config_loop;
                                        }
                                    }
                                    SettingRow::Quit => break 'config_loop, // Quit
                                }
                            }
                        }
                        _ => {}
                    }
                }
                ConfigMode::MidiDeviceList => {
                    let count = state.config_state.system_midi_ports.len();
                    match key.code {
                        KeyCode::Esc => state.mode = ConfigMode::Main,
                        KeyCode::Down | KeyCode::Char('j') => {
                            if count > 0 {
                                let i = state.midi_dev_list_state.selected().map_or(0, |i| (i + 1) % count);
                                state.midi_dev_list_state.select(Some(i));
                            }
                        },
                        KeyCode::Up | KeyCode::Char('k') => {
                            if count > 0 {
                                let i = state.midi_dev_list_state.selected().map_or(count - 1, |i| (i + count - 1) % count);
                                state.midi_dev_list_state.select(Some(i));
                            }
                        },
                        KeyCode::Char(' ') => {
                            // Toggle Enabled
                            if let Some(idx) = state.midi_dev_list_state.selected() {
                                if let Some((_, name)) = state.config_state.system_midi_ports.get(idx) {
                                    if let Some(cfg) = state.config_state.settings.midi_devices.iter_mut().find(|d| d.name == *name) {
                                        cfg.enabled = !cfg.enabled;
                                    }
                                }
                            }
                        },
                        KeyCode::Enter => {
                            // Go to Mapping
                            if let Some(idx) = state.midi_dev_list_state.selected() {
                                if let Some((_, name)) = state.config_state.system_midi_ports.get(idx) {
                                    // We need to find the index in settings.midi_devices
                                    if let Some(cfg_idx) = state.config_state.settings.midi_devices.iter().position(|d| d.name == *name) {
                                        // Reset the midi state for the new device
                                        state.midi_mapping_state = tui_midi::TuiMidiState::new();
                                        state.mode = ConfigMode::MidiMapping(cfg_idx);
                                    }
                                }
                            }
                        },
                        _ => {}
                    }
                },
                ConfigMode::MidiMapping(idx) => {
                    let action = if let Some(device) = state.config_state.settings.midi_devices.get_mut(idx) {
                        tui_midi::handle_input(key, &mut state.midi_mapping_state, device)
                    } else {
                        tui_midi::MappingAction::Back
                    };

                    match action {
                        tui_midi::MappingAction::Back => state.mode = ConfigMode::MidiDeviceList,
                        _ => {}
                    }
                },
                ConfigMode::AudioSelection => {
                    match key.code {
                        KeyCode::Esc => state.mode = ConfigMode::Main,
                        KeyCode::Down | KeyCode::Char('j') => {
                            let len = state.config_state.available_audio_devices.len() + 1; // +1 for "Default"
                            if len > 0 {
                                let i = state.audio_list_state.selected().map_or(0, |i| (i + 1) % len);
                                state.audio_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let len = state.config_state.available_audio_devices.len() + 1; // +1 for "Default"
                            if len > 0 {
                                let i = state.audio_list_state.selected().map_or(len - 1, |i| (i + len - 1) % len);
                                state.audio_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = state.audio_list_state.selected() {
                                // Update selection
                                if idx == 0 {
                                    state.config_state.selected_audio_device_name = None;
                                } else {
                                    state.config_state.selected_audio_device_name = state.config_state.available_audio_devices.get(idx - 1).cloned();
                                }
                                
                                // REFRESH RATES
                                if let Ok(rates) = get_supported_sample_rates(state.config_state.selected_audio_device_name.clone()) {
                                    state.config_state.available_sample_rates = rates;
                                    // Reset current selection if invalid
                                    if !state.config_state.available_sample_rates.contains(&state.config_state.settings.sample_rate) {
                                        if let Some(&first) = state.config_state.available_sample_rates.first() {
                                            state.config_state.settings.sample_rate = first;
                                        }
                                    }
                                }
                            }
                            state.mode = ConfigMode::Main;
                        }
                        _ => {}
                    }
                }
                ConfigMode::SampleRateSelection => {
                    match key.code {
                        KeyCode::Esc => state.mode = ConfigMode::Main,
                        KeyCode::Down | KeyCode::Char('j') => {
                            let len = state.config_state.available_sample_rates.len();
                            if len > 0 {
                                let i = state.sample_rate_list_state.selected().map_or(0, |i| (i + 1) % len);
                                state.sample_rate_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let len = state.config_state.available_sample_rates.len();
                            if len > 0 {
                                let i = state.sample_rate_list_state.selected().map_or(len - 1, |i| (i + len - 1) % len);
                                state.sample_rate_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = state.sample_rate_list_state.selected() {
                                if let Some(&rate) = state.config_state.available_sample_rates.get(idx) {
                                    state.config_state.settings.sample_rate = rate;
                                }
                            }
                            state.mode = ConfigMode::Main;
                        }
                        _ => {}
                    }
                }
                ConfigMode::IrSelection => {
                    match key.code {
                        KeyCode::Esc => state.mode = ConfigMode::Main,
                        KeyCode::Down | KeyCode::Char('j') => {
                            let len = state.config_state.available_ir_files.len() + 1; // +1 for "None"
                            if len > 0 {
                                let i = state.ir_list_state.selected().map_or(0, |i| (i + 1) % len);
                                state.ir_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let len = state.config_state.available_ir_files.len() + 1;
                            if len > 0 {
                                let i = state.ir_list_state.selected().map_or(len - 1, |i| (i + len - 1) % len);
                                state.ir_list_state.select(Some(i));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = state.ir_list_state.selected() {
                                if idx == 0 {
                                    state.config_state.settings.ir_file = None;
                                } else {
                                    if let Some((_, path)) = state.config_state.available_ir_files.get(idx - 1) {
                                        state.config_state.settings.ir_file = Some(path.clone());
                                    }
                                }
                            }
                            state.mode = ConfigMode::Main;
                        }
                        _ => {}
                    }
                }
                ConfigMode::TextInput(idx, mut buffer) => {
                    match key.code {
                        KeyCode::Char(c) => {
                            buffer.push(c);
                            state.mode = ConfigMode::TextInput(idx, buffer); // Update buffer in state
                        }
                        KeyCode::Backspace => { 
                            buffer.pop(); 
                            state.mode = ConfigMode::TextInput(idx, buffer);
                        }
                        KeyCode::Esc => state.mode = ConfigMode::Main,
                        KeyCode::Enter => {
                            match SettingRow::from_index(idx).unwrap() {
                                SettingRow::ReverbMix => { 
                                    if let Ok(val) = buffer.parse::<f32>() {
                                        state.config_state.settings.reverb_mix = val.clamp(0.0, 1.0);
                                    }
                                }
                                SettingRow::Gain => {
                                    if let Ok(val) = buffer.parse::<f32>() {
                                        state.config_state.settings.gain = val.clamp(0.0, 1.0);
                                    }
                                }
                                SettingRow::Polyphony => {
                                    if let Ok(val) = buffer.parse::<usize>() {
                                        state.config_state.settings.polyphony = val.clamp(1, 1024);
                                    }
                                }
                                SettingRow::AudioBuffer => {
                                    if let Ok(val) = buffer.parse::<usize>() {
                                        state.config_state.settings.audio_buffer_frames = val;
                                    }
                                }
                                SettingRow::PreloadFrames => {
                                    if let Ok(val) = buffer.parse::<usize>() {
                                        state.config_state.settings.preload_frames = val;
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

    // Check if we are in a fullscreen sub-mode (Mapping)
    if let ConfigMode::MidiMapping(idx) = state.mode {
        // Clone device to avoid double borrow (one mutable for state, one immutable for device)
        if let Some(dev) = state.config_state.settings.midi_devices.get(idx).cloned() {
             tui_midi::draw(frame, area, &mut state.midi_mapping_state, &dev);
             return;
        }
    }

    // --- Calculate new header height ---
    let pipes_lines = PIPES.lines().count(); // 7
    let logo_lines_count = LOGO.lines().count(); // 5
    // 7(pipes) + 5(logo) + 1(indicia) + 1(blank) + 1(title) + 2(borders) = 17
    let header_height = (pipes_lines + logo_lines_count + 5) as u16;

    // --- Main Layout ---
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
    let num_config_items = SettingRow::Quit as usize + 1;
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

    // --- Modals ---
    match state.mode {
        ConfigMode::MidiDeviceList => {
             draw_midi_device_list(frame, state);
        },
        ConfigMode::AudioSelection => {
            let mut items = vec!["[ Default ]".to_string()];
            items.extend(state.config_state.available_audio_devices.iter().cloned());
            draw_modal_list(
                frame,
                "Select Audio Device (↑/↓, Enter, Esc)",
                &items,
                &mut state.audio_list_state,
            );
        }
        ConfigMode::SampleRateSelection => {
             let items: Vec<String> = state.config_state.available_sample_rates.iter()
                .map(|r| format!("{}", r))
                .collect();
             draw_modal_list(
                frame,
                "Select Sample Rate",
                &items,
                &mut state.sample_rate_list_state
             );
        }
        ConfigMode::IrSelection => {
             let mut items = vec!["[ No Reverb ]".to_string()];
             items.extend(state.config_state.available_ir_files.iter().map(|(name, _)| name.clone()));
             
             draw_modal_list(
                frame,
                "Select Impulse Response",
                &items,
                &mut state.ir_list_state
             );
        }
        ConfigMode::TextInput(idx, ref buffer) => {
            let title = match SettingRow::from_index(idx).unwrap() {
                SettingRow::ReverbMix => "Enter Reverb Mix (0.0 - 1.0)",
                SettingRow::Gain => "Enter Gain (0.0 - 1.0)",
                SettingRow::Polyphony => "Enter Polyphony (1 - 1024)",
                SettingRow::AudioBuffer => "Enter Audio Buffer Size",
                SettingRow::PreloadFrames => "Enter Preload Frames",
                _ => "Enter Value",
            };
            draw_text_input_modal(frame, title, buffer, 40, 3);
        }
        _ => {}
    }
}

fn draw_midi_device_list(frame: &mut Frame, state: &mut TuiConfigState) {
    let area = centered_rect(frame.area(), 60, 60);
    
    let items: Vec<ListItem> = state.config_state.system_midi_ports.iter().map(|(_, name)| {
        // Find config status
        let enabled = if let Some(cfg) = state.config_state.settings.midi_devices.iter().find(|d| d.name == *name) {
            cfg.enabled
        } else { false };
        
        let checkbox = if enabled { "[x]" } else { "[ ]" };
        ListItem::new(format!("{} {}", checkbox, name))
    }).collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" MIDI Devices ")
        .title_bottom(" Space: Toggle | Enter: Configure | Esc: Back ");
        
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black));
        
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state.midi_dev_list_state);
}

fn draw_modal_list(
    frame: &mut Frame,
    title: &str,
    items: &[String],
    list_state: &mut ListState,
) {
    let area = centered_rect(frame.area(), 60, 50);

    let items: Vec<ListItem> = items.iter().map(|name| ListItem::new(name.clone())).collect();

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