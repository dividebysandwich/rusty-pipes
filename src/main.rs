use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::sync::mpsc;
use std::sync::Arc;
use std::path::PathBuf;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::File;
use std::thread::{self, JoinHandle};
use std::sync::Mutex;
use midir::{MidiInput, MidiInputConnection};

mod app;
mod audio;
mod midi;
mod organ;
mod tui;
mod wav;
mod wav_converter;
mod app_state;
mod gui;
mod gui_filepicker;
mod tui_filepicker;
mod config;
mod tui_config;
mod gui_config;

use app::{AppMessage, TuiMessage};
use app_state::{AppState, connect_to_midi};
use organ::Organ;
use config::{AppSettings, RuntimeConfig};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
#[value(rename_all = "lower")]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to organ definition file (e.g., friesach/friesach.organ or friesach/OrganDefinitions/Friesach.Organ_Hauptwerk_xml)
    #[arg(value_name = "ORGAN_DEFINITION")]
    organ_file: Option<PathBuf>,

    /// Optional path to a MIDI file to play
    #[arg(long = "midi-file", value_name = "MIDI_FILE")]
    midi_file: Option<PathBuf>,

    /// Pre-cache all samples on startup (uses more memory, reduces latency)
    #[arg(long)]
    precache: Option<bool>, // Made Option to allow overriding

    /// Convert all samples to 16-bit PCM on load (saves memory, may reduce quality)
    #[arg(long)]
    convert_to_16bit: Option<bool>, // Made Option

    /// Set the application log level
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: LogLevel,

    /// Optional path to a convolution reverb Impulse Response (IR) file
    #[arg(long, value_name = "IR_FILE")]
    ir_file: Option<PathBuf>,

    /// Reverb mix level (0.0 = dry, 1.0 = fully wet)
    #[arg(long, value_name = "REVERB_MIX")]
    reverb_mix: Option<f32>,

    /// Preserve original (de)tuning of recorded samples up to +/- 20 cents to preserve organ character
    #[arg(long)]
    original_tuning: Option<bool>,

    /// List all available MIDI input devices and exit
    #[arg(long)]
    list_midi_devices: bool,

    /// Select a MIDI device by name
    #[arg(long, value_name = "DEVICE_NAME")]
    midi_device: Option<String>,

    /// Audio buffer size in frames (lower values reduce latency but may cause glitches)
    #[arg(long, value_name = "NUM_FRAMES")]
    audio_buffer_frames: Option<usize>,
    
    /// Run in terminal UI (TUI) mode as a fallback
    #[arg(long)]
    tui: bool,
}

