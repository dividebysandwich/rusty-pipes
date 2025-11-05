use anyhow::{anyhow, Result};
use midir::{MidiInput, MidiInputConnection, Ignore};
use std::io::{stdin, stdout, Write};
use std::sync::mpsc::Sender;
use std::path::PathBuf;
use std::fs;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use midly::{Smf, TrackEventKind, MidiMessage as MidlyMidiMessage, MetaMessage};

use crate::app::{AppMessage, TuiMessage};

/// Formats any MIDI message as a readable string.
fn format_midi_message(message: &[u8]) -> String {
    let mut s = String::new();
    for (i, byte) in message.iter().enumerate() {
        s.push_str(&format!("0x{:02X}", byte));
        if i < message.len() - 1 {
            s.push(' ');
        }
    }

    // Add a basic interpretation
    match message.get(0) {
        Some(0x90..=0x9F) => s.push_str(" (Note On)"),
        Some(0x80..=0x8F) => s.push_str(" (Note Off)"),
        Some(0xB0..=0xBF) => s.push_str(" (Control Change)"),
        Some(0xE0..=0xEF) => s.push_str(" (Pitch Bend)"),
        _ => s.push_str(" (Other)"),
    }
    s
}

pub fn setup_midi_input(
    audio_tx: Sender<AppMessage>,
    tui_tx: Sender<TuiMessage>,
) -> Result<MidiInputConnection<()>> {
    let mut midi_in = MidiInput::new("grandorgue-rs-input")?;
    midi_in.ignore(Ignore::ActiveSense);

    let in_ports = midi_in.ports();
    let in_port = match in_ports.len() {
        0 => return Err(anyhow!("No MIDI input ports found!")),
        1 => {
            println!("Choosing the only available MIDI port: {}", midi_in.port_name(&in_ports[0])?);
            &in_ports[0]
        },
        _ => {
            println!("\nAvailable MIDI input ports:");
            for (i, p) in in_ports.iter().enumerate() {
                println!("{}: {}", i, midi_in.port_name(p)?);
            }
            print!("Please select port number: ");
            stdout().flush()?;
            let mut input = String::new();
            stdin().read_line(&mut input)?;
            let port_index: usize = input.trim().parse()?;
            in_ports.get(port_index).ok_or_else(|| anyhow!("Invalid port number"))?
        }
    };

    println!("Opening MIDI connection...");
    let port_name = midi_in.port_name(in_port)?;

    let connection = midi_in.connect(in_port, &port_name, move |_timestamp, message, _| {
        // 1. Log the formatted message to the TUI thread
        let log_msg = format_midi_message(message);
        // We don't want to panic if the TUI is gone, so we ignore the error
        let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
        
        // 2. Parse and send to Audio thread
        if message.len() >= 3 {
            match message[0] {
                0x90..=0x9F => { // Note On (channel 1-16)
                    let note = message[1];
                    let velocity = message[2];
                    
                    if velocity > 0 {
                        // This is a real Note On
                        audio_tx.send(AppMessage::NoteOn(note, velocity)).unwrap_or_else(|e| {
                            let _ = tui_tx.send(TuiMessage::Error(format!("Failed to send NoteOn: {}", e)));
                        });
                        let _ = tui_tx.send(TuiMessage::TuiNoteOn(note));
                    } else {
                        // Note On with velocity 0 is a Note Off
                         audio_tx.send(AppMessage::NoteOff(note)).unwrap_or_else(|e| {
                            let _ = tui_tx.send(TuiMessage::Error(format!("Failed to send NoteOff: {}", e)));
                        });
                        let _ = tui_tx.send(TuiMessage::TuiNoteOff(note));
                    }
                },
                0x80..=0x8F => { // Note Off (channel 1-16)
                    let note = message[1];
                    audio_tx.send(AppMessage::NoteOff(note)).unwrap_or_else(|e| {
                        let _ = tui_tx.send(TuiMessage::Error(format!("Failed to send NoteOff: {}", e)));
                    });
                    let _ = tui_tx.send(TuiMessage::TuiNoteOff(note));
                },
                _ => {} // Ignore other messages
            }
        }
    }, ())
    .map_err(|e| anyhow!("Failed to connect to MIDI input: {}", e))?;
    
    Ok(connection)
}

