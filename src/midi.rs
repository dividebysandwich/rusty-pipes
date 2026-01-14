use anyhow::Result;
use midir::{MidiInput, MidiInputPort};
use midly::{MetaMessage, MidiMessage as MidlyMidiMessage, Smf, TrackEventKind};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::app::TuiMessage;
use crate::config::{MidiDeviceConfig, MidiMappingMode};
use crate::midi_recorder::MidiRecorder;

/// Returns a list of all available MIDI input device names.
pub fn get_midi_device_names() -> Result<Vec<String>> {
    let midi_in = MidiInput::new("rusty-pipes-lister")?;
    let mut names = Vec::new();
    for port in midi_in.ports() {
        names.push(midi_in.port_name(&port)?);
    }
    Ok(names)
}

/// Converts a MIDI note number to its name (e.g., 60 -> "C4").
pub fn midi_note_to_name(note: u8) -> String {
    const NOTES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = (note / 12).saturating_sub(1); // MIDI note 0 is C-1
    let note_name = NOTES[(note % 12) as usize];
    format!("{}{}", note_name, octave)
}

/// Connects to a specific MIDI port with a specific configuration.
pub fn connect_to_midi(
    midi_input: MidiInput,
    port: &MidiInputPort,
    device_name: &str,
    tui_tx: &Sender<TuiMessage>,
    config: MidiDeviceConfig,
    shared_recorder: Arc<Mutex<Option<MidiRecorder>>>,
) -> Result<midir::MidiInputConnection<()>> {
    let tx_clone = tui_tx.clone();
    let name_clone = device_name.to_string();

    midi_input
        .connect(
            port,
            device_name,
            move |_, message, _| {
                process_live_midi_message(
                    message,
                    &tx_clone,
                    &config,
                    &name_clone,
                    &shared_recorder,
                );
            },
            (),
        )
        .map_err(|e| anyhow::anyhow!("Failed to connect to MIDI device {}: {}", device_name, e))
}

/// Processes raw MIDI bytes, applies channel mapping, and sends events to the App.
fn process_live_midi_message(
    message: &[u8],
    tui_tx: &Sender<TuiMessage>,
    config: &MidiDeviceConfig,
    _device_name: &str, // Useful if you want to log *which* device sent the message
    shared_recorder: &Arc<Mutex<Option<MidiRecorder>>>,
) {
    if message.len() < 3 {
        return;
    }

    let status = message[0];
    // Ignore system real-time messages (0xF8-0xFF)
    if status >= 0xF8 {
        return;
    }

    if status == 0xF0 {
        let _ = tui_tx.send(TuiMessage::MidiSysEx(message.to_vec()));
        return;
    }

    let raw_channel = status & 0x0F;
    let msg_type = status & 0xF0;

    // --- APPLY MAPPING ---
    let target_channel = match config.mapping_mode {
        MidiMappingMode::Simple => config.simple_target_channel,
        MidiMappingMode::Complex => {
            // Safety check for array bounds (0-15)
            config
                .complex_mapping
                .get(raw_channel as usize)
                .cloned()
                .unwrap_or(raw_channel)
        }
    };

    if let Ok(mut recorder_guard) = shared_recorder.lock() {
        if let Some(recorder) = recorder_guard.as_mut() {
            // Record using the MAPPED target_channel, not the raw_channel
            recorder.record(target_channel, status, message[1], message[2]);
        }
    }

    // Reconstruct the status byte with the new channel
    let new_status = msg_type | (target_channel & 0x0F);

    // Create new message buffer
    let mut mapped_message = Vec::with_capacity(message.len());
    mapped_message.push(new_status);
    mapped_message.extend_from_slice(&message[1..]);

    // Pass to the parser
    parse_and_send(&mapped_message, tui_tx, target_channel);
}

/// Parses the (mapped) message and sends TUI/Audio events.
fn parse_and_send(message: &[u8], tui_tx: &Sender<TuiMessage>, channel: u8) {
    let now = Instant::now();
    let status = message[0];

    match status & 0xF0 {
        0x90 => {
            // Note On
            let note = message[1];
            let velocity = message[2];
            if velocity > 0 {
                let note_name = midi_note_to_name(note);
                let log_msg = format!(
                    "Note On: {} (Ch {}, Vel {})",
                    note_name,
                    channel + 1,
                    velocity
                );
                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                let _ = tui_tx.send(TuiMessage::MidiNoteOn(note, velocity, channel));
                let _ = tui_tx.send(TuiMessage::TuiNoteOn(note, channel, now));
            } else {
                // Velocity 0 = Note Off
                let note_name = midi_note_to_name(note);
                let log_msg = format!("Note Off: {} (Ch {})", note_name, channel + 1);
                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                let _ = tui_tx.send(TuiMessage::MidiNoteOff(note, channel));
                let _ = tui_tx.send(TuiMessage::TuiNoteOff(note, channel, now));
            }
        }
        0x80 => {
            // Note Off
            let note = message[1];
            let note_name = midi_note_to_name(note);
            let log_msg = format!("Note Off: {} (Ch {})", note_name, channel + 1);
            let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
            let _ = tui_tx.send(TuiMessage::MidiNoteOff(note, channel));
            let _ = tui_tx.send(TuiMessage::TuiNoteOff(note, channel, now));
        }
        0xB0 => {
            // Control Change
            let controller = message[1];
            if controller == 123 {
                // All Notes Off
                let log_msg = format!("All Off (Ch {})", channel + 1);
                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                let _ = tui_tx.send(TuiMessage::MidiChannelNotesOff(channel));
                let _ = tui_tx.send(TuiMessage::TuiAllNotesOff);
            }
        }
        _ => {}
    }
}

