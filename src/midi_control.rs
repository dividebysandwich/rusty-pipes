use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use anyhow::Result;

use crate::config::MidiEventSpec;

// Defines how a specific internal organ channel (0-15) is controlled for a specific stop
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StopChannelControl {
    pub enable_event: Option<MidiEventSpec>,
    pub disable_event: Option<MidiEventSpec>,
}

// The master map: Stop Index -> Internal Virtual Channel (0-15) -> Control Config
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MidiControlMap {
    // Map<StopIndex, Map<InternalChannel, Control>>
    pub stops: HashMap<usize, HashMap<u8, StopChannelControl>>,
}

impl MidiControlMap {
    pub fn new() -> Self {
        Self { stops: HashMap::new() }
    }

    pub fn get_file_path(organ_name: &str) -> PathBuf {
        let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")
            .expect("Could not get configuration file path");
        let parent = config_path.parent().expect("Could not get config parent");
        // Sanitize organ name for filename
        let safe_name: String = organ_name.chars()
            .map(|x| if x.is_alphanumeric() { x } else { '_' })
            .collect();
        parent.join(format!("{}.midi_map.json", safe_name))
    }

    pub fn load(organ_name: &str) -> Self {
        let path = Self::get_file_path(organ_name);
        if path.exists() {
            if let Ok(file) = File::open(&path) {
                let reader = BufReader::new(file);
                if let Ok(map) = serde_json::from_reader(reader) {
                    return map;
                }
            }
        }
        Self::new()
    }

    pub fn save(&self, organ_name: &str) -> Result<()> {
        let path = Self::get_file_path(organ_name);
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)?;
        Ok(())
    }

    /// Assigns a MIDI event to a specific function (Enable or Disable)
    pub fn learn(
        &mut self, 
        stop_index: usize, 
        internal_channel: u8, 
        event: MidiEventSpec, 
        is_enable_action: bool
    ) {
        let stop_entry = self.stops.entry(stop_index).or_default();
        let ch_entry = stop_entry.entry(internal_channel).or_default();

        if is_enable_action {
            ch_entry.enable_event = Some(event);
        } else {
            ch_entry.disable_event = Some(event);
        }
    }

    pub fn clear(&mut self, stop_index: usize, internal_channel: u8) {
        if let Some(stop_entry) = self.stops.get_mut(&stop_index) {
            stop_entry.remove(&internal_channel);
        }
    }

    /// Checks incoming MIDI against the map and returns a list of actions to take.
    /// Returns: Vec<(StopIndex, InternalChannel, SetActive)>
    pub fn check_event(&self, incoming: &MidiEventSpec) -> Vec<(usize, u8, bool)> {
        let mut actions = Vec::new();

        for (stop_idx, channel_map) in &self.stops {
            for (internal_channel, control) in channel_map {
                // Check Enable Trigger
                if let Some(trigger) = &control.enable_event {
                    if trigger == incoming {
                        actions.push((*stop_idx, *internal_channel, true));
                    }
                }
                // Check Disable Trigger
                if let Some(trigger) = &control.disable_event {
                    if trigger == incoming {
                        actions.push((*stop_idx, *internal_channel, false));
                    }
                }
            }
        }
        actions
    }
}