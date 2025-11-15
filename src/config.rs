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
}

/// Default settings for a new installation.
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            organ_file: None,
            ir_file: None,
            reverb_mix: 0.5,
            audio_buffer_frames: 512,
            precache: false,
            convert_to_16bit: false,
            original_tuning: false,
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
    pub fn new(settings: AppSettings, _midi_input_arc: &Arc<Mutex<Option<MidiInput>>>) -> Result<Self> {
        let mut _midi_input = None;
        let mut available_ports = Vec::new();
        let mut error_msg = None;

        match MidiInput::new("Rusty Pipes MIDI Input") {
            Ok(midi_in) => {
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
                _midi_input = Some(midi_in);
            }
            Err(e) => {
                error_msg = Some(format!("Failed to initialize MIDI: {}", e));
            }
        }

        Ok(Self {
            settings,
            midi_file: None,
            selected_midi_port: None,
            available_ports,
            error_msg,
        })
    }
}