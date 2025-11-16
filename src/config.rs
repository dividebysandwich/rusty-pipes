use anyhow::Result;
use midir::{MidiInput, MidiInputPort};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Settings that are saved to the configuration file.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppSettings {
    pub organ_file: Option<PathBuf>,
    pub ir_file: Option<PathBuf>,
    pub reverb_mix: f32,
    pub audio_buffer_frames: usize,
    pub precache: bool,
    pub convert_to_16bit: bool,
    pub original_tuning: bool,
    pub tui_mode: bool,
    pub midi_device_name: Option<String>,
    pub gain: f32,
}

/// Default settings for a new installation.
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            organ_file: None,
            ir_file: None,
            reverb_mix: 0.5,
            audio_buffer_frames: 256,
            precache: false,
            convert_to_16bit: false,
            original_tuning: false,
            tui_mode: false, // Default to GUI
            midi_device_name: None,
            gain: 0.4, // Conservative default gain
        }
    }
}

/// A complete configuration passed from the config UI to the main app.
/// This is *not* saved to disk.
#[derive(Clone)]
pub struct RuntimeConfig {
    // --- Saved Settings ---
    pub organ_file: PathBuf,
    pub ir_file: Option<PathBuf>,
    pub reverb_mix: f32,
    pub audio_buffer_frames: usize,
    pub precache: bool,
    pub convert_to_16bit: bool,
    pub original_tuning: bool,
    pub gain: f32,

    // --- Runtime-Only Settings ---
    pub midi_file: Option<PathBuf>,
    pub midi_port: Option<MidiInputPort>, // The port to connect to
    pub midi_port_name: Option<String>,
}

/// Loads settings from disk.
pub fn load_settings() -> Result<AppSettings> {
    let settings: AppSettings = confy::load("rusty-pipes", "settings")?;
    Ok(settings)
}

/// Saves settings to disk.
pub fn save_settings(settings: &AppSettings) -> Result<()> {
    confy::store("rusty-pipes", "settings", settings)?;
    Ok(())
}

/// Helper struct to pass around in the config UIs
pub struct ConfigState {
    pub settings: AppSettings,
    pub midi_file: Option<PathBuf>,
    pub selected_midi_port: Option<(MidiInputPort, String)>,
    
    // MIDI-related fields
    pub available_ports: Vec<(MidiInputPort, String)>,
    pub error_msg: Option<String>,
}

impl ConfigState {
    pub fn new(settings: AppSettings, midi_input_arc: &Arc<Mutex<Option<MidiInput>>>) -> Result<Self> {
        let mut available_ports = Vec::new();
        let mut error_msg = None;
        let mut selected_midi_port = None;

        if let Some(midi_in) = midi_input_arc.lock().unwrap().as_ref() {
            let ports = midi_in.ports();
            if ports.is_empty() {
                error_msg = Some("No MIDI devices found.".to_string());
            } else {
                for port in ports.iter() {
                    if let Ok(name) = midi_in.port_name(port) {
                        available_ports.push((port.clone(), name));
                    }
                }
            }

            // After populating available_ports, check against saved settings
            if let Some(saved_name) = &settings.midi_device_name {
                if let Some(found_port) = available_ports.iter().find(|(_, name)| name == saved_name) {
                    selected_midi_port = Some(found_port.clone());
                }
            }

        } else {
            error_msg = Some("Failed to initialize MIDI.".to_string());
        }

        Ok(Self {
            settings,
            midi_file: None,
            selected_midi_port, // <-- Set here
            available_ports,
            error_msg,
        })
    }
}