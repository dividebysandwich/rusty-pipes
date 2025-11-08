use anyhow::{anyhow, Context, Result};
use ini::inistr; // Use the macro import
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::wav_converter;
use crate::wav_converter::SampleMetadata;

/// Top-level structure for the entire organ definition.
#[derive(Debug, Default)]
pub struct Organ {
    pub name: String,
    pub stops: Vec<Stop>,
    pub ranks: HashMap<String, Rank>, // Keyed by rank ID (e.g., "013")
    pub base_path: PathBuf, // The directory containing the .organ file
    pub sample_cache: Option<HashMap<PathBuf, Arc<Vec<f32>>>>, // Cache for loaded samples
    pub metadata_cache: Option<HashMap<PathBuf, Arc<SampleMetadata>>>, // Cache for loop points etc.
}

/// Represents a single stop (a button on the TUI).
#[derive(Debug)]
pub struct Stop {
    pub name: String,
    pub id_str: String, // e.g., "013"
    pub rank_ids: Vec<String>, // IDs of ranks it triggers
}

/// Represents a rank (a set of pipes).
#[allow(dead_code)]
#[derive(Debug)]
pub struct Rank {
    pub name: String,
    pub id_str: String, // e.g., "013"
    pub first_midi_note: u8,
    pub pipe_count: usize,
    pub gain_db: f32,
    pub tracker_delay_ms: u32,
    /// Keyed by MIDI note number (e.g., 36)
    pub pipes: HashMap<u8, Pipe>,
}

/// Represents a single pipe with its attack and release samples.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Pipe {
    pub attack_sample_path: PathBuf,
    pub gain_db: f32,
    pub pitch_tuning_cents: f32,
    pub releases: Vec<ReleaseSample>,
}

/// Represents a release sample and its trigger condition.
#[derive(Debug)]
pub struct ReleaseSample {
    pub path: PathBuf,
    /// Max key press time in ms. -1 means "default".
    pub max_key_press_time_ms: i64,
}

impl Organ {
    /// Loads and parses a .organ file.
    pub fn load(path: &Path, convert_to_16_bit: bool, pre_cache: bool) -> Result<Self> {
        println!("Loading organ from: {:?}", path);
        let base_path = path.parent().ok_or_else(|| anyhow!("Invalid file path"))?;
        if pre_cache {
            println!("[Organ] Pre-caching mode enabled. This may take a moment...");
        }
        
        // Read file to string
        let file_content = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("Failed to read organ file {:?}: {}", path, e))?;
        
        // Replace '#' with a safe placeholder
        // We use a placeholder that won't be in a real path.
        let safe_content = file_content.replace('#', "__HASH__");

        // Load the modified string
        let conf = inistr!(&safe_content);

        println!("Found {} sections in INI.", conf.len());

        let mut organ = Organ {
            base_path: base_path.to_path_buf(),
            name: path.file_stem().unwrap_or_default().to_string_lossy().to_string(),
            sample_cache: if pre_cache { Some(HashMap::new()) } else { None },
            metadata_cache: if pre_cache { Some(HashMap::new()) } else { None },
            ..Default::default()
        };

        let mut stops_map: HashMap<String, Stop> = HashMap::new();
        let mut ranks_map: HashMap<String, Rank> = HashMap::new();

        let mut stops_found = 0;
        let mut stops_filtered = 0;

