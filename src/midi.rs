use anyhow::Result;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use std::fs;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use midly::{
    Format, Header, Smf, TrackEvent, TrackEventKind, 
    MetaMessage, MidiMessage as MidlyMidiMessage, Timing, num::*
};
use midir::{MidiInput, MidiInputPort};
use chrono::Local;

use crate::app::TuiMessage;
use crate::config::{MidiDeviceConfig, MidiMappingMode};

pub struct MidiRecorder {
    track: Vec<TrackEvent<'static>>, 
    last_event_time: Instant,
    organ_name: String,
}

impl MidiRecorder {
    pub fn new(organ_name: String) -> Self {
        Self {
            track: Vec::new(),
            last_event_time: Instant::now(),
            organ_name,
        }
    }

    pub fn record(&mut self, channel: u8, status_byte: u8, param1: u8, param2: u8) {
        let now = Instant::now();
        let delta_micros = now.duration_since(self.last_event_time).as_micros() as u32;
        self.last_event_time = now;

        // Convert micros to MIDI ticks (approximate).
        // 120 BPM = 500,000 micros/beat. 480 ticks/beat.
        // Factor = 480 / 500,000 = 0.00096
        let delta_ticks = (delta_micros as f32 * 0.00096) as u32;

        // midly types require specific wrappers
        let u4_channel = u4::from(channel & 0x0F);
        let u7_p1 = u7::from(param1 & 0x7F);
        let u7_p2 = u7::from(param2 & 0x7F);

        let kind = match status_byte & 0xF0 {
            0x90 => Some(TrackEventKind::Midi { 
                channel: u4_channel, 
                message: MidlyMidiMessage::NoteOn { key: u7_p1, vel: u7_p2 } 
            }),
            0x80 => Some(TrackEventKind::Midi { 
                channel: u4_channel, 
                message: MidlyMidiMessage::NoteOff { key: u7_p1, vel: u7_p2 } 
            }),
            0xB0 => Some(TrackEventKind::Midi { 
                channel: u4_channel, 
                message: MidlyMidiMessage::Controller { controller: u7_p1, value: u7_p2 } 
            }),
            _ => None,
        };

        if let Some(kind) = kind {
            self.track.push(TrackEvent {
                delta: u28::from(delta_ticks), // Wrap delta time in u28
                kind,
            });
        }
    }