/// Spawns a new thread to play a MIDI file.
pub fn play_midi_file(
    path: PathBuf,
    audio_tx: Sender<AppMessage>,
    tui_tx: Sender<TuiMessage>,
) -> Result<JoinHandle<()>> {

    let handle = thread::spawn(move || {
        // Load and parse the MIDI file
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                let _ = tui_tx.send(TuiMessage::Error(format!("Failed to read MIDI file: {}", e)));
                return;
            }
        };

        let smf = match Smf::parse(&data) {
            Ok(s) => s,
            Err(e) => {
                let _ = tui_tx.send(TuiMessage::Error(format!("Failed to parse MIDI file: {}", e)));
                return;
            }
        };

        let tpqn = match smf.header.timing {
            midly::Timing::Metrical(t) => t.as_int() as f64,
            _ => {
                let _ = tui_tx.send(TuiMessage::Error("Unsupported MIDI timing format (must be Metrical/TPQN)".into()));
                return;
            }
        };
        
        // Set up playback state
        // Default tempo: 120 BPM = 500,000 microseconds per quarter note
        let mut micros_per_quarter = 500_000.0;
        
        // Create peekable iterators for each track
        let mut tracks: Vec<_> = smf.tracks.iter()
            .map(|track| track.iter().peekable())
            .collect();
        
        // Store the absolute tick time for the *next* event in each track
        let mut track_next_event_times: Vec<u32> = vec![0; tracks.len()];
        let mut global_ticks: u32 = 0;

        let _ = tui_tx.send(TuiMessage::MidiLog(format!("Starting playback of {}...", path.display())));

        // Start the playback loop
        loop {
            let mut next_event_tick = u32::MAX;
            let mut next_track_idx = None;

            // Find the track with the earliest upcoming event
            for (i, track_iter) in tracks.iter_mut().enumerate() {
                if let Some(event) = track_iter.peek() {
                    let event_time = track_next_event_times[i] + event.delta.as_int();
                    if event_time < next_event_tick {
                        next_event_tick = event_time;
                        next_track_idx = Some(i);
                    }
                }
            }

            // Get the index of the track with the next event
            let track_idx = match next_track_idx {
                Some(idx) => idx,
                None => break, // All tracks are finished
            };

            // This is safe because we peeked
            let event = tracks[track_idx].next().unwrap();
            
            // Update this track's "next event time"
            track_next_event_times[track_idx] = next_event_tick;
            
            // Calculate time to wait since the last event
            let ticks_to_wait = next_event_tick - global_ticks;
            global_ticks = next_event_tick;

            if ticks_to_wait > 0 {
                let micros_per_tick = micros_per_quarter / tpqn;
                let wait_micros = (ticks_to_wait as f64 * micros_per_tick) as u64;
                thread::sleep(Duration::from_micros(wait_micros));
            }

            // Process the MIDI event
            match event.kind {
                TrackEventKind::Midi { channel, message } => {
                    match message {
                        MidlyMidiMessage::NoteOn { key, vel } => {
                            let key = key.as_int();
                            let vel = vel.as_int();
                            let log_msg = format!("0x9{} 0x{:02X} 0x{:02X} (Note On from file)", channel.as_int(), key, vel);
                            let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                            let _ = tui_tx.send(TuiMessage::TuiNoteOn(key));
                            if vel > 0 {
                                audio_tx.send(AppMessage::NoteOn(key, vel))
                            } else {
                                // Velocity 0 is a Note Off
                                audio_tx.send(AppMessage::NoteOff(key))
                            }.unwrap_or_else(|e| {
                                let _ = tui_tx.send(TuiMessage::Error(format!("File player failed to send message: {}", e)));
                            });
                        },
                        MidlyMidiMessage::NoteOff { key, vel: _ } => {
                            let key = key.as_int();
                            let log_msg = format!("0x8{} 0x{:02X} 0x00 (Note Off from file)", channel.as_int(), key);
                            let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                            let _ = tui_tx.send(TuiMessage::TuiNoteOff(key));
                            audio_tx.send(AppMessage::NoteOff(key)).unwrap_or_else(|e| {
                                let _ = tui_tx.send(TuiMessage::Error(format!("File player failed to send NoteOff: {}", e)));
                            });
                        },
                        MidlyMidiMessage::Controller { controller, value: _ } => {
                            // CC #123 is "All Notes Off"
                            if controller.as_int() == 123 {
                                let log_msg = format!("0xB{} 0x7B 0x00 (All Notes Off from file)", channel.as_int());
                                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                                
                                audio_tx.send(AppMessage::AllNotesOff).unwrap_or_else(|e| {
                                    let _ = tui_tx.send(TuiMessage::Error(format!("File player failed to send AllNotesOff: {}", e)));
                                });
                                let _ = tui_tx.send(TuiMessage::TuiAllNotesOff);
                            }
                            // TODO: Handle Sustain command (CC #64)
                        },
                        _ => {} // Ignore other MIDI messages (CC, etc.)
                    }
                },
                TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                    micros_per_quarter = micros.as_int() as f64;
                    let _ = tui_tx.send(TuiMessage::MidiLog(format!("Tempo set to {} μs/quarter", micros.as_int())));
                },
                _ => {} // Ignore Sysex or other meta events
            }
        }
        
        let _ = tui_tx.send(TuiMessage::MidiLog("MIDI file playback finished.".into()));
    });

    Ok(handle)
}