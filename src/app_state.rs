use anyhow::Result;
use midir::{MidiInput, MidiInputPort, MidiInputConnection};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs::File,
    io::{BufReader, BufWriter},
    path::PathBuf,
    sync::{mpsc::Sender, Arc, Mutex},
    time::{Duration, Instant},
};
use crate::{
    app::{AppMessage, TuiMessage},
    midi,
    organ::Organ,
    config::{load_settings, save_settings, MidiDeviceConfig},
    input::KeyboardLayout,
    midi_recorder::MidiRecorder,
    midi_control::{MidiControlMap, MidiEventSpec},
};

// --- Shared Constants & Types ---

pub const PRESET_FILE_NAME: &str = "rusty-pipes.presets.json";
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
    config: MidiDeviceConfig, // New Argument
    shared_midi_recorder: Arc<Mutex<Option<MidiRecorder>>>,
) -> Result<MidiInputConnection<()>> {
    // We delegate to the logic in midi.rs, which sets up the callback 
    // with the specific channel mapping rules found in `config`.
    midi::connect_to_midi(midi_input, port, port_name, tui_tx, config, shared_midi_recorder)
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
    // Stores active notes keyed by (Channel, Note) to support multi-manual play without collision.
    pub active_midi_notes: HashMap<(u8, u8), PlayedNote>,
    // Notes that have finished playing, but are still within the display window
    pub finished_notes_display: VecDeque<PlayedNote>,
    // Time parameters for the scrolling window
    pub piano_roll_display_duration: Duration,
    /// Maps MIDI Channel (0-15) -> Set of active notes (0-127)
    pub channel_active_notes: HashMap<u8, BTreeSet<u8>>,
    /// MIDI channel assignment presets
    pub presets: PresetBank,
    pub gain: f32,
    pub polyphony: usize,
    pub last_underrun: Option<Instant>, // Store when the last buffer underrun occurred
    pub active_voice_count: usize,
    pub cpu_load: f32,
    pub keyboard_layout: KeyboardLayout,
    pub octave_offset: i8, // Octave offset for computer keyboard input
    pub reverb_mix: f32,
    pub selected_reverb_index: Option<usize>,
    /// Set of currently active tremulant IDs
    pub active_tremulants: BTreeSet<String>,
    pub is_recording_midi: bool,
    pub is_recording_audio: bool,
    pub midi_control_map: MidiControlMap,
    // Stores the last raw midi event received and when, used by the Learn UI
    pub last_midi_event_received: Option<(MidiEventSpec, Instant)>,
}

pub fn get_preset_file_path() -> PathBuf {
    let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")
        .expect("Could not get configuration file path");
    let preset_dir = config_path.parent().expect("Could not get preset directory");
    preset_dir.join(PRESET_FILE_NAME)
}

impl AppState {
    pub fn new(
        organ: Arc<Organ>, 
        gain: f32, 
        polyphony: usize,
        keyboard_layout: KeyboardLayout,
    ) -> Result<Self> {

        let presets = Self::load_presets(&organ.name);
        let midi_control_map = MidiControlMap::load(&organ.name);

        Ok(Self {
            organ,
            stop_channels: HashMap::new(),
            midi_log: VecDeque::with_capacity(MIDI_LOG_CAPACITY),
            error_msg: None,
            currently_playing_notes: HashMap::new(),
            active_midi_notes: HashMap::new(),
            finished_notes_display: VecDeque::new(),
            piano_roll_display_duration: Duration::from_secs(1), // Show 1 second of history
            channel_active_notes: HashMap::new(),
            presets,
            gain,
            polyphony,
            last_underrun: None,
            active_voice_count: 0,
            cpu_load: 0.0,
            keyboard_layout,
            octave_offset: 0,
            reverb_mix: 0.0,
            selected_reverb_index: None,
            active_tremulants: BTreeSet::new(),
            is_recording_midi: false,
            is_recording_audio: false,
            midi_control_map,
            last_midi_event_received: None,
        })
    }
    
    // Helper to calculate the actual MIDI note
    pub fn get_keyboard_midi_note(&self, semitone: u8) -> u8 {
        // Base C3 = 48
        (48 + (self.octave_offset as i32 * 12)) as u8 + semitone
    }

    pub fn persist_settings(&self) {
        // Load existing settings to preserve other fields (like devices)
        let mut settings = load_settings().unwrap_or_default();
        
        // Update values
        settings.gain = self.gain;
        settings.polyphony = self.polyphony;

        // Save back to disk
        if let Err(e) = save_settings(&settings) {
            log::error!("Failed to persist settings: {}", e);
        }
    }

    pub fn modify_gain(&mut self, delta: f32, audio_tx: &Sender<AppMessage>) {
        self.gain = (self.gain + delta).clamp(0.0, 1.0);
        let _ = audio_tx.send(AppMessage::SetGain(self.gain));
        self.persist_settings();
    }

