use anyhow::Result;
use midir::{MidiInput, MidiInputPort, MidiInputConnection};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs::File,
    io::{BufReader, BufWriter},
    sync::mpsc::Sender,
    time::{Duration, Instant},
    sync::Arc,
};
use crate::{
    app::{AppMessage, TuiMessage},
    midi,
    organ::Organ,
};

// --- Shared Constants & Types ---

pub const PRESET_FILE_PATH: &str = "rusty-pipes.presets.json";
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Preset {
    pub name: String,
    pub stop_channels: HashMap<usize, BTreeSet<u8>>,
}
pub type PresetBank = [Option<Preset>; 12];
pub type PresetConfig = HashMap<String, PresetBank>;

pub const MIDI_LOG_CAPACITY: usize = 10; // Max log lines

#[derive(Debug, Clone, PartialEq)]
pub struct PlayedNote {
    pub note: u8,
    pub channel: u8,
    pub start_time: Instant,
    pub end_time: Option<Instant>, // None if still playing
}

// --- Shared MIDI Connection Logic ---

/// Creates a MIDI connection using the specified input and port.
/// This function consumes the `MidiInput` to create the connection.
pub fn connect_to_midi(
    midi_input: MidiInput, // Takes ownership
    port: &MidiInputPort,
    port_name: &str,
    tui_tx: &Sender<TuiMessage>,
) -> Result<MidiInputConnection<()>> {
    let tui_tx_clone = tui_tx.clone();
    let conn = midi_input.connect(
        port,
        port_name,
        move |_timestamp, message, _| {
            midi::midi_callback(message, &tui_tx_clone);
        },
        (),
    ).map_err(|e| anyhow::anyhow!("Failed to connect to MIDI device {}: {}", port_name, e))?;

    Ok(conn)
}

// --- Shared State Struct ---

/// Holds the shared state for both TUI and GUI.
pub struct AppState {
    pub organ: Arc<Organ>,
    /// Maps stop_index -> set of active MIDI channels (0-9)
    pub stop_channels: HashMap<usize, BTreeSet<u8>>,
    pub midi_log: VecDeque<String>,
    pub error_msg: Option<String>,
    // Currently active notes, mapping midi note -> PlayedNote instance
    pub currently_playing_notes: HashMap<u8, PlayedNote>,
    // Notes that have finished playing, but are still within the display window
    pub finished_notes_display: VecDeque<PlayedNote>,
    // Time parameters for the scrolling window
    pub piano_roll_display_duration: Duration,
    /// Maps MIDI Channel (0-15) -> Set of active notes (0-127)
    pub channel_active_notes: HashMap<u8, BTreeSet<u8>>,
    /// MIDI channel assignment presets
    pub presets: PresetBank,
}

impl AppState {
    pub fn new(organ: Arc<Organ>) -> Result<Self> {

        let presets = Self::load_presets(&organ.name);

        Ok(Self {
            organ,
            stop_channels: HashMap::new(),
            midi_log: VecDeque::with_capacity(MIDI_LOG_CAPACITY),
            error_msg: None,
            currently_playing_notes: HashMap::new(),
            finished_notes_display: VecDeque::new(),
            piano_roll_display_duration: Duration::from_secs(1), // Show 1 second of history
            channel_active_notes: HashMap::new(),
            presets,
        })
    }
    
    /// Loads the MIDI channel mapping preset bank for the specified organ from the JSON file.
    fn load_presets(organ_name: &str) -> PresetBank {
        File::open(PRESET_FILE_PATH)
            .map_err(anyhow::Error::from) // Convert std::io::Error
            .and_then(|file| {
                // Read the entire config map
                serde_json::from_reader(BufReader::new(file)).map_err(anyhow::Error::from)
            })
            .ok() // Convert Result to Option
            .and_then(|config: PresetConfig| {
                // Find the presets for this organ
                config.get(organ_name).cloned()
            })
            .unwrap_or_else(Default::default) // Return an empty bank [None; 12] if not found
    }

    /// Saves the entire configuration map back to the JSON file.
    fn save_all_presets_to_file(&self) -> Result<()> {
        // 1. Load the entire config file (all organs)
        let mut config: PresetConfig = File::open(PRESET_FILE_PATH)
            .map_err(anyhow::Error::from)
            .and_then(|file| serde_json::from_reader(BufReader::new(file)).map_err(anyhow::Error::from))
            .unwrap_or_default(); // Create a new map if it doesn't exist

        // 2. Update or insert the preset bank for the current organ
        config.insert(self.organ.name.clone(), self.presets.clone());

        // 3. Write the entire config file back to disk
        let file = File::create(PRESET_FILE_PATH)?;
        serde_json::to_writer_pretty(BufWriter::new(file), &config)?;

        Ok(())
    }

