use anyhow::{anyhow, Context, Result};
use ini::inistr;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use serde::Deserialize;
use quick_xml::de::from_str;
use itertools::Itertools;
use rayon::prelude::*;

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
#[derive(Debug, Clone)]
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

// These structs are defined *only* for XML deserialization.
// They mirror the Hauptwerk XML file structure.

fn default_string() -> String { "".to_string() }
fn default_i64() -> i64 { -1 } // -1 for default release

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "Hauptwerk")]
struct HauptwerkXml {
    #[serde(rename = "ObjectList", default)]
    object_lists: Vec<ObjectList>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ObjectList {
    #[serde(rename = "@ObjectType")]
    object_type: String,

    #[serde(rename = "Stop", default)]
    stops: Vec<XmlStop>,
    #[serde(rename = "Rank", default)]
    ranks: Vec<XmlRank>,
    #[serde(rename = "StopRank", default)]
    stop_ranks: Vec<XmlStopRank>,
    #[serde(rename = "_General", default)]
    general: Vec<XmlGeneral>,

    #[serde(rename = "Pipe_SoundEngine01", default)]
    pipes: Vec<XmlPipe>,
    
    #[serde(rename = "Pipe_SoundEngine01_Layer", default)]
    layers: Vec<XmlLayer>,
    
    #[serde(rename = "Pipe_SoundEngine01_AttackSample", default)]
    attack_samples: Vec<XmlAttackSample>,
    
    #[serde(rename = "Pipe_SoundEngine01_ReleaseSample", default)]
    release_samples: Vec<XmlReleaseSample>,

    #[serde(rename = "Sample", default)]
    samples: Vec<XmlSample>,

    #[serde(rename = "Combination", default)]
    combinations: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "WindCompartmentLinkage", default)]
    wind_linkages: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "CombinationElement", default)]
    combination_elements: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "Switch", default)]
    switches: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "DisplayPage", default)]
    display_pages: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "Enclosure", default)]
    enclosures: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "Key", default)]
    keys: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "Winding", default)]
    windings: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "ContinuousControl", default)]
    continuous_controls: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "ContinuousControlImageSetStage", default)]
    cc_image_set_stages: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "ContinuousControlLinkage", default)]
    cc_linkages: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "ContinuousControlStageSwitch", default)]
    cc_stage_switches: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "Division", default)]
    divisions: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "DivisionInput", default)]
    division_inputs: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "ImageSet", default)]
    image_sets: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "ImageSetElement", default)]
    image_set_elements: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "ImageSetInstance", default)]
    image_set_instances: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "KeyAction", default)]
    key_actions: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "KeyImageSet", default)]
    key_image_sets: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "Keyboard", default)]
    keyboards: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "RequiredInstallationPackage", default)]
    req_packages: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "SwitchLinkage", default)]
    switch_linkages: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "WindCompartment", default)]
    wind_compartments: Vec<serde::de::IgnoredAny>
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlGeneral {
    #[serde(rename = "Name", default = "default_string")]
    name: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlStop {
    #[serde(rename = "StopID")]
    id: String,
    #[serde(rename = "Name", default = "default_string")]
    name: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlStopRank {
    #[serde(rename = "StopID")]
    stop_id: String,
    #[serde(rename = "RankID")]
    rank_id: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlRank {
    #[serde(rename = "RankID")]
    id: String,
    #[serde(rename = "Name", default = "default_string")]
    name: String,
}

// From ObjectType="Pipe_SoundEngine01"
#[derive(Debug, Deserialize, PartialEq)]
struct XmlPipe {
    #[serde(rename = "PipeID")]
    id: String,
    #[serde(rename = "RankID")]
    rank_id: String,
    #[serde(rename = "NormalMIDINoteNumber")]
    midi_note: u8,
}

// From ObjectType="Pipe_SoundEngine01_Layer"
#[derive(Debug, Deserialize, PartialEq)]
struct XmlLayer {
    //#[serde(rename = "Pipe_SoundEngine01_LayerID")]
    #[serde(rename = "LayerID")]
    id: String,
    #[serde(rename = "PipeID")]
    pipe_id: String,
}

// From ObjectType="Sample"
#[derive(Debug, Deserialize, PartialEq)]
struct XmlSample {
    #[serde(rename = "SampleID")]
    id: String,
    #[serde(rename = "SampleFilename", default = "default_string")]
    path: String,
    #[serde(rename = "InstallationPackageID", default = "default_string")]
    installation_package_id: String,
    pitch_exact_sample_pitch: Option<f32>,
    pitch_normal_midi_note_number: Option<u8>,
}

// From ObjectType="Pipe_SoundEngine01_AttackSample"
#[derive(Debug, Deserialize, PartialEq)]
struct XmlAttackSample {
    #[serde(rename = "LayerID")]
    layer_id: String,
    #[serde(rename = "SampleID")]
    sample_id: String,
    // The "path" field is removed
}

// From ObjectType="Pipe_SoundEngine01_ReleaseSample"
#[derive(Debug, Deserialize, PartialEq)]
struct XmlReleaseSample {
    #[serde(rename = "LayerID")] // Assuming it links to Layer
    layer_id: String,
    #[serde(rename = "MaxKeypressTimeMilliseconds", default = "default_i64")]
    max_key_press_time_ms: i64,
    #[serde(rename = "SampleID")]
    sample_id: String,
}

impl Organ {
    /// Loads and parses an organ file (either .organ or .Organ_Hauptwerk_xml).
    /// This function dispatches to the correct parser based on the file extension.
    pub fn load(
        path: &Path, 
        convert_to_16_bit: bool, 
        pre_cache: bool, 
        original_tuning: bool,
        progress_tx: Option<mpsc::Sender<(f32, String)>>,
    ) -> Result<Self> {
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        
        // Parse the organ definition and build the struct
        let mut organ = if extension == "organ" {
            Self::load_grandorgue(path, convert_to_16_bit, false, original_tuning)?
        } else if extension == "Organ_Hauptwerk_xml" {
            Self::load_hauptwerk(path, convert_to_16_bit, false, original_tuning)?
        } else {
            return Err(anyhow!("Unsupported organ file format: {:?}", path));
        };

        if pre_cache {
            log::info!("[Organ] Pre-caching mode enabled. This may take a moment...");
            
            // Initialize the caches
            organ.sample_cache = Some(HashMap::new());
            organ.metadata_cache = Some(HashMap::new());
            
            // Run the parallel loader
            organ.run_parallel_precache(progress_tx)?;
        }
        Ok(organ)

    }

    /// Collects all unique sample paths from all pipes in the organ.
    fn get_all_unique_sample_paths(&self) -> HashSet<PathBuf> {
        let mut paths = HashSet::new();
        for rank in self.ranks.values() {
            for pipe in rank.pipes.values() {
                paths.insert(pipe.attack_sample_path.clone());
                for release in &pipe.releases {
                    paths.insert(release.path.clone());
                }
            }
        }
        paths
    }

    /// Runs the pre-caching in parallel after the organ struct is built.
    fn run_parallel_precache(&mut self, progress_tx: Option<mpsc::Sender<(f32, String)>>) -> Result<()> {
        
        let paths_to_load: Vec<PathBuf> = self.get_all_unique_sample_paths().into_iter().collect();
        let total_samples = paths_to_load.len();
        if total_samples == 0 {
            log::warn!("[Cache] Pre-cache enabled, but no sample paths were found.");
            return Ok(());
        }

        let loaded_sample_count = AtomicUsize::new(0);
        let tx_clone = progress_tx.clone(); // Clone sender for parallel use

        log::info!("[Cache] Loading {} unique samples using all available CPU cores...", total_samples);

        // Use Rayon to load samples in parallel and collect the results
        let results: Vec<Result<(PathBuf, Arc<Vec<f32>>, Arc<SampleMetadata>)>> = paths_to_load
            .par_iter()
            .map(|path| {
                // This closure runs on a different thread
                let (samples, metadata) = wav_converter::load_sample_as_f32(path)
                    .with_context(|| format!("Failed to load sample {:?}", path))?;

                // Report progress atomically
                let count = loaded_sample_count.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some(tx) = &tx_clone {
                    let progress = count as f32 / total_samples as f32;
                    let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    let _ = tx.send((progress, file_name)); // Send can fail if UI is closed
                }
                
                Ok((path.clone(), Arc::new(samples), Arc::new(metadata)))
            })
            .collect(); // Collect all results

        // Now, serially insert the results back into the main struct's hashmaps
        let sample_cache = self.sample_cache.as_mut().unwrap();
        let metadata_cache = self.metadata_cache.as_mut().unwrap();

        for result in results {
            match result {
                Ok((path, samples, metadata)) => {
                    sample_cache.insert(path.clone(), samples);
                    metadata_cache.insert(path, metadata);
                }
                Err(e) => {
                    // Log the error but continue. One failed sample shouldn't stop the app.
                    log::error!("[Cache] {}", e);
                }
            }
        }
        
        log::info!("Pre-caching complete. Loaded {}/{} samples.", sample_cache.len(), total_samples);
        Ok(())
    }

    fn try_infer_midi_note_from_filename(path_str: &str) -> Option<f32> {
        let path = Path::new(path_str);
        
        // Get the filename stem, e.g., "052-e" from "052-e.wav"
        let stem = path.file_stem().and_then(|s| s.to_str())?;
        
        // Take the part before a potential '-', e.g., "052" from "052-e"
        let note_str = stem.split('-').next()?;
        
        // Try to parse just that part as a number
        match note_str.parse::<u8>() {
            Ok(midi_note) => Some(midi_note as f32),
            Err(_) => None // Failed to parse
        }
    }

    /// Loads and parses a Hauptwerk (.Organ_Hauptwerk_xml) file.
    fn load_hauptwerk(
        path: &Path, 
        convert_to_16_bit: bool, 
        pre_cache: bool, 
        _original_tuning: bool,
    ) -> Result<Self> {
        println!("Loading Hauptwerk organ from: {:?}", path);
        
        let organ_root_path = path.parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| anyhow!("Invalid Hauptwerk file path structure. Expected .../OrganDefinitions/*.Organ_Hauptwerk_xml"))?;
        
        let file_content = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("Failed to read Hauptwerk XML file {:?}: {}", path, e))?;

        let mut organ = Organ {
            base_path: organ_root_path.to_path_buf(),
            name: path.file_stem().unwrap_or_default().to_string_lossy().replace(".Organ_Hauptwerk_xml", ""),
            sample_cache: if pre_cache { Some(HashMap::new()) } else { None },
            metadata_cache: if pre_cache { Some(HashMap::new()) } else { None },
            ..Default::default()
        };

        if pre_cache {
            organ.sample_cache = Some(HashMap::new());
            organ.metadata_cache = Some(HashMap::new());
        }

        // Deserialize the XML
        log::debug!("Parsing XML file...");
        let xml_data: HauptwerkXml = from_str(&file_content)
            .map_err(|e| anyhow!("Failed to parse Hauptwerk XML: {}", e))?;
        log::debug!("XML parsing complete. Found {} ObjectList entries.", xml_data.object_lists.len());

        // Extract and organize XML objects
        let mut xml_stops = Vec::new();
        let mut xml_ranks = Vec::new();
        let mut xml_pipes = Vec::new();    
        let mut xml_layers = Vec::new();
        let mut xml_attack_samples = Vec::new();
        let mut xml_release_samples = Vec::new();
        let mut xml_stop_ranks = Vec::new();
        let mut xml_generals = Vec::new();
        let mut xml_samples = Vec::new();

        for list in xml_data.object_lists {
            match list.object_type.as_str() {
                "Stop" => xml_stops.extend(list.stops),
                "Rank" => xml_ranks.extend(list.ranks),
                "StopRank" => xml_stop_ranks.extend(list.stop_ranks),
                "_General" => xml_generals.extend(list.general),
                "Pipe_SoundEngine01" => xml_pipes.extend(list.pipes),
                "Pipe_SoundEngine01_Layer" => xml_layers.extend(list.layers),
                "Pipe_SoundEngine01_AttackSample" => xml_attack_samples.extend(list.attack_samples),
                "Pipe_SoundEngine01_ReleaseSample" => xml_release_samples.extend(list.release_samples),
                "Sample" => xml_samples.extend(list.samples),
                _ => {} // Ignore other object types
            }
        }
        
        log::debug!("Collected: {} stops, {} ranks, {} stop-ranks.", xml_stops.len(), xml_ranks.len(), xml_stop_ranks.len());
        log::debug!("Collected: {} pipes, {} layers, {} attacks, {} releases, {} samples.",
            xml_pipes.len(), xml_layers.len(), xml_attack_samples.len(), xml_release_samples.len(), xml_samples.len());

        // Set organ name
        if let Some(general) = xml_generals.first() {
            if !general.name.is_empty() {
                organ.name = general.name.clone();
            }
        }
        
        // Build StopRank map (StopID -> Vec<RankID>)
        let mut stop_to_ranks_map: HashMap<String, Vec<String>> = HashMap::new();
        for sr in xml_stop_ranks {
            stop_to_ranks_map
                .entry(sr.stop_id)
                .or_default()
                .push(sr.rank_id);
        }

        // Build Ranks map (empty pipes)
        let mut ranks_map: HashMap<String, Rank> = HashMap::new();
        for xr in xml_ranks {
            ranks_map.insert(xr.id.clone(), Rank {
                name: xr.name,
                id_str: xr.id,
                pipe_count: 0,
                pipes: HashMap::new(),
                first_midi_note: 0,
                gain_db: 0.0,
                tracker_delay_ms: 0,
            });
        }
        log::debug!("Created {} empty ranks.", ranks_map.len());

        // Assemble Pipes
        
        // Create lookup maps for quick assembly
        let pipe_map: HashMap<String, &XmlPipe> = xml_pipes.iter().map(|p| (p.id.clone(), p)).collect();
        
        // Map SampleID -> Sample (for filename and pitch)
        let sample_map: HashMap<String, &XmlSample> = xml_samples.iter()
            .filter(|s| !s.path.is_empty()) // Only map samples that have a path
            .map(|s| (s.id.clone(), s))
            .collect();

        
        // Map LayerID -> AttackSample (for SampleID)
        let attack_map: HashMap<String, &XmlAttackSample> = xml_attack_samples.iter()
            .map(|a| (a.layer_id.clone(), a))
            .collect();
        
        // Map LayerID -> Vec<ReleaseSamples> (for SampleID)
        let mut release_map: HashMap<String, Vec<&XmlReleaseSample>> = HashMap::new();
        for rel in &xml_release_samples {
             release_map.entry(rel.layer_id.clone()).or_default().push(rel);
        }

        log::debug!("Built Sample map ({} entries), Attack map ({} entries).", sample_map.len(), attack_map.len());

        log::debug!("Assembling pipes from {} layers...", xml_layers.len());
        let mut pipes_assembled = 0;

        // Iterate layers (the central object) and build pipes
        for layer in &xml_layers {
            // Find the pipe this layer belongs to
            let Some(pipe_info) = pipe_map.get(&layer.pipe_id) else {
                log::warn!("Layer {} references non-existent PipeID {}", layer.id, layer.pipe_id);
                continue;
            };

            // Find the rank this pipe belongs to
            let Some(rank) = ranks_map.get_mut(&pipe_info.rank_id) else {
                log::debug!("Pipe {} references non-existent RankID {}", pipe_info.id, pipe_info.rank_id);
                continue;
            };

            // Find the Attack linking object (LayerID -> SampleID)
            let Some(attack_link) = attack_map.get(&layer.id) else {
                log::warn!("Layer {} has no attack sample link.", layer.id);
                continue;
            };

            // Find the Sample object (SampleID -> SampleFileName)
            let Some(attack_sample_info) = sample_map.get(&attack_link.sample_id) else {
                log::warn!("Layer {} references non-existent SampleID {}", layer.id, attack_link.sample_id);
                continue;
            };
            
            // Get Target Pitch (the note this pipe should play)
            let target_midi_note = pipe_info.midi_note as f32;

            // Get Original Pitch (the recorded pitch of the .wav file)
            // We must convert it to a MIDI note number for comparison.
            let original_midi_note = if let Some(pitch_hz) = attack_sample_info.pitch_exact_sample_pitch {
                if pitch_hz > 0.0 {
                    // Use exact pitch (Hz) if available
                    // Formula: MIDI_note = 12 * log2(freq_hz / 440.0) + 69.0
                    let pitch = 12.0 * (pitch_hz / 440.0).log2() + 69.0;
                    log::debug!("SampleID {} has Pitch_ExactSamplePitch {} Hz, target MIDI note {:.2}.",
                        attack_link.sample_id, pitch_hz, target_midi_note);

                    pitch
                } else {
                    // Invalid pitch, fallback to target_midi_note to get 0 shift
                    log::warn!("SampleID {} has invalid Pitch_ExactSamplePitch {}. Assuming no shift.", 
                        attack_link.sample_id, pitch_hz);

                    target_midi_note
                }
            } else if let Some(midi_note) = attack_sample_info.pitch_normal_midi_note_number {
                log::debug!("SampleID {} has Pitch_NormalMidiNoteNumber {}, target MIDI note {:.2}.",
                    attack_link.sample_id, midi_note, target_midi_note);
                // Fallback to MIDI note number if pitch in Hz is not specified
                midi_note as f32
            } else {
                // Priority 3: No pitch defined in XML. Try to infer from filename.
                if let Some(inferred_note) = Self::try_infer_midi_note_from_filename(&attack_sample_info.path) {
                    if (inferred_note - target_midi_note).abs() > 0.01 { // Check if it's actually different
                        log::info!("SampleID {} has no defined pitch. Inferred original pitch {} from filename '{}' (target is {}).",
                            attack_link.sample_id, inferred_note, attack_sample_info.path, target_midi_note);
                    } else {
                        // Inferred note matches target, so no shift. This is the common case.
                        log::debug!("SampleID {} has no defined pitch. Inferred pitch {} from filename, matches target {}.",
                            attack_link.sample_id, inferred_note, target_midi_note);
                    }
                    inferred_note // Use the inferred note
                } else {
                    // Priority 4: Filename parsing failed, fallback to original behavior (no shift)
                    log::warn!("SampleID {} has no defined pitch and filename '{}' could not be parsed. Assuming pitch matches target {}.",
                        attack_link.sample_id, attack_sample_info.path, target_midi_note);
                    target_midi_note // Fallback
                }
            };

            // Calculate the coarse pitch shift in cents
            // This is the difference between where it *should* play and where it *was* recorded.
            let final_pitch_tuning_cents = (target_midi_note - original_midi_note) * 100.0;


            // Process Attack Sample
            let attack_path_str = format!("OrganInstallationPackages/{:0>6}/{}", attack_sample_info.installation_package_id,attack_sample_info.path.replace('\\', "/"));
            let mut attack_sample_path_relative = PathBuf::from(&attack_path_str);
            
            log::debug!("Processing LayerID {}: midi note {}, Attack sample path '{}'", layer.id, target_midi_note, attack_sample_path_relative.display());

            if convert_to_16_bit || final_pitch_tuning_cents != 0.0 {
                attack_sample_path_relative = wav_converter::process_sample_file(
                    &attack_sample_path_relative,
                    &organ.base_path,
                    final_pitch_tuning_cents,
                    convert_to_16_bit,
                )?;
            }

            if final_pitch_tuning_cents != 0.0 {
                log::debug!("Pipe (LayerID {}) attack sample '{}' retuned by {:.2} cents (Target MIDI: {}, Original MIDI: {}). File: {}",
                    layer.id, attack_sample_info.path, final_pitch_tuning_cents, target_midi_note, original_midi_note, attack_sample_path_relative.display());
            }

            let attack_sample_path = organ.base_path.join(attack_sample_path_relative);

            // Process Release Samples (needs double-lookup too)
            let mut releases = Vec::new();
            if let Some(xml_release_links) = release_map.get(&layer.id) {
                for release_link in xml_release_links {
                    // Find the Sample object for this release
                    let Some(release_sample_info) = sample_map.get(&release_link.sample_id) else {
                        log::warn!("Release for Layer {} references non-existent SampleID {}", layer.id, release_link.sample_id);
                        continue;
                    };

                    let rel_path_str = format!("OrganInstallationPackages/{:0>6}/{}", release_sample_info.installation_package_id, release_sample_info.path.replace('\\', "/"));

                    let mut rel_path_relative = PathBuf::from(&rel_path_str);

                    if convert_to_16_bit || final_pitch_tuning_cents != 0.0 {
                        rel_path_relative = wav_converter::process_sample_file(
                            &rel_path_relative,
                            &organ.base_path,
                            final_pitch_tuning_cents,
                            convert_to_16_bit,
                        )?;
                    }
                    let rel_path = organ.base_path.join(rel_path_relative);

                    releases.push(ReleaseSample {
                        path: rel_path,
                        max_key_press_time_ms: release_link.max_key_press_time_ms,
                    });
                }
            }
            
            releases.sort_by_key(|r| if r.max_key_press_time_ms == -1 { i64::MAX } else { r.max_key_press_time_ms });

            // Create and insert Pipe into its Rank
            let final_pipe = Pipe {
                attack_sample_path,
                gain_db: 0.0,
                pitch_tuning_cents: 0.0,
                releases,
            };

            if let Some(_existing) = rank.pipes.insert(pipe_info.midi_note, final_pipe) {
                log::warn!("Duplicate pipe for MIDI note {} in Rank {}. Overwriting.",
                    pipe_info.midi_note, rank.id_str);
            }
            pipes_assembled += 1; // Count successfully assembled pipes
        }

        log::debug!("Pipe assembly loop finished. Assembled {} pipes.", pipes_assembled);

        // Final Assembly
        
        // Update pipe counts in ranks
        for rank in ranks_map.values_mut() {
            rank.pipe_count = rank.pipes.len();
            // Optionally find the first MIDI note
            if let Some(first_key) = rank.pipes.keys().next() {
                rank.first_midi_note = *first_key;
            }
        }

        // Build Stops Vec
        let mut stops_filtered = 0;
        let mut stops_map: HashMap<String, Stop> = HashMap::new();
        log::debug!("--- Starting Stop Filtering ---");
        let xml_stops_len = xml_stops.len();
        for xs in xml_stops {
            // Apply same filter as INI loader
            if xs.name.contains("Key action") || xs.name.contains("noise") || xs.name.is_empty() {
                stops_filtered += 1;
                continue;
            }

            log::debug!("Filtering stop '{}' (ID: {})", xs.name, xs.id);
            let rank_ids = stop_to_ranks_map.get(&xs.id).cloned().unwrap_or_default();
            
            if rank_ids.is_empty() {
                log::debug!("-> Stop {} has no associated rank IDs.", xs.id);
            } else {
                log::debug!("-> Stop {} is associated with rank IDs: {:?}", xs.id, rank_ids);
            }

            let has_pipes = rank_ids.iter().any(|rid| {
                if let Some(rank) = ranks_map.get(rid) {
                    if !rank.pipes.is_empty() {
                        log::debug!("-> Rank {} (Name: {}) has {} pipes. OK.", rid, rank.name, rank.pipes.len());
                        // List pipes and their samples for debugging, ordered by midi_note
                        for (midi_note, pipe) in rank.pipes.iter().sorted_by_key(|(k, _)| *k) {
                            log::debug!("   - MIDI Note {}: Attack Sample: {}", midi_note, pipe.attack_sample_path.display());
                            for release in &pipe.releases {
                                log::debug!("       - Release Sample: {} (Max Key Press Time: {} ms)", release.path.display(), release.max_key_press_time_ms);
                            }
                        }

                        true
                        
                    } else {
                        log::debug!("-> Rank {} (Name: {}) has 0 pipes.", rid, rank.name);
                        false
                    }
                } else {
                    log::debug!("-> Rank ID {} not found in ranks_map.", rid);
                    false
                }
            });

            if !rank_ids.is_empty() && has_pipes {
                log::debug!("-> SUCCESS: Adding stop '{}'", xs.name);
                stops_map.insert(xs.id.clone(), Stop {
                    name: xs.name,
                    id_str: xs.id,
                    rank_ids,
                });
            } else {
                log::debug!("-> FILTERED: Stop '{}' (ID: {}): No (valid) ranks associated or ranks have no pipes.", xs.name, xs.id);
                 stops_filtered += 1;
            }
        }
        log::debug!("--- Stop Filtering Finished ---");

        println!("Parsing complete. Stops found: {}. Stops filtered (noise/empty/no pipes): {}. Stops added: {}.", xml_stops_len, stops_filtered, stops_map.len());

        let mut stops: Vec<Stop> = stops_map.into_values().collect();
        stops.sort_by_key(|s| s.id_str.parse::<u32>().unwrap_or(0));

        organ.stops = stops;
        organ.ranks = ranks_map;

        log::debug!("Final maps: {} stops, {} ranks.", organ.stops.len(), organ.ranks.len());
        Ok(organ)
    }
    
    /// Loads and parses a GrandOrgue (.organ) file.
    fn load_grandorgue(
        path: &Path, 
        convert_to_16_bit: bool, 
        pre_cache: bool, 
        original_tuning: bool,
    ) -> Result<Self> {
        println!("Loading GrandOrgue organ from: {:?}", path);
        let base_path = path.parent().ok_or_else(|| anyhow!("Invalid file path"))?;
        
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
            sample_cache: None,
            metadata_cache: None,
            ..Default::default()
        };

        if pre_cache {
            organ.sample_cache = Some(HashMap::new());
            organ.metadata_cache = Some(HashMap::new());
        }

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
                    .replace("__HASH__", "#") // Replace placeholder back
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
                        
                        let mut pitch_tuning_cents: f32 = get_prop(
                            &format!("{}PitchTuning", pipe_key_prefix_upper), 
                            &format!("{}pitchtuning", pipe_key_prefix_lower), 
                            "0.0"
                        ).parse().unwrap_or(0.0);

                        // if original_tuning is enabled, we only apply pitch tuning if it's more than +/- 20 cents
                        if original_tuning {
                            if pitch_tuning_cents.abs() <= 20.0 {
                                pitch_tuning_cents = 0.0;
                            }
                        }

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