    pub fn modify_polyphony(&mut self, delta: i32, audio_tx: &Sender<AppMessage>) {
        let new_val = (self.polyphony as i32 + delta).max(1); // Minimum 1 voice
        self.polyphony = new_val as usize;
        let _ = audio_tx.send(AppMessage::SetPolyphony(self.polyphony));
        self.persist_settings();
    }

    pub fn set_tremulant_active(&mut self, trem_id: String, active: bool, audio_tx: &Sender<AppMessage>) {
        if active {
            self.active_tremulants.insert(trem_id.clone());
        } else {
            self.active_tremulants.remove(&trem_id);
        }
        let _ = audio_tx.send(AppMessage::SetTremulantActive(trem_id, active));
    }

    /// Loads the MIDI channel mapping preset bank for the specified organ from the JSON file.
    fn load_presets(organ_name: &str) -> PresetBank {
        let preset_path = get_preset_file_path();
        File::open(preset_path)
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
        let preset_path = get_preset_file_path();
        // Load the entire config file (all organs)
        let mut config: PresetConfig = File::open(preset_path.clone())
            .map_err(anyhow::Error::from)
            .and_then(|file| serde_json::from_reader(BufReader::new(file)).map_err(anyhow::Error::from))
            .unwrap_or_default(); // Create a new map if it doesn't exist

        // Update or insert the preset bank for the current organ
        config.insert(self.organ.name.clone(), self.presets.clone());

        // Write the entire config file back to disk
        let file = File::create(preset_path)?;
        serde_json::to_writer_pretty(BufWriter::new(file), &config)?;

        Ok(())
    }