        for (section_name, props) in &conf {

            let get_prop = |key_upper: &str, key_lower: &str, default: &str| {
                props.get(key_upper) // Try uppercase
                    .or_else(|| props.get(key_lower)) // Try lowercase
                    .and_then(|opt| opt.as_deref()) // Get Option<&str>
                    .map(|s| s.to_string()) // Convert to Option<String>
                    .unwrap_or_else(|| default.to_string()) // Provide default String
                    .trim()
                    .replace("__HASH__", "#") // <-- 4. Replace placeholder back
                    .to_string()

            };

            
            // --- Parse Stops ---
            if section_name.starts_with("stop") {
                stops_found += 1;
                let id_str = section_name.trim_start_matches("stop").to_string();
                
                let name = get_prop("Name", "name", "");
                // println!("Parsing stop {}: {}", id_str, name); // Debug line

                // Filter out noise/key action stops
                if name.contains("Key action") || name.contains("noise") || name.is_empty() {
                    stops_filtered += 1;
                    continue;
                }

                let rank_count: usize = get_prop("NumberOfRanks", "numberofranks", "0").parse().unwrap_or(0);
                let mut rank_ids = Vec::new();
                
                for i in 1..=rank_count {
                    let key_upper = format!("Rank{:03}", i);
                    let key_lower = format!("rank{:03}", i);
                    if let Some(rank_id) = get_prop(&key_upper, &key_lower, "").non_empty_or(None) {
                        rank_ids.push(rank_id.to_string());
                    }
                }

                stops_map.insert(id_str.clone(), Stop { name, id_str, rank_ids });
            }
            // --- Parse Ranks ---
            else if section_name.starts_with("rank") {
                let id_str = section_name.trim_start_matches("rank").to_string();
                let name = get_prop("Name", "name", "");

                let first_midi_note: u8 = get_prop("FirstMidiNoteNumber", "firstmidinotenumber", "0")
                    .parse()
                    .context(format!("Parsing FirstMidiNoteNumber for {section_name}"))?;
                
                let pipe_count: usize = get_prop("NumberOfLogicalPipes", "numberoflogicalpipes", "0")
                    .parse()
                    .context(format!("Parsing NumberOfLogicalPipes for {section_name}"))?;

                let gain_db: f32 = get_prop("Gain", "gain", "0.0").parse().unwrap_or(0.0);
                let tracker_delay_ms: u32 = get_prop("TrackerDelay", "trackerdelay", "0").parse().unwrap_or(0);
                
                let mut pipes = HashMap::new();
                for i in 1..=pipe_count {
                    let pipe_key_prefix_upper = format!("Pipe{:03}", i);
                    let pipe_key_prefix_lower = format!("pipe{:03}", i);
                    let midi_note = first_midi_note + (i as u8 - 1);

                    // Get attack sample
                    if let Some(attack_path_str) = get_prop(&pipe_key_prefix_upper, &pipe_key_prefix_lower, "").non_empty_or(None) {
                        // Handle path separators and create relative path
                        let attack_path_str = attack_path_str.replace('\\', "/");
                        let mut attack_sample_path_relative = PathBuf::from(&attack_path_str);

                        let pipe_gain_db: f32 = get_prop(
                            &format!("{}Gain", pipe_key_prefix_upper), 
                            &format!("{}gain", pipe_key_prefix_lower), 
                            "0.0"
                        ).parse().unwrap_or(0.0);
                        
                        let pitch_tuning_cents: f32 = get_prop(
                            &format!("{}PitchTuning", pipe_key_prefix_upper), 
                            &format!("{}pitchtuning", pipe_key_prefix_lower), 
                            "0.0"
                        ).parse().unwrap_or(0.0);

                        // Only process if needed (16-bit conversion OR pitch shift)
                            if convert_to_16_bit || pitch_tuning_cents != 0.0 {
                                attack_sample_path_relative = wav_converter::process_sample_file(
                                    &attack_sample_path_relative,
                                    &organ.base_path,
                                    pitch_tuning_cents,
                                    convert_to_16_bit
                                )?;
                            }

                        // Join with base path for the final absolute-like path
                        let attack_sample_path = organ.base_path.join(attack_sample_path_relative);

                        // Pre-cache attack sample if enabled
                        if let Some(audio_cache) = &mut organ.sample_cache {
                            if !audio_cache.contains_key(&attack_sample_path) {
                                match wav_converter::load_sample_as_f32(&attack_sample_path) {
                                    Ok((samples, metadata)) => {
                                        audio_cache.insert(attack_sample_path.clone(), Arc::new(samples));
                                        // Also cache the metadata
                                        organ.metadata_cache.as_mut().unwrap().insert(attack_sample_path.clone(), Arc::new(metadata));
                                    }
                                    Err(e) => {
                                        log::error!("[Cache] Failed to load sample {:?}: {}", attack_sample_path, e);
                                    }
                                }
                            }
                        }

                        let release_count: usize = get_prop(
                            &format!("{}ReleaseCount", pipe_key_prefix_upper), 
                            &format!("{}releasecount", pipe_key_prefix_lower), 
                            "0"
                        ).parse().unwrap_or(0);
                        
                        let mut releases = Vec::new();

                        for r_idx in 1..=release_count {
                            let rel_key_upper = format!("{}Release{:03}", pipe_key_prefix_upper, r_idx);
                            let rel_key_lower = format!("{}release{:03}", pipe_key_prefix_lower, r_idx);
                            
                            if let Some(rel_path_str) = get_prop(&rel_key_upper, &rel_key_lower, "").non_empty_or(None) {
                                let rel_path_str = rel_path_str.replace('\\', "/");
                                let mut rel_path_relative = PathBuf::from(&rel_path_str);

                                // Run converter
                                if convert_to_16_bit || pitch_tuning_cents != 0.0 {
                                    rel_path_relative = wav_converter::process_sample_file(
                                        &rel_path_relative,
                                        &organ.base_path,
                                        pitch_tuning_cents, // Use the pipe's pitch
                                        convert_to_16_bit
                                    )?;
                                }

                                let rel_path = organ.base_path.join(rel_path_relative);
                                
                                // Pre-cache release sample if enabled
                                if let Some(audio_cache) = &mut organ.sample_cache {
                                    if !audio_cache.contains_key(&rel_path) {
                                         match wav_converter::load_sample_as_f32(&rel_path) {
                                            Ok((samples, metadata)) => {
                                                audio_cache.insert(rel_path.clone(), Arc::new(samples));
                                                organ.metadata_cache.as_mut().unwrap().insert(rel_path.clone(), Arc::new(metadata));
                                            }
                                            Err(e) => {
                                                log::error!("[Cache] Failed to load sample {:?}: {}", rel_path, e);
                                            }
                                        }
                                    }
                                }
                                
                                let time_key_upper = format!("{}MaxKeyPressTime", rel_key_upper);
                                let time_key_lower = format!("{}maxkeypresstime", rel_key_lower);
                                
                                let max_time: i64 = get_prop(&time_key_upper, &time_key_lower, "-1").parse().unwrap_or(-1);
                                releases.push(ReleaseSample { path: rel_path, max_key_press_time_ms: max_time });
                            }
                        }
                        
                        // Sort releases by time, smallest first. -1 (default) should be last.
                        releases.sort_by_key(|r| if r.max_key_press_time_ms == -1 { i64::MAX } else { r.max_key_press_time_ms });

                        pipes.insert(midi_note, Pipe {
                            attack_sample_path,
                            gain_db: pipe_gain_db,
                            pitch_tuning_cents: 0.0, // Pitch tuning is pre-applied during conversion
                            releases,
                        });
                    }
                }
                ranks_map.insert(id_str.clone(), Rank { name, id_str, first_midi_note, pipe_count, gain_db, tracker_delay_ms, pipes });
            }
        }
        
        println!("Parsing complete. Stops found: {}. Stops filtered (noise/empty): {}. Stops added: {}.", stops_found, stops_filtered, stops_map.len());

        // Convert stops_map to a vec and sort it for stable TUI display
        let mut stops: Vec<Stop> = stops_map.into_values().collect();
        stops.sort_by(|a, b| a.id_str.cmp(&b.id_str)); // Sort by ID string

        organ.stops = stops;
        organ.ranks = ranks_map;

        Ok(organ)
    }
}

// Helper trait to treat empty strings as None
trait NonEmpty: Sized {
    fn non_empty_or(self, default: Option<Self>) -> Option<Self>;
}

impl NonEmpty for String {
    fn non_empty_or(self, default: Option<Self>) -> Option<Self> {
        if self.is_empty() {
            default
        } else {
            Some(self)
        }
    }
}


