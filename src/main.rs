use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::sync::mpsc;
use std::sync::Arc;
use std::path::PathBuf;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::File;
use std::thread::JoinHandle;
use midir::MidiInputConnection;

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
    /// Path to the pipe organ definition file (e.g., organs/friesach/friesach.organ)
    #[arg(value_name = "ORGAN_DEFINITION")]
    organ_file: PathBuf,

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
}

fn main() -> Result<()> {
    let args = Args::parse();
   
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
    let organ_path = args.organ_file;
    let convert_to_16_bit = args.convert_to_16bit;
    let precache = args.precache;
    let midi_file_path = args.midi_file;
    let ir_file_path = args.ir_file;
    if !organ_path.exists() {
        return Err(anyhow::anyhow!("File not found: {}", organ_path.display()));
    }

    // --- Parse the organ definition ---
    // This is the immutable definition of the instrument.
    // We wrap it in an Arc to share it safely and cheaply with all threads.
    println!("Loading organ definition...");
    let organ = Arc::new(Organ::load(&organ_path, convert_to_16_bit, precache)?);
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
    let _stream = audio::start_audio_playback(audio_rx, Arc::clone(&organ))?;
    println!("Audio engine running.");

    // --- Start the MIDI input ---
    // This sets up the MIDI callback.
    // The `_midi_connection` must also be kept in scope.
    let _midi_connection: Option<MidiInputConnection<()>>;
    let _midi_file_thread: Option<JoinHandle<()>>;

    if let Some(path) = midi_file_path {
        // --- Play from MIDI file ---
        if !path.exists() {
            return Err(anyhow::anyhow!("MIDI file not found: {}", path.display()));
        }
        println!("Starting MIDI file playback: {}", path.display());
        _midi_file_thread = Some(midi::play_midi_file(
            path,
            tui_tx.clone()
        )?);
        _midi_connection = None;
    } else {
        // --- Use live MIDI input ---
        println!("Initializing MIDI...");
        _midi_connection = Some(midi::setup_midi_input(tui_tx.clone())?);
        _midi_file_thread = None;
        println!("MIDI input enabled.");
    }

    // --- Run the TUI on the main thread ---
    // This function will block until the user quits.
    // It takes ownership of its own sender to send messages (StopToggle, Quit).
    println!("Starting TUI... Press 'q' to quit.");
    tui::run_tui_loop(audio_tx, tui_rx, organ, ir_file_path)?;

    // --- Shutdown ---
    // When run_tui_loop returns (on quit), main exits.
    // `_stream` and `_midi_connection` are dropped, cleaning up their threads.
    println!("Shutting down...");
    Ok(())
}