// Helper to scan file for total duration in seconds
fn get_midi_duration_seconds(smf: &Smf) -> f64 {
    let mut duration = 0.0;
    let mut micros_per_quarter = 500_000.0;

    let tpqn = match smf.header.timing {
        midly::Timing::Metrical(t) => t.as_int() as f64,
        _ => return 0.0,
    };

    // Estimate duration by dry-running all MIDI events
    let mut tracks: Vec<_> = smf.tracks.iter().map(|t| t.iter().peekable()).collect();
    let mut track_next_tick: Vec<u32> = vec![0; tracks.len()];
    let mut global_ticks = 0;

    loop {
        let mut next_event_tick = u32::MAX;
        let mut next_track_idx = None;

        for (i, track) in tracks.iter_mut().enumerate() {
            if let Some(event) = track.peek() {
                let t = track_next_tick[i] + event.delta.as_int();
                if t < next_event_tick {
                    next_event_tick = t;
                    next_track_idx = Some(i);
                }
            }
        }

        let idx = match next_track_idx {
            Some(i) => i,
            None => break,
        };

        let event = tracks[idx].next().unwrap();
        track_next_tick[idx] = next_event_tick;

        let delta_ticks = next_event_tick - global_ticks;
        global_ticks = next_event_tick;

        if delta_ticks > 0 {
            let micros_per_tick = micros_per_quarter / tpqn;
            duration += (delta_ticks as f64 * micros_per_tick) / 1_000_000.0;
        }

        if let TrackEventKind::Meta(MetaMessage::Tempo(micros)) = event.kind {
            micros_per_quarter = micros.as_int() as f64;
        }
    }
    duration
}