    pub fn save(&self) -> Result<String> {
        let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;
        let parent = config_path.parent().ok_or_else(|| anyhow::anyhow!("No config parent dir"))?;
        let recording_dir = parent.join("recordings");
        if !recording_dir.exists() {
            fs::create_dir_all(&recording_dir)?;
        }

        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename = format!("{}_{}_virtual.mid", self.organ_name, timestamp);
        let path = recording_dir.join(&filename);

        // Use Format::SingleTrack (Type 0 MIDI file)
        // Wrap timing (480) in u15
        let header = Header::new(
            Format::SingleTrack, 
            Timing::Metrical(u15::from(480))
        );
        
        let mut smf = Smf::new(header);
        
        // Smf expects a Vec of tracks. Since Format is SingleTrack, we push one track.
        smf.tracks.push(self.track.clone());

        smf.save(&path)?;
        
        log::info!("Saved MIDI file to {:?}", path);
        Ok(path.to_string_lossy().to_string())
    }
}

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

    midi_input.connect(
        port, 
        device_name, 
        move |_, message, _| {
            process_live_midi_message(message, &tx_clone, &config, &name_clone, &shared_recorder);
        }, 
        ()
    ).map_err(|e| anyhow::anyhow!("Failed to connect to MIDI device {}: {}", device_name, e))
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
    // Ignore system real-time messages (0xF8-0xFF) and Sysex for now
    if status >= 0xF0 {
        return;
    }

    let raw_channel = status & 0x0F;
    let msg_type = status & 0xF0;

    // --- APPLY MAPPING ---
    let target_channel = match config.mapping_mode {
        MidiMappingMode::Simple => config.simple_target_channel,
        MidiMappingMode::Complex => {
            // Safety check for array bounds (0-15)
            config.complex_mapping.get(raw_channel as usize).cloned().unwrap_or(raw_channel)
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
        0x90 => { // Note On
            let note = message[1];
            let velocity = message[2];
            if velocity > 0 {
                let note_name = midi_note_to_name(note);
                let log_msg = format!("Note On: {} (Ch {}, Vel {})", note_name, channel + 1, velocity);
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
        },
        0x80 => { // Note Off
            let note = message[1];
            let note_name = midi_note_to_name(note);
            let log_msg = format!("Note Off: {} (Ch {})", note_name, channel + 1);
            let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
            let _ = tui_tx.send(TuiMessage::MidiNoteOff(note, channel));
            let _ = tui_tx.send(TuiMessage::TuiNoteOff(note, channel, now));
        },
        0xB0 => { // Control Change
            let controller = message[1];
            if controller == 123 { // All Notes Off
                let log_msg = format!("All Off (Ch {})", channel + 1);
                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                let _ = tui_tx.send(TuiMessage::MidiChannelNotesOff(channel));
                let _ = tui_tx.send(TuiMessage::TuiAllNotesOff);
            }
        },
        _ => {}
    }
}

/// Spawns a new thread to play a MIDI file.
pub fn play_midi_file(
    path: PathBuf,
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

        // Delay playback for 3 seconds to allow user to prepare
        let _ = tui_tx.send(TuiMessage::MidiLog("Playback will start in 3 seconds...".into()));
        thread::sleep(Duration::from_secs(3));

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
            let now = Instant::now();

            // Process the MIDI event
            match event.kind {
                TrackEventKind::Midi { channel, message } => {
                    let channel_num = channel.as_int(); // This is 0-15
                    match message {
                        MidlyMidiMessage::NoteOn { key, vel } => {
                            let key = key.as_int();
                            let vel = vel.as_int();
                            let note_name = midi_note_to_name(key);
                            if vel > 0 {
                                let log_msg = format!("Note On: {} (Ch {}, Vel {})", note_name, channel_num + 1, vel);
                                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                                let _ = tui_tx.send(TuiMessage::TuiNoteOn(key, channel_num, now));
                                let _ = tui_tx.send(TuiMessage::MidiNoteOn(key, vel, channel_num));
                            } else {
                                // Velocity 0 is a Note Off
                                let log_msg = format!("Note Off: {} (Ch {})", note_name, channel_num + 1);
                                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                                let _ = tui_tx.send(TuiMessage::TuiNoteOff(key, channel_num,now));
                                let _ = tui_tx.send(TuiMessage::MidiNoteOff(key, channel_num));
                            }
                        },
                        MidlyMidiMessage::NoteOff { key, vel: _ } => {
                            let key = key.as_int();
                            let note_name = midi_note_to_name(key);
                            let log_msg = format!("Note Off: {} (Ch {})", note_name, channel_num + 1);
                            let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                            let _ = tui_tx.send(TuiMessage::TuiNoteOff(key, channel_num, now));
                            let _ = tui_tx.send(TuiMessage::MidiNoteOff(key, channel_num));
                        },
                        MidlyMidiMessage::Controller { controller, value: _ } => {
                            // CC #123 is "All Notes Off"
                            if controller.as_int() == 123 {
                                let log_msg = format!("All Off (Ch {})", channel_num + 1);
                                let _ = tui_tx.send(TuiMessage::MidiLog(log_msg));
                                let _ = tui_tx.send(TuiMessage::MidiChannelNotesOff(channel_num));
                                let _ = tui_tx.send(TuiMessage::TuiAllNotesOff);
                            }
                            // TODO: Handle Sustain command (CC #64)
                        },
                        _ => {} // Ignore other MIDI messages
                    }
                },
                TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                    micros_per_quarter = micros.as_int() as f64;
                    let _ = tui_tx.send(TuiMessage::MidiLog(format!("Tempo {} μs/q", micros.as_int())));
                },
                _ => {} // Ignore Sysex or other meta events
            }
        }
        
        let _ = tui_tx.send(TuiMessage::MidiLog("Playback finished.".into()));
    });

    Ok(handle)
}