    /// Processes an incoming TuiMessage, updates state, and sends AppMessages.
    /// This is the core message-handling logic for both UIs.
    pub fn handle_tui_message(&mut self, msg: TuiMessage, audio_tx: &Sender<AppMessage>) -> Result<()> {
         match msg {
            // --- Raw MIDI events ---
            TuiMessage::MidiNoteOn(note, vel, channel) => {
                // Track the active note
                self.channel_active_notes.entry(channel).or_default().insert(note);
                // Find all stops mapped to this channel and send AppMessage
                for (stop_index, active_channels) in &self.stop_channels {
                    if active_channels.contains(&channel) {
                        if let Some(stop) = self.organ.stops.get(*stop_index) {
                            let stop_name = stop.name.clone();
                            audio_tx.send(AppMessage::NoteOn(note, vel, stop_name))?;
                        }
                    }
                }
            },
            TuiMessage::MidiNoteOff(note, channel) => {
                // Stop tracking the active note
                if let Some(notes) = self.channel_active_notes.get_mut(&channel) {
                    notes.remove(&note);
                }
                // Find all stops mapped to this channel and send AppMessage
                for (stop_index, active_channels) in &self.stop_channels {
                    if active_channels.contains(&channel) {
                        if let Some(stop) = self.organ.stops.get(*stop_index) {
                            let stop_name = stop.name.clone();
                            audio_tx.send(AppMessage::NoteOff(note, stop_name))?;
                        }
                    }
                }
            },
            TuiMessage::MidiChannelNotesOff(channel) => {
                // Handle channel-specific all notes off
                if let Some(notes_to_stop) = self.channel_active_notes.remove(&channel) {
                    // Find all stops mapped to this channel
                    for (stop_index, active_channels) in &self.stop_channels {
                        if active_channels.contains(&channel) {
                            if let Some(stop) = self.organ.stops.get(*stop_index) {
                                let stop_name = stop.name.clone();
                                // Send NoteOff for each note that was active on this channel
                                for &note in &notes_to_stop {
                                    audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                                }
                            }
                        }
                    }
                }
            },
            
            // --- Other TUI messages ---
            TuiMessage::MidiLog(log) => self.add_midi_log(log),
            TuiMessage::Error(err) => self.error_msg = Some(err),
            TuiMessage::TuiNoteOn(note, channel, start_time) => self.handle_tui_note_on(note, channel, start_time),
            TuiMessage::TuiNoteOff(note, channel, end_time) => self.handle_tui_note_off(note, channel, end_time),
            TuiMessage::TuiAllNotesOff => self.handle_tui_all_notes_off(),
        }
        Ok(())
    }

    /// Toggles a specific channel (0-9) for the specified stop.
    pub fn toggle_stop_channel(
        &mut self,
        stop_index: usize,
        channel: u8,
        audio_tx: &Sender<AppMessage>
    ) -> Result<()> {
        let stop_set = self.stop_channels.entry(stop_index).or_default();
        
        if stop_set.contains(&channel) {
            stop_set.remove(&channel);
            
            // --- Send NoteOff for all active notes on this channel for this stop ---
            if let Some(notes_to_stop) = self.channel_active_notes.get(&channel) {
                if let Some(stop) = self.organ.stops.get(stop_index) {
                    let stop_name = stop.name.clone();
                    for &note in notes_to_stop {
                        audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                    }
                }
            }
        } else {
            stop_set.insert(channel);
        };
        Ok(())
    }

    /// Activates all channels (0-9) for the specified stop.
    pub fn select_all_channels_for_stop(&mut self, stop_index: usize) {
        let stop_set = self.stop_channels.entry(stop_index).or_default();
        for channel in 0..10 { // Channels 0-9
            stop_set.insert(channel);
        }
    }