/// Spawns a new thread to play a MIDI file.
pub fn play_midi_file(
    path: PathBuf,
    tui_tx: Sender<TuiMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    
    // Create a channel for seek commands
    let (seek_tx, seek_rx) = mpsc::channel::<i32>();
    
    // Send the seek channel back to the main thread so the GUI can use it
    let _ = tui_tx.send(TuiMessage::MidiSeekChannel(seek_tx));

    let handle = thread::spawn(move || {
        // Load and parse the MIDI file
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) => { let _ = tui_tx.send(TuiMessage::Error(format!("Read fail: {}", e))); return; }
        };
        let smf = match Smf::parse(&data) {
            Ok(s) => s,
            Err(e) => { let _ = tui_tx.send(TuiMessage::Error(format!("Parse fail: {}", e))); return; }
        };

        let tpqn = match smf.header.timing {
            midly::Timing::Metrical(t) => t.as_int() as f64,
            _ => 480.0,
        };

        // Calculate Total Duration
        let _ = tui_tx.send(TuiMessage::MidiLog("Calculating duration...".into()));
        let total_seconds = get_midi_duration_seconds(&smf);
        let _ = tui_tx.send(TuiMessage::MidiLog(format!("Duration: {:.0}s", total_seconds)));

        // For restarting playback (rewind/seek)
        let mut start_at_seconds = 0.0;

        loop {
            // Yield
            thread::yield_now();

            // Check stop before starting a loop
            if stop_signal.load(Ordering::Relaxed) { break; }

            let mut micros_per_quarter = 500_000.0;
            let mut tracks: Vec<_> = smf.tracks.iter().map(|t| t.iter().peekable()).collect();
            let mut track_next_tick: Vec<u32> = vec![0; tracks.len()];
            let mut global_ticks = 0;
            let mut current_time_seconds = 0.0;
            
            // Should we play audio?
            // If fast-forwarding to a seek point, we mute NoteOns.
            let mut is_fast_forwarding = start_at_seconds > 0.0;
            
            // Delay start only if we are at the very beginning
            if start_at_seconds == 0.0 {
                let _ = tui_tx.send(TuiMessage::MidiProgress(0.0, 0, total_seconds as u32));
                thread::sleep(Duration::from_millis(500));
            } else {
                let _ = tui_tx.send(TuiMessage::MidiLog(format!("Seeking to {:.0}s...", start_at_seconds)));
            }
            
            let mut last_progress_update = Instant::now();

            // Event processing loop
            loop {
                // Yield
                thread::yield_now();

                // Check stop signal
                if stop_signal.load(Ordering::Relaxed) { return; }

                // Check Seek Command
                match seek_rx.try_recv() {
                    Ok(skip_sec) => {
                        let new_time = (current_time_seconds + skip_sec as f64).max(0.0);
                        
                        // Prevent endless restart loop if rewinding past 0 while already at 0
                        if new_time == 0.0 && current_time_seconds < 0.5 {
                            // Do nothing, just continue playing
                        } else {
                            start_at_seconds = new_time;
                            // Send "All Notes Off" to clear hanging notes
                            let _ = tui_tx.send(TuiMessage::TuiAllNotesOff); 
                            // 0xB0 123 is All Notes Off standard CC, do for channel 0 (visual aid mostly)
                            let _ = tui_tx.send(TuiMessage::MidiChannelNotesOff(0)); 
                            
                            break; // BREAK inner loop -> Restarts Outer Loop with new `start_at_seconds`
                        }
                    },
                    Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => {}
                }

                // Find next event
                let mut next_event_tick = u32::MAX;
                let mut next_track_idx = None;

                for (i, track) in tracks.iter_mut().enumerate() {
                    if let Some(event) = track.peek() {
                        let t = track_next_tick[i] + event.delta.as_int();
                        if t < next_event_tick {
                            next_event_tick = t;
                            next_track_idx = Some(i);
                        }
                    }
                }

                // Get the index of the track with the next event
                let idx = match next_track_idx {
                    Some(i) => i,
                    None => {
                        // End of song
                        let _ = tui_tx.send(TuiMessage::MidiLog("Playback finished.".into()));
                        let _ = tui_tx.send(TuiMessage::MidiPlaybackFinished);
                        return; // Exit thread
                    },
                };

                let event = tracks[idx].next().unwrap();
                track_next_tick[idx] = next_event_tick;
                
                let ticks_to_wait = next_event_tick - global_ticks;
                global_ticks = next_event_tick;

                // Time math
                let micros_per_tick = micros_per_quarter / tpqn;
                let delta_seconds = (ticks_to_wait as f64 * micros_per_tick) / 1_000_000.0;
                
                // If we were fast forwarding, check if we reached the target
                if is_fast_forwarding && (current_time_seconds + delta_seconds) >= start_at_seconds {
                    is_fast_forwarding = false;
                    // Adjust current time exactly
                    current_time_seconds = start_at_seconds;
                } else {
                    current_time_seconds += delta_seconds;
                }

                // Sleep logic
                if !is_fast_forwarding && ticks_to_wait > 0 {
                    let wait_micros = (ticks_to_wait as f64 * micros_per_tick) as u64;
                    thread::sleep(Duration::from_micros(wait_micros));
                }

                // Check stop signal again after sleep
                if stop_signal.load(Ordering::Relaxed) { return; }

                // Send progress (throttled)
                if last_progress_update.elapsed().as_millis() > 250 {
                     let progress = if total_seconds > 0.0 { current_time_seconds / total_seconds } else { 0.0 };
                     let _ = tui_tx.send(TuiMessage::MidiProgress(
                         progress as f32, 
                         current_time_seconds as u32, 
                         total_seconds as u32
                    ));
                    last_progress_update = Instant::now();
                }

                // Process the MIDI event
                match event.kind {
                    TrackEventKind::Midi { channel, message } => {
                        // If fast-forwarding, we SKIP NoteOn messages to avoid noise bursts,
                        // but we process other events (controllers) if needed.
                        if !is_fast_forwarding {
                             let channel_num = channel.as_int();
                             match message {
                                MidlyMidiMessage::NoteOn { key, vel } => {
                                     let key = key.as_int();
                                     let vel = vel.as_int();
                                     if vel > 0 {
                                         let _ = tui_tx.send(TuiMessage::MidiNoteOn(key, vel, channel_num));
                                         let _ = tui_tx.send(TuiMessage::TuiNoteOn(key, channel_num, Instant::now()));
                                     } else {
                                         let _ = tui_tx.send(TuiMessage::MidiNoteOff(key, channel_num));
                                         let _ = tui_tx.send(TuiMessage::TuiNoteOff(key, channel_num, Instant::now()));
                                     }
                                },
                                MidlyMidiMessage::NoteOff { key, .. } => {
                                    let key = key.as_int();
                                    let _ = tui_tx.send(TuiMessage::MidiNoteOff(key, channel_num));
                                    let _ = tui_tx.send(TuiMessage::TuiNoteOff(key, channel_num, Instant::now()));
                                },
                                MidlyMidiMessage::Controller { controller, .. } => {
                                    // CC #123 is "All Notes Off"
                                    if controller.as_int() == 123 {
                                        let _ = tui_tx.send(TuiMessage::MidiChannelNotesOff(channel_num));
                                        let _ = tui_tx.send(TuiMessage::TuiAllNotesOff);
                                    }
                                    // TODO: Handle Sustain command (CC #64)
                                },
                                _ => {} // Ignore other MIDI messages
                             }
                        }
                    },
                    TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                        micros_per_quarter = micros.as_int() as f64;
                    },
                    _ => {} // Ignore Sysex or other meta events
                }
            } // End Inner Loop
        } // End Outer Loop
    });
    Ok(handle)
}