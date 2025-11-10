use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::sync::mpsc;
use std::sync::Arc;
use std::path::PathBuf;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::File;
use std::thread::JoinHandle;

mod app;
mod audio;
mod midi;
mod organ;
mod tui;
mod wav;
mod wav_converter;

use app::{AppMessage, TuiMessage};
use organ::Organ;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
#[value(rename_all = "lower")] // Allows users to type 'info', 'debug', etc.
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, arg_required_else_help = true)]
struct Args {
    /// Path to organ definition file (e.g., friesach/friesach.organ or friesach/OrganDefinitions/Friesach.Organ_Hauptwerk_xml)
    #[arg(value_name = "ORGAN_DEFINITION")]
    organ_file: Option<PathBuf>,

    /// Optional path to a MIDI file to play
    #[arg(value_name = "MIDI_FILE")]
    midi_file: Option<PathBuf>,

    /// Pre-cache all samples on startup (uses more memory, reduces latency)
    #[arg(long)] // Creates '--precache'
    precache: bool, // 'bool' flags are false by default

    #[arg(long)] // Creates '--convert-to-16bit'
    convert_to_16bit: bool,

    /// Set the application log level
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: LogLevel,

    /// Optional path to a convolution reverb Impulse Response (IR) file
    #[arg(long, value_name = "IR_FILE")]
    ir_file: Option<PathBuf>,

    /// Reverb mix level (0.0 = dry, 1.0 = fully wet)
    #[arg(long, value_name = "REVERB_MIX", default_value_t = 0.5)]
    reverb_mix: f32,

    /// Preserve original (de)tuning of recorded samples up to +/- 20 cents to preserve organ character
    #[arg(long)] // Creates '--preserve-tuning'
    original_tuning: bool,

    /// List all available MIDI input devices and exit
    #[arg(long)]
    list_midi_devices: bool,

    /// Select a MIDI device by name
    #[arg(long, value_name = "DEVICE_NAME")]
    midi_device: Option<String>,

    /// Audio buffer size in milliseconds (lower values reduce latency but may cause glitches)
    #[arg(long, value_name = "BUFFER_MS", default_value_t = 10)]
    audio_buffer_ms: u32,

}

fn main() -> Result<()> {
    println!("\nRusty Pipes - Virtual Pipe Organ Simulator v{}\n", env!("CARGO_PKG_VERSION"));

    let args = Args::parse();
   
    // This runs before any other setup and exits.
    if args.list_midi_devices {
        println!("Available MIDI Input Devices:");
        match midi::get_midi_device_names() {
            Ok(names) => {
                if names.is_empty() {
                    println!("  No MIDI devices found.");
                } else {
                    for (i, name) in names.iter().enumerate() {
                        println!("  {}: {}", i, name);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error fetching MIDI devices: {}", e);
            }
        }
        return Ok(()); // Exit after listing
    }

    let organ_path = match args.organ_file {
        Some(path) => path,
        None => return Err(anyhow::anyhow!("The ORGAN_DEFINITION argument is required when not using --list-midi-devices.")),
    };

    let log_level = match args.log_level {
        LogLevel::Error => LevelFilter::Error,
        LogLevel::Warn => LevelFilter::Warn,
        LogLevel::Info => LevelFilter::Info,
        LogLevel::Debug => LevelFilter::Debug,
        LogLevel::Trace => LevelFilter::Trace,
    };

    WriteLogger::init(
        log_level,
        Config::default(),
        File::create("rusty-pipes.log")?
    )?;
    let convert_to_16_bit = args.convert_to_16bit;
    let precache = args.precache;
    let midi_file_path = args.midi_file;
    let ir_file_path = args.ir_file;
    let reverb_mix = args.reverb_mix;
    let original_tuning = args.original_tuning;
    let preselected_midi_device = args.midi_device;
    let audio_buffer_ms = args.audio_buffer_ms;
    if !organ_path.exists() {
        return Err(anyhow::anyhow!("File not found: {}", organ_path.display()));
    }

    // --- Parse the organ definition ---
    // This is the immutable definition of the instrument.
    // We wrap it in an Arc to share it safely and cheaply with all threads.
    println!("Loading organ definition...");
    let organ = Arc::new(Organ::load(&organ_path, convert_to_16_bit, precache, original_tuning)?);
    println!("Successfully loaded organ: {}", organ.name);
    println!("Found {} stops.", organ.stops.len());

    // --- Create channels for thread communication ---
    // This channel sends messages *from* the MIDI and TUI threads
    // *to* the Audio processing thread.
    let (audio_tx, audio_rx) = mpsc::channel::<AppMessage>();
    // Channel for messages to the TUI thread (e.g., logs, errors)
    let (tui_tx, tui_rx) = mpsc::channel::<TuiMessage>();

    // --- Start the Audio thread ---
    // This spawns the audio processing thread and starts the cpal audio stream.
    // The `_stream` variable must be kept in scope, or audio will stop.
    println!("Starting audio engine...");
    let _stream = audio::start_audio_playback(audio_rx, Arc::clone(&organ), audio_buffer_ms)?;
    println!("Audio engine running.");

    // --- Start MIDI input ---
    // The _midi_connection is now managed inside the TUI
    let _midi_file_thread: Option<JoinHandle<()>>;
    let is_file_playback = midi_file_path.is_some(); // NEW: Tell TUI if file is playing

    if let Some(path) = midi_file_path {
        // --- Play from MIDI file ---
        if !path.exists() {
            return Err(anyhow::anyhow!("MIDI file not found: {}", path.display()));
        }
        println!("Starting MIDI file playback: {}", path.display());
        _midi_file_thread = Some(midi::play_midi_file(
            path,
            tui_tx.clone() // TUI still needs messages from file player
        )?);
    } else {
        // --- Use live MIDI input ---
        _midi_file_thread = None;
        println!("Initializing TUI for MIDI device selection...");
    }

    // --- Run the TUI on the main thread ---
    // This function will block until the user quits.
    println!("Starting TUI... Press 'q' to quit.");
    
    // Pass in tui_tx so the TUI can create the MIDI callback
    // Pass in is_file_playback to tell the TUI to skip MIDI selection
    tui::run_tui_loop(
        audio_tx,
        tui_rx,
        tui_tx,
        organ,
        ir_file_path,
        reverb_mix,
        is_file_playback,
        preselected_midi_device,
    )?;

    // --- Shutdown ---
    println!("Shutting down...");
    Ok(())
}