    /// Deactivates all channels (0-9) for the specified stop.
    pub fn select_none_channels_for_stop(
        &mut self,
        stop_index: usize,
        audio_tx: &Sender<AppMessage>
    ) -> Result<()> {
        if let Some(stop_set) = self.stop_channels.get_mut(&stop_index) {
            // Collect channels to deactivate
            let channels_to_deactivate: Vec<u8> = stop_set.iter().copied()
                .filter(|&c| c < 10)
                .collect();

            if !channels_to_deactivate.is_empty() {
                if let Some(stop) = self.organ.stops.get(stop_index) {
                    let stop_name = stop.name.clone();
                    for channel in channels_to_deactivate {
                        // --- Send NoteOff for all active notes on this channel for this stop ---
                        if let Some(notes_to_stop) = self.channel_active_notes.get(&channel) {
                            for &note in notes_to_stop {
                                audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                            }
                        }
                        // Now remove it from the state
                        stop_set.remove(&channel);
                    }
                } else {
                    // Fallback (shouldn't happen)
                    for channel in channels_to_deactivate {
                        stop_set.remove(&channel);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn add_midi_log(&mut self, msg: String) {
        if self.midi_log.len() == MIDI_LOG_CAPACITY {
            self.midi_log.pop_front();
        }
        self.midi_log.push_back(msg);
    }

    pub fn handle_tui_note_on(&mut self, note: u8, channel: u8, start_time: Instant) {
        let played_note = PlayedNote {
            note,
            channel,
            start_time,
            end_time: None,
        };
        self.currently_playing_notes.insert(note, played_note);
    }

    pub fn handle_tui_note_off(&mut self, note: u8, channel: u8, end_time: Instant) {
        let mut found = None;
        let mut to_reinsert = Vec::new();

        for (n, mut played_note) in self.currently_playing_notes.drain() {
            if n == note && played_note.channel == channel && found.is_none() {
                played_note.end_time = Some(end_time);
                self.finished_notes_display.push_back(played_note);
                found = Some(n);
            } else {
                to_reinsert.push((n, played_note));
            }
        }

        for (n, played_note) in to_reinsert {
            self.currently_playing_notes.insert(n, played_note);
        }
    }

    pub fn handle_tui_all_notes_off(&mut self) {
        let now = Instant::now();
        for (_, mut played_note) in self.currently_playing_notes.drain() {
            played_note.end_time = Some(now);
            self.finished_notes_display.push_back(played_note);
        }
    }

    /// Saves the current `stop_channels` to a preset slot with a given name.
    pub fn save_preset(&mut self, slot: usize, name: String) {
        if slot >= 12 { return; }
        
        // Create the new Preset struct
        let new_preset = Preset {
            name: name.clone(),
            stop_channels: self.stop_channels.clone(),
        };
        self.presets[slot] = Some(new_preset);

        self.add_midi_log(format!("Preset slot F{} saved as '{}'", slot + 1, name));
        
        // After saving in memory, write the change to disk.
        if let Err(e) = self.save_all_presets_to_file() {
            self.add_midi_log(format!("ERROR saving presets: {}", e));
        }
    }

    /// Recalls a preset from a slot into `stop_channels`.
    pub fn recall_preset(&mut self, slot: usize, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if slot >= 12 { return Ok(()); }
        if let Some(preset_data) = &self.presets[slot] {
            // Get the data from inside the struct
            let stop_channels = &preset_data.stop_channels;
            let _preset_name = &preset_data.name;

            let is_valid = stop_channels.keys().all(|&stop_index| stop_index < self.organ.stops.len());
            if is_valid {
                // First, update all stops to the preset
                self.stop_channels = stop_channels.clone();

                // Iterate through all stops
                for stop in self.organ.stops.iter() {
                    let stop_name = stop.name.clone();
                    // Send NoteOff for all active notes on this stop
                    for notes in self.channel_active_notes.values() {
                        for &note in notes {
                            audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                        }
                    }
                }

                // Then, for each stop, send NoteOff for channels that are being deactivated
                for stop in self.organ.stops.iter() {
                    for channel in 0..10 {
                        let active_notes_on_channel = self.channel_active_notes.get(&channel);
                        // Get active channels for this stop in the recalled preset
                        // We must use `stop_channels` (the map) not `preset_data` (the struct)
                        let active_channels = stop_channels.get(&stop.id_str.parse::<usize>()?).cloned().unwrap_or_default();
                        if !active_channels.contains(&channel) {
                            // Send NoteOff for all active notes on this channel for this stop
                            if let Some(notes_to_stop) = active_notes_on_channel {
                                let stop_name = stop.name.clone();
                                for &note in notes_to_stop {
                                    audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                                }
                            }
                        }
                    }
                }
                log::info!("Recalled preset from slot F{}", slot + 1);
                self.add_midi_log(format!("Recalled preset F{}", slot + 1));
            } else {
                // This can happen if the organ definition file changed
                let err_msg = format!(
                    "Failed to recall preset F{}: stop count mismatch (preset has {}, organ has {})",
                    slot + 1, stop_channels.len(), self.stop_channels.len()
                );
                log::warn!("{}", err_msg);
                self.add_midi_log(err_msg);
            }
        } else {
            let err_msg = format!("No preset found in slot F{}", slot + 1);
            log::warn!("{}", err_msg);
            self.add_midi_log(err_msg);
        }
        Ok(())
    }

    pub fn update_piano_roll_state(&mut self) {
        let now = Instant::now();

        // Remove notes that are entirely off-screen
        let oldest_time_to_display = now.checked_sub(self.piano_roll_display_duration)
            .unwrap_or(Instant::now()); // Safely get the boundary

        while let Some(front_note) = self.finished_notes_display.front() {
            // A note is off-screen if its end_time is older than the oldest_time_to_display
            let is_off_screen = front_note.end_time.map_or(
                false, // Still playing (shouldn't be in this queue, but handle defensively)
                |et| et < oldest_time_to_display, // Finished, and ended too long ago
            );

            if is_off_screen {
                self.finished_notes_display.pop_front();
            } else {
                break; // Stop when we find a note that's still on screen
            }
        }
    }
}
