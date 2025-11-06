use anyhow::Result;
use std::sync::mpsc;
use std::sync::Arc;
use std::path::PathBuf;
use std::env;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::File;
use std::thread::JoinHandle;
use midir::MidiInputConnection;

mod app;
mod audio;
mod midi;
mod organ;
mod tui;
mod wav_converter;

use app::{AppMessage, TuiMessage};
use organ::Organ;

fn main() -> Result<()> {
    WriteLogger::init(
        LevelFilter::Debug,
        Config::default(),
        File::create("rusty-pipes.log")?
    )?;
    // --- Get .organ file from command line arguments ---
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <path-to-organ-file.organ>", args[0]);
        return Err(anyhow::anyhow!("Missing .organ file argument"));
    }
    let organ_path = PathBuf::from(&args[1]);
    if !organ_path.exists() {
        return Err(anyhow::anyhow!("File not found: {}", organ_path.display()));
    }

    // Check for optional MIDI file path
    let midi_file_path: Option<PathBuf> = args.get(2).map(PathBuf::from);

    // --- Parse the organ definition ---
    // This is the immutable definition of the instrument.
    // We wrap it in an Arc to share it safely and cheaply with all threads.
    println!("Loading organ definition...");
    let organ = Arc::new(Organ::load(&organ_path)?);
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
            audio_tx.clone(),
            tui_tx.clone()
        )?);
        _midi_connection = None;
    } else {
        // --- Use live MIDI input ---
        println!("Initializing MIDI...");
        _midi_connection = Some(midi::setup_midi_input(audio_tx.clone(), tui_tx.clone())?);
        _midi_file_thread = None;
        println!("MIDI input enabled.");
    }

    // --- Run the TUI on the main thread ---
    // This function will block until the user quits.
    // It takes ownership of its own sender to send messages (StopToggle, Quit).
    println!("Starting TUI... Press 'q' to quit.");
    tui::run_tui_loop(audio_tx, tui_rx, organ)?;

    // --- Shutdown ---
    // When run_tui_loop returns (on quit), main exits.
    // `_stream` and `_midi_connection` are dropped, cleaning up their threads.
    println!("Shutting down...");
    Ok(())
}

