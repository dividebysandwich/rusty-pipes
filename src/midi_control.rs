use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

use crate::config::MidiEventSpec;

// Defines how a control (Stop channel or Tremulant) is toggled
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StopChannelControl {
    pub enable_event: Option<MidiEventSpec>,
    pub disable_event: Option<MidiEventSpec>,
}

/// Unified action type returned when checking events
#[derive(Debug, PartialEq)]
pub enum ControlAction {
    SetStop {
        index: usize,
        internal_channel: u8,
        active: bool,
    },
    SetTremulant {
        id: String,
        active: bool,
    },
    LoadPreset {
        slot_index: usize,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MidiControlMap {
    // Map<StopIndex, Map<InternalChannel, Control>>
    pub stops: HashMap<usize, HashMap<u8, StopChannelControl>>,

    // Map<TremulantID, Control>
    #[serde(default)]
    pub tremulants: HashMap<String, StopChannelControl>,

    // Map<PresetSlotIndex, Trigger> (0-11)
    #[serde(default)]
    pub presets: HashMap<usize, Option<MidiEventSpec>>,
}

impl MidiControlMap {
    pub fn new() -> Self {
        Self {
            stops: HashMap::new(),
            tremulants: HashMap::new(),
            presets: HashMap::new(),
        }
    }

    pub fn get_file_path(organ_name: &str) -> PathBuf {
        let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")
            .expect("Could not get configuration file path");
        let parent = config_path.parent().expect("Could not get config parent");
        let safe_name: String = organ_name
            .chars()
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

    pub fn learn_stop(
        &mut self,
        stop_index: usize,
        internal_channel: u8,
        event: MidiEventSpec,
        is_enable_action: bool,
    ) {
        let stop_entry = self.stops.entry(stop_index).or_default();
        let ch_entry = stop_entry.entry(internal_channel).or_default();

        if is_enable_action {
            ch_entry.enable_event = Some(event);
        } else {
            ch_entry.disable_event = Some(event);
        }
    }

    pub fn learn_tremulant(
        &mut self,
        trem_id: String,
        event: MidiEventSpec,
        is_enable_action: bool,
    ) {
        let entry = self.tremulants.entry(trem_id).or_default();
        if is_enable_action {
            entry.enable_event = Some(event);
        } else {
            entry.disable_event = Some(event);
        }
    }

    pub fn learn_preset(&mut self, slot_index: usize, event: MidiEventSpec) {
        self.presets.insert(slot_index, Some(event));
    }

    pub fn clear_stop(&mut self, stop_index: usize, internal_channel: u8) {
        if let Some(stop_entry) = self.stops.get_mut(&stop_index) {
            stop_entry.remove(&internal_channel);
        }
    }

    pub fn clear_tremulant(&mut self, trem_id: &str) {
        self.tremulants.remove(trem_id);
    }

    pub fn clear_preset(&mut self, slot_index: usize) {
        self.presets.remove(&slot_index);
    }

    /// Checks incoming MIDI against the map and returns a list of actions to take.
    pub fn check_event(&self, incoming: &MidiEventSpec) -> Vec<ControlAction> {
        let mut actions = Vec::new();

        // Check Stops
        for (stop_idx, channel_map) in &self.stops {
            for (internal_channel, control) in channel_map {
                if let Some(trigger) = &control.enable_event {
                    if trigger == incoming {
                        actions.push(ControlAction::SetStop {
                            index: *stop_idx,
                            internal_channel: *internal_channel,
                            active: true,
                        });
                    }
                }
                if let Some(trigger) = &control.disable_event {
                    if trigger == incoming {
                        actions.push(ControlAction::SetStop {
                            index: *stop_idx,
                            internal_channel: *internal_channel,
                            active: false,
                        });
                    }
                }
            }
        }

        // Check Tremulants
        for (trem_id, control) in &self.tremulants {
            if let Some(trigger) = &control.enable_event {
                if trigger == incoming {
                    actions.push(ControlAction::SetTremulant {
                        id: trem_id.clone(),
                        active: true,
                    });
                }
            }
            if let Some(trigger) = &control.disable_event {
                if trigger == incoming {
                    actions.push(ControlAction::SetTremulant {
                        id: trem_id.clone(),
                        active: false,
                    });
                }
            }
        }

        // Check Presets
        for (slot, trigger_opt) in &self.presets {
            if let Some(trigger) = trigger_opt {
                if trigger == incoming {
                    actions.push(ControlAction::LoadPreset { slot_index: *slot });
                }
            }
        }

        actions
    }
}