#[cfg_attr(feature = "hotpath", hotpath::main(percentiles = [99]))]
fn main() -> Result<()> {
    let args = Args::parse();

    // --- Setup logging ---
    let log_level = match args.log_level {
        LogLevel::Error => LevelFilter::Error,
        LogLevel::Warn => LevelFilter::Warn,
        LogLevel::Info => LevelFilter::Info,
        LogLevel::Debug => LevelFilter::Debug,
        LogLevel::Trace => LevelFilter::Trace,
    };
    WriteLogger::init(log_level, Config::default(), File::create("rusty-pipes.log")?)?;

    // --- List MIDI devices and exit ---
    if args.list_midi_devices {
        println!("Available MIDI Input Devices:");
        match midi::get_midi_device_names() {
            Ok(names) => {
                if names.is_empty() { println!("  No MIDI devices found."); }
                else { for (i, name) in names.iter().enumerate() { println!("  {}: {}", i, name); } }
            }
            Err(e) => { eprintln!("Error fetching MIDI devices: {}", e); }
        }
        return Ok(());
    }

    let midi_input_arc = Arc::new(Mutex::new(match MidiInput::new("Rusty Pipes MIDI Input") {
        Ok(mi) => Some(mi),
        Err(e) => {
            log::error!("Failed to initialize MIDI: {}", e);
            None
        }
    }));

    // --- Load Config and Merge CLI Args ---
    let mut settings = config::load_settings().unwrap_or_default();
    let tui_mode = args.tui;

    
    // Command-line arguments override saved config
    if let Some(f) = args.organ_file { settings.organ_file = Some(f); }
    if let Some(f) = args.ir_file { settings.ir_file = Some(f); }
    if let Some(m) = args.reverb_mix { settings.reverb_mix = m; }
    if let Some(b) = args.audio_buffer_frames { settings.audio_buffer_frames = b; }
    if let Some(p) = args.precache { settings.precache = p; }
    if let Some(c) = args.convert_to_16bit { settings.convert_to_16bit = c; }
    if let Some(o) = args.original_tuning { settings.original_tuning = o; }

    // --- Run Configuration UI ---
    let config_result = if tui_mode {
        tui_config::run_config_ui(settings, Arc::clone(&midi_input_arc))
    } else {
        gui_config::run_config_ui(settings, Arc::clone(&midi_input_arc))
    };
    
    // `config` is the final, user-approved configuration
    let config: RuntimeConfig = match config_result {
        Ok(Some(config)) => config,
        Ok(None) => {
            // User quit the config screen
            println!("Configuration cancelled. Exiting.");
            return Ok(());
        }
        Err(e) => {
            // Need to make sure TUI is cleaned up if it failed
            if args.tui {
                let _ = tui::cleanup_terminal();
            }
            log::error!("Error in config UI: {}", e);
            return Err(e);
        }
    };
    
    // --- Save Final Settings (excluding runtime options) ---
    let settings_to_save = AppSettings {
        organ_file: Some(config.organ_file.clone()),
        ir_file: config.ir_file.clone(),
        reverb_mix: config.reverb_mix,
        audio_buffer_frames: config.audio_buffer_frames,
        precache: config.precache,
        convert_to_16bit: config.convert_to_16bit,
        original_tuning: config.original_tuning,
        midi_device_name: config.midi_port_name.clone(),
        gain: config.gain,
        tui_mode,
    };
    if let Err(e) = config::save_settings(&settings_to_save) {
        log::warn!("Failed to save settings: {}", e);
    }

    // --- APPLICATION STARTUP ---
    
    if tui_mode {
        println!("\nRusty Pipes - Virtual Pipe Organ Simulator v{}\n", env!("CARGO_PKG_VERSION"));
    }

    // --- Parse the organ definition ---
    if tui_mode { println!("Loading organ definition..."); }
    let organ = Arc::new(Organ::load(
        &config.organ_file, 
        config.convert_to_16bit, 
        config.precache, 
        config.original_tuning
    )?);
    if tui_mode {
        println!("Successfully loaded organ: {}", organ.name);
        println!("Found {} stops.", organ.stops.len());
    }

    // --- Create channels for thread communication ---
    let (audio_tx, audio_rx) = mpsc::channel::<AppMessage>();
    let (tui_tx, tui_rx) = mpsc::channel::<TuiMessage>();

    // --- Start the Audio thread ---
    if tui_mode { println!("Starting audio engine..."); }
    let _stream = audio::start_audio_playback(
        audio_rx, 
        Arc::clone(&organ), 
        config.audio_buffer_frames
    )?;
    if tui_mode { println!("Audio engine running."); }
    
    // --- Load IR file ---
    if let Some(path) = config.ir_file {
        if path.exists() {
            log::info!("Loading IR file: {}", path.display());
            audio_tx.send(AppMessage::SetReverbIr(path))?;
            audio_tx.send(AppMessage::SetReverbWetDry(config.reverb_mix))?;
        } else {
            log::warn!("IR file not found: {}", path.display());
        }
    }

    // --- Create thread-safe AppState ---
    let app_state = Arc::new(Mutex::new(AppState::new(organ.clone())?));

    // --- Spawn the dedicated MIDI logic thread ---
    let logic_app_state = Arc::clone(&app_state);
    let logic_audio_tx = audio_tx.clone();
    let _logic_thread = thread::spawn(move || {
        log::info!("MIDI logic thread started.");
        // This is a blocking loop, it waits for messages from either the MIDI callback or the file player.
        while let Ok(msg) = tui_rx.recv() {
            // Yield
            thread::yield_now();
            // Lock the state, handle the message, then unlock.
            let mut app_state_locked = logic_app_state.lock().unwrap();
            if let Err(e) = app_state_locked.handle_tui_message(msg, &logic_audio_tx) {
                let err_msg = format!("Error handling TUI message: {}", e);
                log::error!("{}", err_msg);
                app_state_locked.add_midi_log(err_msg);
            }
        }
        log::info!("MIDI logic thread shutting down.");
    });

    // --- Start MIDI input ---
    let _midi_file_thread: Option<JoinHandle<()>>;
    let mut _midi_connection: Option<MidiInputConnection<()>> = None;

    // We take the MidiInput object back from the Arc *after* the config UI is closed.
    let mut midi_input_opt = midi_input_arc.lock().unwrap().take();

    if let Some(path) = config.midi_file {
        // --- Play from MIDI file ---
        if tui_mode { println!("Starting MIDI file playback: {}", path.display()); }
        _midi_file_thread = Some(midi::play_midi_file(path, tui_tx.clone())?);
    } else if let (Some(port), Some(name), Some(midi_input)) = (
        config.midi_port,
        config.midi_port_name,
        midi_input_opt.take() // Use the MidiInput we just took from the Arc
    ) {
        // --- Use live MIDI input ---
        if tui_mode { println!("Connecting to MIDI device: {}", name); }
        // The `midi_input` and `port` are now guaranteed to be from the same instance.
        _midi_connection = Some(connect_to_midi(midi_input, &port, &name, &tui_tx)?);
        app_state.lock().unwrap().add_midi_log(format!("Connected to: {}", name));
        _midi_file_thread = None;
    } else {
        // --- No MIDI file or device ---
        if tui_mode { println!("No MIDI file or device selected. Running without MIDI input."); }
        _midi_file_thread = None;
    }

    // --- Run the TUI or GUI on the main thread ---
    if tui_mode {
        tui::run_tui_loop(
            audio_tx,
            Arc::clone(&app_state),
        )?;
    } else {
        log::info!("Starting GUI...");
        gui::run_gui_loop(
            audio_tx,
            Arc::clone(&app_state),
            tui_tx,
            organ,
            _midi_connection, // Pass the connection to the GUI
        )?;
    }

    // --- Shutdown ---
    if tui_mode { println!("Shutting down..."); }
    log::info!("Shutting down...");
    Ok(())
}