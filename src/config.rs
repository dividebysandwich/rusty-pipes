use anyhow::Result;
use midir::{MidiInput, MidiInputPort};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::audio::{get_audio_device_names, get_default_audio_device_name, get_supported_sample_rates};

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
    pub polyphony: usize,
    pub audio_device_name: Option<String>,
    pub sample_rate: u32,
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
            polyphony: 128,
            audio_device_name: None,
            sample_rate: 48000,
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
    pub polyphony: usize,

    // --- Runtime-Only Settings ---
    pub midi_file: Option<PathBuf>,
    pub midi_port: Option<MidiInputPort>,
    pub midi_port_name: Option<String>,
    pub audio_device_name: Option<String>,
    pub sample_rate: u32,
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

    // Audio-related fields
    pub available_audio_devices: Vec<String>,
    pub selected_audio_device_name: Option<String>,
    pub available_sample_rates: Vec<u32>,
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

        let mut available_audio_devices = Vec::new();
        let mut selected_audio_device_name = None;

        match get_audio_device_names() {
            Ok(names) => {
                if names.is_empty() {
                    let msg = "No audio output devices found.".to_string();
                    error_msg = Some(error_msg.map_or(msg.clone(), |e| format!("{} {}", e, msg)));
                }
                available_audio_devices = names;
            }
            Err(e) => {
                let msg = format!("Error finding audio devices: {}", e);
                error_msg = Some(error_msg.map_or(msg.clone(), |err| format!("{} {}", err, msg)));
            }
        }

        // Try to select the saved audio device
        if let Some(saved_name) = &settings.audio_device_name {
            if available_audio_devices.contains(saved_name) {
                selected_audio_device_name = Some(saved_name.clone());
            }
        }

        // If no saved device, try to select the system default
        if selected_audio_device_name.is_none() {
            if let Ok(Some(default_name)) = get_default_audio_device_name() {
                if available_audio_devices.contains(&default_name) {
                    selected_audio_device_name = Some(default_name);
                }
            }
        }
        // If still None, it will just be "Default" in the UI (which is None)

        // Get available sample rates for the selected audio device
        let available_sample_rates = get_supported_sample_rates(selected_audio_device_name.clone()).unwrap_or_else(|_| vec![44100, 48000]);

        Ok(Self {
            settings,
            midi_file: None,
            selected_midi_port, // <-- Set here
            available_ports,
            error_msg,
            available_audio_devices,
            selected_audio_device_name,
            available_sample_rates,
        })
    }
}