    /// Processes an incoming TuiMessage, updates state, and sends AppMessages.
    /// This is the core message-handling logic for both UIs.
    pub fn handle_tui_message(&mut self, msg: TuiMessage, audio_tx: &Sender<AppMessage>) -> Result<()> {
         match msg {
            // --- Raw MIDI events ---
            TuiMessage::MidiNoteOn(note, vel, channel) => {
                // MIDI control learning
                self.last_midi_event_received = Some((
                    MidiEventSpec { channel, note, is_note_off: false },
                    Instant::now()
                ));
                
                // Check if this triggers any stop changes
                let actions = self.midi_control_map.check_event(channel, note, false);
                for (stop_idx, internal_ch, set_active) in actions {
                    if set_active {
                        let _ = self.toggle_stop_channel(stop_idx, internal_ch, audio_tx); // Reuse logic, though toggle logic needs care
                        // Actually, toggle_stop_channel toggles. We want explicit Set.
                        self.set_stop_channel_state(stop_idx, internal_ch, true, audio_tx)?;
                    } else {
                        self.set_stop_channel_state(stop_idx, internal_ch, false, audio_tx)?;
                    }
                }

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
                // MIDI control learning
                self.last_midi_event_received = Some((
                    MidiEventSpec { channel, note, is_note_off: true },
                    Instant::now()
                ));
                // Check if this triggers any stop changes
                let actions = self.midi_control_map.check_event(channel, note, true);
                for (stop_idx, internal_ch, set_active) in actions {
                     self.set_stop_channel_state(stop_idx, internal_ch, set_active, audio_tx)?;
                }
                
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
            TuiMessage::CpuLoadUpdate(cpu_load) => self.cpu_load = cpu_load,
            TuiMessage::ActiveVoicesUpdate(count) => self.active_voice_count = count,
            TuiMessage::AudioUnderrun => self.last_underrun = Some(Instant::now()),
            TuiMessage::MidiLog(log) => self.add_midi_log(log),
            TuiMessage::Error(err) => self.error_msg = Some(err),
            TuiMessage::TuiNoteOn(note, channel, start_time) => self.handle_tui_note_on(note, channel, start_time),
            TuiMessage::TuiNoteOff(note, channel, end_time) => self.handle_tui_note_off(note, channel, end_time),
            TuiMessage::TuiAllNotesOff => self.handle_tui_all_notes_off(),
        }
        Ok(())
    }

    // Helper to explicit set (not toggle) channel state
    pub fn set_stop_channel_state(
        &mut self,
        stop_index: usize,
        channel: u8,
        active: bool,
        audio_tx: &Sender<AppMessage>
    ) -> Result<()> {
        let stop_set = self.stop_channels.entry(stop_index).or_default();
        let was_active = stop_set.contains(&channel);

        if active && !was_active {
            stop_set.insert(channel);
        } else if !active && was_active {
            stop_set.remove(&channel);
            // Cut notes if disabling
            if let Some(notes_to_stop) = self.channel_active_notes.get(&channel) {
                if let Some(stop) = self.organ.stops.get(stop_index) {
                    let stop_name = stop.name.clone();
                    for &note in notes_to_stop {
                        audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Simulates a MIDI event from the computer keyboard on Channel 1 (Index 0).
    /// handles audio dispatching and visual state updates.
    pub fn handle_keyboard_note(&mut self, note: u8, velocity: u8, audio_tx: &Sender<AppMessage>) {
        let channel = 0; // Computer keyboard mimics MIDI Channel 1
        let now = Instant::now();
        let note_name = crate::midi::midi_note_to_name(note); // Ensure this helper is public in midi.rs

        if velocity > 0 {
            // --- NOTE ON ---
            
            let played_note = crate::app_state::PlayedNote {
                note,
                channel,
                start_time: now,
                end_time: None,
            };

            self.currently_playing_notes.insert(note, played_note.clone());
            self.active_midi_notes.insert((channel, note), played_note);

            // Update Log
            self.add_midi_log(format!("Key On: {} (Ch 1, Vel {})", note_name, velocity));

            // Dispatch Audio for mapped stops
            // We iterate all loaded stops to see which ones are listening to Channel 0
            for (stop_idx, stop) in self.organ.stops.iter().enumerate() {
                if let Some(channels) = self.stop_channels.get(&stop_idx) {
                    if channels.contains(&channel) {
                        // This stop is mapped to Ch 1, play it!
                        let _ = audio_tx.send(AppMessage::NoteOn(note, velocity, stop.name.clone()));
                    }
                }
            }
            } else {
            // --- NOTE OFF ---
            
            // Update Visuals (Piano Roll)
            if let Some(played_note) = self.currently_playing_notes.remove(&note) {
                 // Move to finished notes for the "trail" effect
                let mut finished = played_note;
                finished.end_time = Some(now);
                self.finished_notes_display.push_back(finished);
            }

            // Update Log
            self.add_midi_log(format!("Key Off: {} (Ch 1)", note_name));

            // Dispatch Audio
            for (stop_idx, stop) in self.organ.stops.iter().enumerate() {
                if let Some(channels) = self.stop_channels.get(&stop_idx) {
                    if channels.contains(&channel) {
                        let _ = audio_tx.send(AppMessage::NoteOff(note, stop.name.clone()));
        }
    }
            }
        }
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
        self.currently_playing_notes.insert(note, played_note.clone());
        self.active_midi_notes.insert((channel, note), played_note);
    }

    pub fn handle_tui_note_off(&mut self, note: u8, channel: u8, end_time: Instant) {
        let mut found = None;
        let mut to_reinsert = Vec::new();

        if let Some(mut played_note) = self.active_midi_notes.remove(&(channel, note)) {
             played_note.end_time = Some(end_time);
        }

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
        self.active_midi_notes.clear();
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
    /// Only releases notes if their controlling MIDI channel is no longer mapped to the stop.
    pub fn recall_preset(&mut self, slot: usize, audio_tx: &Sender<AppMessage>) -> Result<()> {
        if slot >= 12 { return Ok(()); }
        if let Some(preset_data) = &self.presets[slot] {
            let new_preset_map = &preset_data.stop_channels;
            let _preset_name = &preset_data.name;

            let is_valid = new_preset_map.keys().all(|&stop_index| stop_index < self.organ.stops.len());
            
            if is_valid {
                // Snapshot the current configuration before we change it
                let old_map = self.stop_channels.clone();

                // Update the state to the new preset immediately
                // Any new notes played after this line will use the new mapping
                self.stop_channels = new_preset_map.clone();

                // We iterate over the OLD map to find Stop -> Channel mappings that have been removed.
                for (stop_index, old_active_channels) in &old_map {
                    
                    // Get the set of channels enabled for this stop in the new preset
                    let new_active_channels_opt = self.stop_channels.get(stop_index);

                    for &channel in old_active_channels {
                        // Check if this specific channel is still mapped to this stop in the new preset
                        let is_still_mapped = match new_active_channels_opt {
                            Some(new_set) => new_set.contains(&channel),
                            None => false, // The stop was completely disabled in the new preset
                        };

                        // If the channel is no longer mapped to this stop, we must cut the audio
                        // for any notes currently being held on this MIDI channel.
                        if !is_still_mapped {
                            if let Some(active_notes_on_channel) = self.channel_active_notes.get(&channel) {
                                if let Some(stop) = self.organ.stops.get(*stop_index) {
                                    let stop_name = stop.name.clone();
                                    
                                    // Send NoteOff for currently active notes on this specific channel/stop combo
                                    for &note in active_notes_on_channel {
                                        audio_tx.send(AppMessage::NoteOff(note, stop_name.clone()))?;
                                    }
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
                    "Failed to recall preset F{}: stop count mismatch or invalid indices",
                    slot + 1
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
