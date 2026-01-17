use anyhow::Result;
use midir::{MidiInput, MidiInputPort};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::audio::{
    get_audio_device_names, get_default_audio_device_name, get_supported_sample_rates,
};
use crate::input::KeyboardLayout;

/// Represents a specific MIDI trigger (Note or SysEx)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MidiEventSpec {
    Note {
        channel: u8, // 0-15
        note: u8,
        is_note_off: bool,
    },
    SysEx(Vec<u8>),
}

impl fmt::Display for MidiEventSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiEventSpec::Note { channel, note, is_note_off } => {
                let status = if *is_note_off { "Off" } else { "On" };
                write!(f, "Ch{} Note {} ({})", channel + 1, note, status)
            }
            MidiEventSpec::SysEx(bytes) => {
                let hex: Vec<String> = bytes.iter().map(|b| format!("{:02X}", b)).collect();
                // Truncate if too long for display
                if hex.len() > 8 {
                    write!(f, "SysEx: {}...", hex[0..8].join(" "))
                } else {
                    write!(f, "SysEx: {}", hex.join(" "))
                }
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum MidiMappingMode {
    /// All input channels from this device map to a single target channel
    Simple,
    /// Each input channel (0-15) is individually mapped to a target channel
    Complex,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MidiDeviceConfig {
    pub name: String,
    pub enabled: bool,
    pub mapping_mode: MidiMappingMode,
    /// Used if mode is Simple: All inputs go to this channel (0-15)
    pub simple_target_channel: u8,
    /// Used if mode is Complex: Index = Input Channel, Value = Target Channel
    pub complex_mapping: [u8; 16],
}

impl Default for MidiDeviceConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: false,
            mapping_mode: MidiMappingMode::Simple,
            simple_target_channel: 0,
            // Default 1:1 mapping (0->0, 1->1, etc.)
            complex_mapping: std::array::from_fn(|i| i as u8),
        }
    }
}

/// Settings that are saved to the configuration file.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppSettings {
    pub organ_file: Option<PathBuf>,
    pub ir_file: Option<PathBuf>,
    pub reverb_mix: f32,
    pub audio_buffer_frames: usize,
    pub max_ram_gb: f32,
    pub precache: bool,
    pub convert_to_16bit: bool,
    pub original_tuning: bool,
    pub tui_mode: bool,
    pub gain: f32,
    pub polyphony: usize,
    pub audio_device_name: Option<String>,
    pub sample_rate: u32,
    pub keyboard_layout: KeyboardLayout,
    #[serde(default)]
    pub midi_devices: Vec<MidiDeviceConfig>,
}

/// Default settings for a new installation.
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            organ_file: None,
            ir_file: None,
            reverb_mix: 0.5,
            audio_buffer_frames: 256,
            max_ram_gb: 8.0,
            precache: false,
            convert_to_16bit: false,
            original_tuning: false,
            tui_mode: false, // Default to GUI
            gain: 0.4,       // Conservative default gain
            polyphony: 128,
            audio_device_name: None,
            sample_rate: 48000,
            keyboard_layout: KeyboardLayout::Qwerty,
            midi_devices: Vec::new(),
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
    pub max_ram_gb: f32,
    pub precache: bool,
    pub convert_to_16bit: bool,
    pub original_tuning: bool,
    pub gain: f32,
    pub polyphony: usize,

    // --- Runtime-Only Settings ---
    pub midi_file: Option<PathBuf>,
    pub audio_device_name: Option<String>,
    pub sample_rate: u32,
    pub active_midi_devices: Vec<(MidiInputPort, MidiDeviceConfig)>,
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

/// Helper to get the Reverb directory, creating it if it doesn't exist.
pub fn get_reverb_directory() -> Result<PathBuf> {
    let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;
    let parent = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("No config parent dir"))?;
    let reverb_dir = parent.join("reverb");

    if !reverb_dir.exists() {
        fs::create_dir_all(&reverb_dir)?;
    }
    Ok(reverb_dir)
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct OrganProfile {
    pub name: String,
    pub path: PathBuf,
    /// MIDI SysEx command bytes that trigger loading this organ.
    /// We use a hex string for easier JSON editing/viewing, but store as Vec codes in runtime if needed.
    #[serde(default)]
    pub activation_trigger: Option<MidiEventSpec>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct OrganLibrary {
    pub organs: Vec<OrganProfile>,
}

pub fn load_organ_library() -> Result<OrganLibrary> {
    let lib: OrganLibrary = confy::load("rusty-pipes", "organ_library")?;
    Ok(lib)
}

pub fn save_organ_library(lib: &OrganLibrary) -> Result<()> {
    confy::store("rusty-pipes", "organ_library", lib)?;
    Ok(())
}

/// Scans the reverb directory for supported audio files.
/// Returns a vector of (Display Name, PathBuf).
pub fn get_available_ir_files() -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();

    // Add "None" option logic implicitly, but here we just list files.

    if let Ok(dir) = get_reverb_directory() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        match ext.to_lowercase().as_str() {
                            "wav" | "flac" | "mp3" => {
                                let name = path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("Unknown")
                                    .to_string();
                                files.push((name, path));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Sort alphabetically by name
    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    files
}

/// Helper struct to pass around in the config UIs
pub struct ConfigState {
    pub settings: AppSettings,
    pub midi_file: Option<PathBuf>,

    // List of currently connected system ports: (Port, Name)
    pub system_midi_ports: Vec<(MidiInputPort, String)>,

    pub error_msg: Option<String>,

    // Audio-related fields
    pub available_audio_devices: Vec<String>,
    pub selected_audio_device_name: Option<String>,
    pub available_sample_rates: Vec<u32>,
    pub available_ir_files: Vec<(String, PathBuf)>,
}

impl ConfigState {
    pub fn new(
        mut settings: AppSettings,
        midi_input_arc: &Arc<Mutex<Option<MidiInput>>>,
    ) -> Result<Self> {
        let mut error_msg = None;

        let mut system_midi_ports = Vec::new();

        if let Some(midi_in) = midi_input_arc.lock().unwrap().as_ref() {
            for port in midi_in.ports() {
                if let Ok(name) = midi_in.port_name(&port) {
                    system_midi_ports.push((port, name.clone()));

                    // Sync settings with detected ports.
                    // If a detected port is not in settings, add it (disabled by default).
                    if !settings.midi_devices.iter().any(|d| d.name == name) {
                        settings.midi_devices.push(MidiDeviceConfig {
                            name,
                            enabled: false,
                            ..Default::default()
                        });
                    }
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
        let available_sample_rates = get_supported_sample_rates(selected_audio_device_name.clone())
            .unwrap_or_else(|_| vec![44100, 48000]);

        // If the current setting (e.g. default 48000) is not supported by the device,
        // auto-select the best available one.
        if !available_sample_rates.contains(&settings.sample_rate) {
            if available_sample_rates.contains(&48000) {
                settings.sample_rate = 48000;
            } else if available_sample_rates.contains(&44100) {
                settings.sample_rate = 44100;
            } else if let Some(&first) = available_sample_rates.first() {
                settings.sample_rate = first;
            }
        }

        let available_ir_files = get_available_ir_files();

        Ok(Self {
            settings,
            midi_file: None,
            system_midi_ports,
            error_msg,
            available_audio_devices,
            selected_audio_device_name,
            available_sample_rates,
            available_ir_files,
        })
    }
}
