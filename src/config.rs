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
use crate::voice::MAX_NEW_VOICES_PER_BLOCK;

fn default_max_new_voices_per_block() -> usize {
    MAX_NEW_VOICES_PER_BLOCK
}

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
            MidiEventSpec::Note {
                channel,
                note,
                is_note_off,
            } => {
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
#[derive(Debug, Serialize, Deserialize, Clone)]
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
    /// Maximum number of new voices spawned per audio block. Smooths the CPU
    /// load from large simultaneous note-on bursts (e.g. dense MIDI playback).
    #[serde(default = "default_max_new_voices_per_block")]
    pub max_new_voices_per_block: usize,
    pub audio_device_name: Option<String>,
    pub sample_rate: u32,
    pub keyboard_layout: KeyboardLayout,
    #[serde(default)]
    pub midi_devices: Vec<MidiDeviceConfig>,
    #[serde(default)]
    pub lcd_displays: Vec<LcdDisplayConfig>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum LcdColor {
    Off,
    White,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl Default for LcdColor {
    fn default() -> Self {
        Self::White
    }
}

impl LcdColor {
    pub fn to_byte(&self) -> u8 {
        match self {
            LcdColor::Off => 0,
            LcdColor::White => 1,
            LcdColor::Red => 2,
            LcdColor::Green => 3,
            LcdColor::Yellow => 4,
            LcdColor::Blue => 5,
            LcdColor::Magenta => 6,
            LcdColor::Cyan => 7,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum LcdLineType {
    Empty,
    OrganName,
    SystemStatus, // "Loading... / Ready / CPU: x%"
    LastPreset,
    LastStopChange,
    MidiLog,
    Gain,
    ReverbMix,
    MidiPlayerStatus,
}

impl Default for LcdLineType {
    fn default() -> Self {
        Self::Empty
    }
}

impl fmt::Display for LcdLineType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LcdLineType::Empty => write!(f, "Empty"),
            LcdLineType::OrganName => write!(f, "Organ Name"),
            LcdLineType::SystemStatus => write!(f, "System Status"),
            LcdLineType::LastPreset => write!(f, "Last Preset"),
            LcdLineType::LastStopChange => write!(f, "Last Stop Change"),
            LcdLineType::MidiLog => write!(f, "Last MIDI Log"),
            LcdLineType::Gain => write!(f, "Gain"),
            LcdLineType::ReverbMix => write!(f, "Reverb Mix"),
            LcdLineType::MidiPlayerStatus => write!(f, "MIDI Player Status"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LcdDisplayConfig {
    pub id: u8, // 7-bit ID
    pub line1: LcdLineType,
    pub line2: LcdLineType,
    pub background_color: LcdColor,
}

impl Default for LcdDisplayConfig {
    fn default() -> Self {
        Self {
            id: 1,
            line1: LcdLineType::OrganName,
            line2: LcdLineType::SystemStatus,
            background_color: LcdColor::White,
        }
    }
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
            max_new_voices_per_block: default_max_new_voices_per_block(),
            audio_device_name: None,
            sample_rate: 48000,
            keyboard_layout: KeyboardLayout::Qwerty,
            midi_devices: Vec::new(),
            lcd_displays: Vec::new(),
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
    pub max_new_voices_per_block: usize,

    // --- Runtime-Only Settings ---
    pub midi_file: Option<PathBuf>,
    pub audio_device_name: Option<String>,
    pub sample_rate: u32,
    pub active_midi_devices: Vec<(MidiInputPort, MidiDeviceConfig)>,
    pub lcd_displays: Vec<LcdDisplayConfig>,
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

/// State shared between the local configuration UI (GUI/TUI) and the web
/// configuration server. Both read from and write to the same instance, so
/// changes made in either UI become visible in the other.
pub struct ConfigShared {
    pub state: ConfigState,
    /// MIDI input handle used for rescanning system ports on demand.
    pub midi_input_arc: Arc<Mutex<Option<MidiInput>>>,
    /// Set to Some by the web UI when the user clicks "Start". The local UI
    /// polls this; when set, it closes itself and main.rs uses the runtime
    /// config it contains.
    pub web_start_request: Option<RuntimeConfig>,
    /// Set to true when the web UI sends "Quit". The local UI polls this and
    /// closes when set.
    pub web_quit_request: bool,
    /// Bumped whenever shared state is mutated externally. The local UI uses
    /// this to decide whether to refresh internal mirror fields (e.g. the
    /// audio device combo's selected index).
    pub revision: u64,
}

impl ConfigShared {
    pub fn new(state: ConfigState, midi_input_arc: Arc<Mutex<Option<MidiInput>>>) -> Self {
        Self {
            state,
            midi_input_arc,
            web_start_request: None,
            web_quit_request: false,
            revision: 0,
        }
    }

    /// Re-enumerates connected MIDI ports and updates the shared state. New
    /// devices appear disabled by default; existing entries keep their
    /// enabled/mapping configuration. Devices that disappear stay in the
    /// settings list (so unplugging a device doesn't lose its mapping) but
    /// will not appear in `system_midi_ports`.
    pub fn rescan_midi_ports(&mut self) -> Result<()> {
        let mut new_ports = Vec::new();
        if let Some(midi_in) = self.midi_input_arc.lock().unwrap().as_ref() {
            for port in midi_in.ports() {
                if let Ok(name) = midi_in.port_name(&port) {
                    new_ports.push((port, name.clone()));
                    if !self
                        .state
                        .settings
                        .midi_devices
                        .iter()
                        .any(|d| d.name == name)
                    {
                        self.state.settings.midi_devices.push(MidiDeviceConfig {
                            name,
                            enabled: false,
                            ..Default::default()
                        });
                    }
                }
            }
        }
        self.state.system_midi_ports = new_ports;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
}

impl ConfigState {
    /// Create a deep copy of the state. Used to share state between the
    /// local UI's working snapshot and the shared `ConfigShared`.
    pub fn clone_snapshot(&self) -> Self {
        Self {
            settings: self.settings.clone(),
            midi_file: self.midi_file.clone(),
            system_midi_ports: self.system_midi_ports.clone(),
            error_msg: self.error_msg.clone(),
            available_audio_devices: self.available_audio_devices.clone(),
            selected_audio_device_name: self.selected_audio_device_name.clone(),
            available_sample_rates: self.available_sample_rates.clone(),
            available_ir_files: self.available_ir_files.clone(),
        }
    }

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
