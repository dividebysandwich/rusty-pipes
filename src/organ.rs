use anyhow::{anyhow, Context, Result};
use ini::inistr;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::fs;
use serde::Deserialize;
use quick_xml::de::from_str;
use rayon::prelude::*;

use crate::wav_converter;
use crate::wav_converter::SampleMetadata;

/// Top-level structure for the entire organ definition.
#[derive(Debug, Default)]
pub struct Organ {
    pub name: String,
    pub stops: Vec<Stop>,
    pub ranks: HashMap<String, Rank>, // Keyed by rank ID (e.g., "013")
    pub windchest_groups: HashMap<String, WindchestGroup>, // Keyed by group ID (e.g. "001")
    pub tremulants: HashMap<String, Tremulant>, // Keyed by tremulant ID (e.g. "001")
    pub base_path: PathBuf, // The directory containing the .organ file
    pub cache_path: PathBuf, // The directory for cached converted samples
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
    pub division_id: String, // e.g., "SW"
    pub first_midi_note: u8,
    pub pipe_count: usize,
    pub gain_db: f32,
    pub tracker_delay_ms: u32,
    pub windchest_group_id: Option<String>, // Link to a WindchestGroup
    /// Keyed by MIDI note number (e.g., 36)
    pub pipes: HashMap<u8, Pipe>,
    pub is_percussive: bool,
}

/// Represents a Windchest Group (defines shared tremulants/enclosures).
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct WindchestGroup {
    pub name: String,
    pub id_str: String,
    pub tremulant_ids: Vec<String>, // IDs of tremulants attached to this group
}

/// Represents a Tremulant definitions.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct Tremulant {
    pub name: String,
    pub id_str: String,
    pub period: f32,        // Period in ms
    pub start_rate: f32,    
    pub stop_rate: f32,
    pub amp_mod_depth: f32, // Amplitude modulation depth
    pub switch_ids: Vec<String>, // Switches that activate this tremulant
}

/// Represents a single pipe with its attack and release samples.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Pipe {
    pub attack_sample_path: PathBuf,
    pub gain_db: f32,
    pub pitch_tuning_cents: f32,
    pub releases: Vec<ReleaseSample>,
    pub preloaded_bytes: Option<Arc<Vec<f32>>>,
}

/// Represents a release sample and its trigger condition.
#[derive(Debug)]
pub struct ReleaseSample {
    pub path: PathBuf,
    /// Max key press time in ms. -1 means "default".
    pub max_key_press_time_ms: i64,
    pub preloaded_bytes: Option<Arc<Vec<f32>>>,
}

/// Internal struct to track unique conversion jobs for parallel processing
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct ConversionTask {
    relative_path: PathBuf,
    // We store cents as an integer (x100) to allow hashing/equality checks
    tuning_cents_int: i32, 
    to_16bit: bool,
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

    // --- V4 Specific Lists ---
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
    #[serde(rename = "Division", default)]
    divisions: Vec<XmlDivision>,

    // --- V7 Generic List ---
    // V7 puts everything into generic <o> tags
    #[serde(rename = "o", default)]
    v7_objects: Vec<XmlV7Object>,

    // Catch-all for other V4 tags to prevent errors
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

// --- V7 Generic Object ---
#[derive(Debug, Deserialize, PartialEq)]
struct XmlV7Object {
    // V7 maps attributes to single letters
    a: Option<String>,
    b: Option<String>,
    c: Option<String>,
    d: Option<String>,
    e: Option<String>,
    f: Option<String>,
    g: Option<String>,
    // Add more if needed, but a-g covers most IDs/Names/Links
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlDivision {
    #[serde(rename = "DivisionID", alias = "a")]
    id: String,
    #[serde(rename = "Name", alias = "b", default = "default_string")]
    name: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlGeneral {
    // V4 uses "Name", V7 uses "Identification_Name"
    #[serde(rename = "Name", alias = "Identification_Name", default = "default_string")]
    name: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlStop {
    #[serde(rename = "StopID")]
    id: String,
    #[serde(rename = "Name", default = "default_string")]
    name: String,
    #[serde(rename = "DivisionID", default = "default_string")]
    division_id: String,
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
    #[serde(rename = "DivisionID", default = "default_string")]
    division_id: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlPipe {
    #[serde(rename = "PipeID")]
    id: String,
    #[serde(rename = "RankID")]
    rank_id: String,
    #[serde(rename = "NormalMIDINoteNumber")]
    midi_note: u8,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlLayer {
    #[serde(rename = "LayerID")]
    id: String,
    #[serde(rename = "PipeID")]
    pipe_id: String,
}

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

#[derive(Debug, Deserialize, PartialEq)]
struct XmlAttackSample {
    #[serde(rename = "LayerID")]
    layer_id: String,
    #[serde(rename = "SampleID")]
    sample_id: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlReleaseSample {
    #[serde(rename = "LayerID")]
    layer_id: String,
    #[serde(rename = "MaxKeypressTimeMilliseconds", default = "default_i64")]
    max_key_press_time_ms: i64,
    #[serde(rename = "SampleID")]
    sample_id: String,
}

impl Organ {


    /// Reads a file to a String, falling back to Latin-1 (ISO-8859-1) if UTF-8 fails.
    fn normalize_path_preserve_symlinks(path: &Path) -> Result<PathBuf> {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            // Join with current directory to make absolute, but do NOT call canonicalize()
            Ok(std::env::current_dir()?.join(path))
        }
    }

    /// Reads a file to a String, falling back to Latin-1 (ISO-8859-1) if UTF-8 fails.
    fn read_file_tolerant(path: &Path) -> Result<String> {
        let bytes = fs::read(path)
            .with_context(|| format!("Failed to read file {:?}", path))?;

        match String::from_utf8(bytes) {
            Ok(s) => Ok(s),
            Err(e) => {
                log::warn!("File {:?} is not valid UTF-8. Falling back to Latin-1 decoding.", path);
                // Recover the bytes from the error
                let bytes = e.into_bytes();
                // Manual ISO-8859-1 decoding: bytes map 1:1 to chars
                Ok(bytes.into_iter().map(|b| b as char).collect())
            }
        }
    }

    /// Helper to get the cache directory for a specific organ
    fn get_organ_cache_dir(organ_name: &str) -> Result<PathBuf> {
        let settings_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;

        // Get the parent directory (e.g., .../Application Support/rusty-pipes/)
        let config_dir = settings_path.parent().ok_or_else(|| anyhow::anyhow!("Could not get cache directory"))?;
        // Append "cache/<OrganName>"
        let organ_cache = config_dir.join("cache").join(organ_name);
        if !organ_cache.exists() {
            std::fs::create_dir_all(&organ_cache)?;
        }
        Ok(organ_cache)
    }

    /// Loads and parses an organ file (either .organ or .Organ_Hauptwerk_xml).
    /// This function dispatches to the correct parser based on the file extension.
    pub fn load(
        path: &Path, 
        convert_to_16_bit: bool, 
        pre_cache: bool, 
        original_tuning: bool,
        target_sample_rate: u32,
        progress_tx: Option<mpsc::Sender<(f32, String)>>,
        preload_frames: usize,
    ) -> Result<Self> {
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let loader_tx = progress_tx.clone();
        // Parse the organ definition and build the struct
        let mut organ = if extension == "organ" {
            Self::load_grandorgue(path, convert_to_16_bit, false, original_tuning, target_sample_rate, &loader_tx)?
        } else if extension == "Organ_Hauptwerk_xml" || extension == "xml" {
            Self::load_hauptwerk(path, convert_to_16_bit, false, original_tuning, target_sample_rate, &loader_tx)?
        } else {
            return Err(anyhow!("Unsupported organ file format: {:?}", path));
        };

        if pre_cache {
            log::info!("[Organ] Pre-caching mode enabled. This may take a moment...");
            
            // Initialize the caches
            organ.sample_cache = Some(HashMap::new());
            organ.metadata_cache = Some(HashMap::new());
            
            // Run the parallel loader
            organ.run_parallel_precache(target_sample_rate, progress_tx)?;
        } else {
            organ.preload_attack_samples(target_sample_rate, progress_tx, preload_frames)?;
        }
        Ok(organ)

    }

    fn preload_attack_samples(
        &mut self,
        target_sample_rate: u32,
        progress_tx: Option<mpsc::Sender<(f32, String)>>,
        preload_frames: usize,
    ) -> Result<()> {
        log::info!("[Cache] Pre-loading attack/release transients...");
        
        // Collect all paths that need loading
        let mut paths = HashSet::new();
        for rank in self.ranks.values() {
            for pipe in rank.pipes.values() {
                paths.insert(pipe.attack_sample_path.clone());
                for r in &pipe.releases {
                    paths.insert(r.path.clone());
                }
            }
        }
        let unique_paths: Vec<PathBuf> = paths.into_iter().collect();
        let total = unique_paths.len();
        log::debug!("[Cache] Found {} unique attack/release samples to preload.", total);
        if total == 0 { return Ok(()); }
        
        // Load them in parallel
        let loaded_count = AtomicUsize::new(0);
        
        // Map Path -> Arc<Vec<f32>> (Preloaded Chunk)
        let loaded_chunks: HashMap<PathBuf, Arc<Vec<f32>>> = unique_paths
            .par_iter()
            .filter_map(|path| {
                 // Load just the start using a helper from wav_converter
                 match wav_converter::load_sample_head(path, target_sample_rate, preload_frames) {
                     Ok(data) => {
                         let current = loaded_count.fetch_add(1, Ordering::Relaxed);
                         if let Some(tx) = &progress_tx {
                             if current % 50 == 0 {
                                 let _ = tx.send((current as f32 / total as f32, "Pre-loading transients...".to_string()));
                             }
                         }
                         Some((path.clone(), Arc::new(data)))
                     },
                     Err(e) => {
                         log::warn!("Failed to preload {:?}: {}", path, e);
                         None
                     }
                 }
            })
            .collect();

        // Assign the loaded chunks back to the pipes
        for rank in self.ranks.values_mut() {
            for pipe in rank.pipes.values_mut() {
                if let Some(data) = loaded_chunks.get(&pipe.attack_sample_path) {
                    pipe.preloaded_bytes = Some(data.clone());
                }
                for release in &mut pipe.releases {
                    if let Some(data) = loaded_chunks.get(&release.path) {
                        release.preloaded_bytes = Some(data.clone());
                    }
                }
            }
        }
        
        log::info!("[Cache] Pre-loaded {} attack headers.", loaded_chunks.len());
        Ok(())
    }

    /// Helper to execute a set of unique audio conversion tasks in parallel
    fn process_tasks_parallel(
        base_path: &Path,
        cache_path: &Path,
        tasks: HashSet<ConversionTask>,
        target_sample_rate: u32,
        progress_tx: &Option<mpsc::Sender<(f32, String)>>,
    ) -> Result<()> {
        let task_list: Vec<ConversionTask> = tasks.into_iter().collect();
        let total = task_list.len();
        if total == 0 { return Ok(()); }

        log::info!("Processing {} unique audio samples in parallel...", total);
        let completed = AtomicUsize::new(0);

        task_list.par_iter().for_each(|task| {
            let cents = task.tuning_cents_int as f32 / 100.0;
            
            match wav_converter::process_sample_file(
                &task.relative_path,
                base_path,
                cache_path,
                cents,
                task.to_16bit,
                target_sample_rate
            ) {
                Ok(_) => {},
                Err(e) => {
                    // Log the error but continue
                    log::error!("Failed to process audio file {:?}: {}", task.relative_path, e);
                }
            }

            if let Some(tx) = progress_tx {
                let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
                if current % 5 == 0 || current == total {
                    let progress = current as f32 / total as f32;
                    let _ = tx.send((progress, format!("Processing: {}/{}", current, total)));
                }
            }
        });

        Ok(())
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
    fn run_parallel_precache(
        &mut self, 
        target_sample_rate: u32,
        progress_tx: Option<mpsc::Sender<(f32, String)>>
    ) -> Result<()> {
        
        let paths_to_load: Vec<PathBuf> = self.get_all_unique_sample_paths().into_iter().collect();
        let total_samples = paths_to_load.len();
        if total_samples == 0 {
            log::warn!("[Cache] Pre-cache enabled, but no sample paths were found.");
            return Ok(());
        }

        let loaded_sample_count = AtomicUsize::new(0);
        log::info!("[Cache] Loading {} unique samples...", total_samples);

        let results: Vec<Result<(PathBuf, Arc<Vec<f32>>, Arc<SampleMetadata>)>> = paths_to_load
            .par_iter()
            .map(|path| {
                // This closure runs on a different thread
                let (samples, metadata) = wav_converter::load_sample_as_f32(path, target_sample_rate)
                    .with_context(|| format!("Failed to load sample {:?}", path))?;

                // Report progress atomically
                let count = loaded_sample_count.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some(tx) = &progress_tx {
                    let progress = count as f32 / total_samples as f32;
                    // Only update every few files
                    if count % 10 == 0 || count == total_samples {
                        let _ = tx.send((progress, "Loading into RAM...".to_string()));
                    }
                }
                Ok((path.clone(), Arc::new(samples), Arc::new(metadata)))
            })
            .collect();

        let sample_cache = self.sample_cache.as_mut().unwrap();
        let metadata_cache = self.metadata_cache.as_mut().unwrap();

        for result in results {
            if let Ok((path, samples, metadata)) = result {
                sample_cache.insert(path.clone(), samples);
                metadata_cache.insert(path, metadata);
            }
        }
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
        target_sample_rate: u32,
        progress_tx: &Option<mpsc::Sender<(f32, String)>>,
    ) -> Result<Self> {
        let logical_path = Self::normalize_path_preserve_symlinks(path)?;

        println!("Loading Hauptwerk organ from: {:?}", logical_path);
        if let Some(tx) = progress_tx { let _ = tx.send((0.0, "Parsing XML...".to_string())); }
        
        // Use the logical path to determine the root. 
        // This ensures "../OrganInstallationPackages" works even if "OrganDefinitions" is a symlink.
        let organ_root_path = logical_path.parent().and_then(|p| p.parent())
            .ok_or_else(|| anyhow!("Invalid Hauptwerk file path structure."))?;

        let file_content = Self::read_file_tolerant(&logical_path)?;
        let organ_name = logical_path.file_stem().unwrap_or_default().to_string_lossy().replace(".Organ_Hauptwerk_xml", "");
        let cache_path = Self::get_organ_cache_dir(&organ_name)?;

        let mut organ = Organ {
            base_path: organ_root_path.to_path_buf(),
            cache_path: cache_path.clone(),
            name: organ_name,
            sample_cache: if pre_cache { Some(HashMap::new()) } else { None },
            metadata_cache: if pre_cache { Some(HashMap::new()) } else { None },
            ..Default::default()
        };

        // Deserialize the XML
        log::debug!("Parsing XML file...");
        let xml_data: HauptwerkXml = from_str(&file_content)
            .map_err(|e| anyhow!("Failed to parse Hauptwerk XML: {}", e))?;

        // Extract Objects
        let mut xml_stops = Vec::new();
        let mut xml_ranks = Vec::new();
        let mut xml_pipes = Vec::new();      
        let mut xml_layers = Vec::new();
        let mut xml_attack_samples = Vec::new();
        let mut xml_release_samples = Vec::new();
        let mut xml_stop_ranks = Vec::new();
        let mut xml_generals = Vec::new();
        let mut xml_samples = Vec::new();
        let mut xml_divisions = Vec::new();

        for list in xml_data.object_lists {
            match list.object_type.as_str() {
                "Stop" => {
                    xml_stops.extend(list.stops);
                    // V7 Mapping: a=ID, b=Name, c=DivisionID
                    for o in list.v7_objects {
                        xml_stops.push(XmlStop {
                            id: o.a.unwrap_or_default(),
                            name: o.b.unwrap_or_default(),
                            division_id: o.c.unwrap_or_default(),
                        });
                    }
                },
                "Rank" => {
                    xml_ranks.extend(list.ranks);
                    // V7 Mapping: a=ID, b=Name. DivisionID is often not explicit in V7 rank objects.
                    for o in list.v7_objects {
                        xml_ranks.push(XmlRank {
                            id: o.a.unwrap_or_default(),
                            name: o.b.unwrap_or_default(),
                            division_id: "".to_string(), // Will be inferred or skipped
                        });
                    }
                },
                "StopRank" => {
                    xml_stop_ranks.extend(list.stop_ranks);
                    // V7 Mapping: a=StopID, d=RankID (observed in V7 schemas)
                    for o in list.v7_objects {
                        if let (Some(stop_id), Some(rank_id)) = (o.a, o.d) {
                            xml_stop_ranks.push(XmlStopRank { stop_id, rank_id });
                        }
                    }
                },
                "_General" => {
                    xml_generals.extend(list.general);
                    // _General is usually not minified into <o>, but retains ObjectType="_General" with readable tags.
                    // The XmlGeneral struct handles aliases (Name vs Identification_Name).
                },
                "Pipe_SoundEngine01" => {
                    xml_pipes.extend(list.pipes);
                    // V7 Mapping: a=PipeID, b=RankID, d=MidiNote
                    for o in list.v7_objects {
                        let midi_note = o.d.as_deref().unwrap_or("0").parse::<u8>().unwrap_or(0);
                        xml_pipes.push(XmlPipe {
                            id: o.a.unwrap_or_default(),
                            rank_id: o.b.unwrap_or_default(),
                            midi_note,
                        });
                    }
                },
                "Pipe_SoundEngine01_Layer" => {
                    xml_layers.extend(list.layers);
                    // V7 Mapping: a=LayerID, b=PipeID
                    for o in list.v7_objects {
                        xml_layers.push(XmlLayer {
                            id: o.a.unwrap_or_default(),
                            pipe_id: o.b.unwrap_or_default(),
                        });
                    }
                },
                "Pipe_SoundEngine01_AttackSample" => {
                    xml_attack_samples.extend(list.attack_samples);
                    // V7 Mapping: b=LayerID, c=SampleID (a is usually just ID)
                    for o in list.v7_objects {
                        xml_attack_samples.push(XmlAttackSample {
                            layer_id: o.b.unwrap_or_default(),
                            sample_id: o.c.unwrap_or_default(),
                        });
                    }
                },
                "Pipe_SoundEngine01_ReleaseSample" => {
                    xml_release_samples.extend(list.release_samples);
                    // V7 Mapping: b=LayerID, c=SampleID. Time is harder to pin down in V7 minification, defaulting to -1.
                    for o in list.v7_objects {
                        xml_release_samples.push(XmlReleaseSample {
                            layer_id: o.b.unwrap_or_default(),
                            sample_id: o.c.unwrap_or_default(),
                            max_key_press_time_ms: -1, 
                        });
                    }
                },
                "Sample" => {
                    xml_samples.extend(list.samples);
                    // V7 Mapping: a=SampleID, b=PackageID, c=Path
                    for o in list.v7_objects {
                        xml_samples.push(XmlSample {
                            id: o.a.unwrap_or_default(),
                            installation_package_id: o.b.unwrap_or_default(),
                            path: o.c.unwrap_or_default(),
                            pitch_exact_sample_pitch: None, // Often omitted in V7 minified
                            pitch_normal_midi_note_number: None,
                        });
                    }
                },
                "Division" => {
                    xml_divisions.extend(list.divisions);
                    // V7 Mapping: a=ID, b=Name
                    for o in list.v7_objects {
                        xml_divisions.push(XmlDivision {
                            id: o.a.unwrap_or_default(),
                            name: o.b.unwrap_or_default(),
                        });
                    }
                },
                _ => {} 
            }
        }

        if let Some(general) = xml_generals.first() {
            if !general.name.is_empty() { organ.name = general.name.clone(); }
        }

        // Build Division Map (ID -> Name)
        let mut division_name_map: HashMap<String, String> = HashMap::new();
        for div in xml_divisions {
            division_name_map.insert(div.id, div.name);
        }

        // Helper to abbreviate division names
        let get_division_prefix = |div_id: &str| -> String {
            if let Some(name) = division_name_map.get(div_id) {
                let n = name.to_lowercase();
                if n.contains("pedal") { return "P".to_string(); }
                if n.contains("hauptwerk") || n.contains("great") { return "HW".to_string(); }
                if n.contains("schwell") || n.contains("swell") { return "SW".to_string(); }
                if n.contains("positiv") || n.contains("choir") { return "Pos".to_string(); }
                if n.contains("brust") { return "BW".to_string(); }
                if n.contains("ober") { return "OW".to_string(); }
                if n.contains("solo") { return "So".to_string(); }
                // Fallback: First 3 chars of name
                return name.chars().take(3).collect::<String>();
            }
            "".to_string()
        };

        // Build StopRank map
        let mut stop_to_ranks_map: HashMap<String, Vec<String>> = HashMap::new();
        for sr in xml_stop_ranks {
            stop_to_ranks_map.entry(sr.stop_id).or_default().push(sr.rank_id);
        }

        // Build Ranks map
        let mut ranks_map: HashMap<String, Rank> = HashMap::new();
        for xr in xml_ranks {
            ranks_map.insert(xr.id.clone(), Rank {
                name: xr.name,
                id_str: xr.id,
                division_id: xr.division_id, 
                pipe_count: 0,
                pipes: HashMap::new(),
                first_midi_note: 0,
                gain_db: 0.0,
                tracker_delay_ms: 0,
                windchest_group_id: None, // Hauptwerk parser default
                is_percussive: false,
            });
        }

        // Standard Pipe/Sample Loading
        let pipe_map: HashMap<String, &XmlPipe> = xml_pipes.iter().map(|p| (p.id.clone(), p)).collect();
        let sample_map: HashMap<String, &XmlSample> = xml_samples.iter()
            .filter(|s| !s.path.is_empty())
            .map(|s| (s.id.clone(), s))
            .collect();
        let attack_map: HashMap<String, &XmlAttackSample> = xml_attack_samples.iter()
            .map(|a| (a.layer_id.clone(), a))
            .collect();
        let mut release_map: HashMap<String, Vec<&XmlReleaseSample>> = HashMap::new();
        for rel in &xml_release_samples { release_map.entry(rel.layer_id.clone()).or_default().push(rel); }

        let mut conversion_tasks: HashSet<ConversionTask> = HashSet::new();
        let mut seen_pipes: HashSet<(String, u8)> = HashSet::new();

        for layer in &xml_layers {
            let Some(pipe_info) = pipe_map.get(&layer.pipe_id) else { continue; };
            if !ranks_map.contains_key(&pipe_info.rank_id) { continue; }
            
            // This is a duplicate (e.g., tremulant layer or secondary perspective)
            // we skip it to strictly enforce "one pipe per note per rank"
            if seen_pipes.contains(&(pipe_info.rank_id.clone(), pipe_info.midi_note)) {
                continue;
            }
            seen_pipes.insert((pipe_info.rank_id.clone(), pipe_info.midi_note));

            let Some(attack_link) = attack_map.get(&layer.id) else { continue; };
            let Some(attack_sample_info) = sample_map.get(&attack_link.sample_id) else { continue; };
            
            let target_midi_note = pipe_info.midi_note as f32;
            let original_midi_note = if let Some(pitch_hz) = attack_sample_info.pitch_exact_sample_pitch {
                 if pitch_hz > 0.0 { 12.0 * (pitch_hz / 440.0).log2() + 69.0 } else { target_midi_note }
            } else if let Some(midi_note) = attack_sample_info.pitch_normal_midi_note_number {
                midi_note as f32
            } else {
                 Self::try_infer_midi_note_from_filename(&attack_sample_info.path).unwrap_or(target_midi_note)
            };
            let tuning = (target_midi_note - original_midi_note) * 100.0;
            
            let path_str = format!("OrganInstallationPackages/{:0>6}/{}", attack_sample_info.installation_package_id, attack_sample_info.path.replace('\\', "/"));
            conversion_tasks.insert(ConversionTask {
                relative_path: PathBuf::from(path_str),
                tuning_cents_int: (tuning * 100.0) as i32,
                to_16bit: convert_to_16_bit,
            });

            if let Some(xml_release_links) = release_map.get(&layer.id) {
                for release_link in xml_release_links {
                      if let Some(rs) = sample_map.get(&release_link.sample_id) {
                         let path_str = format!("OrganInstallationPackages/{:0>6}/{}", rs.installation_package_id, rs.path.replace('\\', "/"));
                         conversion_tasks.insert(ConversionTask {
                             relative_path: PathBuf::from(path_str),
                             tuning_cents_int: (tuning * 100.0) as i32,
                             to_16bit: convert_to_16_bit,
                         });
                      }
                }
            }
        }

        Self::process_tasks_parallel(&organ.base_path, &organ.cache_path, conversion_tasks, target_sample_rate, progress_tx)?;

        if let Some(tx) = progress_tx { let _ = tx.send((1.0, "Assembling organ...".to_string())); }

        // Pipe Assembly
        for layer in &xml_layers {
            
            let Some(pipe_info) = pipe_map.get(&layer.pipe_id) else {
                log::warn!("Layer {} references non-existent PipeID {}", layer.id, layer.pipe_id);
                continue;
            };

            // Find the rank this pipe belongs to
            let Some(rank) = ranks_map.get_mut(&pipe_info.rank_id) else {
                log::debug!("Pipe {} references non-existent RankID {}", pipe_info.id, pipe_info.rank_id);
                continue;
            };

            if rank.pipes.contains_key(&pipe_info.midi_note) {
                // We already loaded the primary layer (e.g. "Direct") for this note.
                // Skip secondary layers (e.g. "Diffuse", "Rear", "Tremulant").
                continue;
            }

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
            let attack_sample_path_relative = PathBuf::from(&attack_path_str);
            
            log::debug!("Processing LayerID {}: midi note {}, Attack sample path '{}'", layer.id, target_midi_note, attack_sample_path_relative.display());
            
            let final_attack_path = match wav_converter::process_sample_file(
                &attack_sample_path_relative,
                &organ.base_path,
                &organ.cache_path,
                final_pitch_tuning_cents,
                convert_to_16_bit,
                target_sample_rate,
            ) {
                Ok(path) => path,
                Err(e) => {
                    log::warn!("Skipping Pipe (LayerID {}) due to sample error: {:?} - {}", layer.id, attack_sample_path_relative, e);
                    continue;
                }
            };

            if final_pitch_tuning_cents != 0.0 {
                log::debug!("Pipe (LayerID {}) attack sample '{}' retuned by {:.2} cents (Target MIDI: {}, Original MIDI: {}). File: {}",
                    layer.id, attack_sample_info.path, final_pitch_tuning_cents, target_midi_note, original_midi_note, attack_sample_path_relative.display());
            }

            // Process Release Samples (needs double-lookup too)
            let mut releases = Vec::new();
            if let Some(xml_release_links) = release_map.get(&layer.id) {
                for release_link in xml_release_links {
                    if let Some(release_sample_info) = sample_map.get(&release_link.sample_id) {
                        
                        // Construct the full relative path for the release
                        let rel_path_str = format!("OrganInstallationPackages/{:0>6}/{}", release_sample_info.installation_package_id, release_sample_info.path.replace('\\', "/"));
                        let rel_path_buf = PathBuf::from(&rel_path_str);

                        // If the release points to the exact same file as the attack, 
                        // we must try to extract the release tail using CUE markers.
                        let is_self_reference = release_sample_info.path == attack_sample_info.path 
                                             && release_sample_info.installation_package_id == attack_sample_info.installation_package_id;

                        if is_self_reference {
                            // Try to slice the file at the CUE marker
                            if let Ok(Some(extracted_path)) = wav_converter::try_extract_release_sample(
                                &rel_path_buf,
                                &organ.base_path,
                                &organ.cache_path,
                                final_pitch_tuning_cents,
                                convert_to_16_bit,
                                target_sample_rate
                            ) {
                                releases.push(ReleaseSample {
                                    path: extracted_path,
                                    max_key_press_time_ms: release_link.max_key_press_time_ms,
                                    preloaded_bytes: None,
                                });
                            } else {
                                log::warn!("Layer {}: Self-referencing release has no valid CUE/Loop. Skipping to avoid double-attack.", layer.id);
                            }
                        } else {
                            // Standard Processing: It's a separate file
                            match wav_converter::process_sample_file(
                                &rel_path_buf,
                                &organ.base_path,
                                &organ.cache_path,
                                final_pitch_tuning_cents,
                                convert_to_16_bit,
                                target_sample_rate,
                            ) {
                                Ok(final_rel_path) => {
                                    releases.push(ReleaseSample {
                                        path: final_rel_path,
                                        max_key_press_time_ms: release_link.max_key_press_time_ms,
                                        preloaded_bytes: None,
                                    });
                                },
                                Err(e) => {
                                    log::warn!("Skipping release sample for LayerID {} due to error: {} - {}", layer.id, rel_path_str, e);
                                }
                            }
                        }
                    }
                }
            }
            releases.sort_by_key(|r| if r.max_key_press_time_ms == -1 { i64::MAX } else { r.max_key_press_time_ms });

            // If no releases were found in XML, try to extract from the attack sample's CUE markers
            if releases.is_empty() {
                log::info!("Hauptwerk: No release samples defined for Pipe {:?}. Checking for embedded releases...", attack_sample_path_relative);
                // We reuse the relative path and tuning from the attack sample logic
                if let Ok(Some(extracted_path)) = wav_converter::try_extract_release_sample(
                    &attack_sample_path_relative,
                    &organ.base_path,
                    &organ.cache_path,
                    final_pitch_tuning_cents,
                    convert_to_16_bit,
                    target_sample_rate
                ) {
                    log::info!("Found embedded release sample for Pipe ID {}", pipe_info.id);
                    releases.push(ReleaseSample {
                        path: extracted_path,
                        max_key_press_time_ms: -1, // Default release
                        preloaded_bytes: None,
                    });
                }
            }

            rank.pipes.insert(pipe_info.midi_note, Pipe {
                attack_sample_path: final_attack_path,
                gain_db: 0.0,
                pitch_tuning_cents: 0.0,
                releases,
                preloaded_bytes: None,
            });
        }

        for rank in ranks_map.values_mut() {
            rank.pipe_count = rank.pipes.len();
            if let Some(first_key) = rank.pipes.keys().next() {
                rank.first_midi_note = *first_key;
            }
        }

        // Stop linking and naming
        let mut stops_filtered = 0;
        let mut stops_map: HashMap<String, Stop> = HashMap::new();

        let tokenize = |s: &str| -> (Vec<String>, Vec<String>) {
            let mut words = Vec::new();
            let mut pitches = Vec::new();
            for part in s.split(|c: char| !c.is_alphanumeric() && c != '/' && c != '.') {
                let clean = part.trim().to_lowercase();
                if clean.is_empty() { continue; }
                let is_pitch = clean.chars().any(|c| c.is_digit(10)) && 
                               (clean.contains('\'') || clean.len() < 5 || clean.contains('/'));
                if is_pitch { pitches.push(clean); } else { words.push(clean); }
            }
            (words, pitches)
        };

        let parse_id = |id: &str| -> i32 {
            id.chars().filter(|c| c.is_digit(10)).collect::<String>().parse().unwrap_or(999999)
        };

        for xs in xml_stops {
            if xs.name.contains("Key action") || xs.name.contains("noise") || xs.name.is_empty() {
                stops_filtered += 1;
                continue;
            }

            // Explicit
            let mut rank_ids = stop_to_ranks_map.get(&xs.id).cloned().unwrap_or_default();
            let mut linkage_method = "Explicit".to_string();

            // ID Match
            if rank_ids.is_empty() && ranks_map.contains_key(&xs.id) {
                rank_ids.push(xs.id.clone());
                linkage_method = "ID Match".to_string();
            }

            let has_pipes = rank_ids.iter().any(|rid| {
                ranks_map.get(rid).map(|r| !r.pipes.is_empty()).unwrap_or(false)
            });

            // Smart Name Scoring
            if rank_ids.is_empty() || !has_pipes {
                let (stop_words, stop_pitches) = tokenize(&xs.name);
                let stop_id_num = parse_id(&xs.id);

                let mut best_score = 0;
                let mut best_id_match = String::new();
                let mut min_distance = i32::MAX;

                for rank in ranks_map.values() {
                    if rank.pipes.is_empty() { continue; }

                    // Division Check
                    if !xs.division_id.is_empty() && !rank.division_id.is_empty() {
                        if xs.division_id != rank.division_id { continue; }
                    }

                    let (rank_words, rank_pitches) = tokenize(&rank.name);

                    // Exact Pitch Match
                    let pitch_mismatch = stop_pitches.iter().any(|sp| !rank_pitches.contains(sp));
                    if !stop_pitches.is_empty() && pitch_mismatch { continue; }

                    let mut score = 0;
                    
                    if !xs.division_id.is_empty() && xs.division_id == rank.division_id {
                        score += 50; 
                    }

                    for sw in &stop_words {
                        if rank_words.contains(sw) { 
                            score += 2; 
                            if sw.len() <= 2 { score += 10; } 
                        }
                    }

                    if xs.name.to_lowercase() == rank.name.to_lowercase() { score += 20; }
                    if rank.name.contains(&xs.id) { score += 5; }

                    if score > 0 {
                        let rank_id_num = parse_id(&rank.id_str);
                        let distance = (stop_id_num - rank_id_num).abs();

                        if score > best_score {
                            best_score = score;
                            best_id_match = rank.id_str.clone();
                            min_distance = distance;
                        } else if score == best_score {
                            if distance < min_distance {
                                best_id_match = rank.id_str.clone();
                                min_distance = distance;
                            }
                        }
                    }
                }

                if !best_id_match.is_empty() {
                    rank_ids = vec![best_id_match];
                    linkage_method = format!("Smart Score (Best: {})", best_score);
                }
            }

            if rank_ids.len() > 1 {
                // We must pick the "Best" rank to keep.
                // Heuristic: Prefer "Front", "Direct", "Main". Avoid "Rear", "Diffuse".
                
                rank_ids.sort_by(|a_id, b_id| {
                    let get_score = |id: &str| -> i32 {
                        let Some(r) = ranks_map.get(id) else { return -9999; };
                        let n = r.name.to_lowercase();
                        let mut score = 0;
                        
                        // Priority 1: Perspectives
                        if n.contains("front") || n.contains("direct") || n.contains("main") || n.contains("dry") { score += 100; }
                        if n.contains("rear") || n.contains("diffuse") || n.contains("surround") || n.contains("wet") { score -= 100; }
                        
                        // Priority 2: Avoid "Tremulant" ranks if a non-trem version exists (usually preferred for the base stop)
                        if n.contains("trem") { score -= 20; }
                        
                        score
                    };

                    let score_a = get_score(a_id);
                    let score_b = get_score(b_id);
                    
                    // Sort Descending (Highest score first)
                    // If scores are equal, we rely on the original XML order (stable sort implied or insignificant)
                    score_b.cmp(&score_a)
                });

                // Take the winner (index 0 after sort)
                if let Some(winner) = rank_ids.first().cloned() {
                    log::info!("Stop '{}': Forced to single rank. Selected ID {} (from {} candidates).", xs.name, winner, rank_ids.len());
                    rank_ids = vec![winner];
                }
            }
            
            let final_has_pipes = rank_ids.iter().any(|rid| {
                ranks_map.get(rid).map(|r| !r.pipes.is_empty()).unwrap_or(false)
            });

            // Prefix Logic
            let prefix = get_division_prefix(&xs.division_id);
            let mut final_name = xs.name.clone();
            
            // Only prepend if the name doesn't already start with the prefix (case-insensitive check)
            if !prefix.is_empty() {
                // Remove existing "SW", "P", etc. if they are part of the name to ensure uniform formatting? 
                // Or just check if it starts with it.
                let name_lower = final_name.to_lowercase();
                let prefix_lower = prefix.to_lowercase();
                
                // If name is "Octave 4" and prefix is "P", make it "P Octave 4"
                // If name is "P Octave 4" and prefix is "P", leave it alone.
                if !name_lower.starts_with(&prefix_lower) {
                    final_name = format!("{} {}", prefix, final_name);
                }
            }

            if !final_has_pipes {
                 log::warn!("-> WARNING: Stop '{}' (ID: {}, Div: {}) is Silent. (Method: {})", final_name, xs.id, xs.division_id, linkage_method);
            } else {
                 log::info!("-> SUCCESS: Stop '{}' (Div: {}) linked via [{}] to {} rank(s).", final_name, xs.division_id, linkage_method, rank_ids.len());
            }

            stops_map.insert(xs.id.clone(), Stop {
                name: final_name, // Use the prefixed name
                id_str: xs.id,
                rank_ids,
            });
        }
        log::info!("--- Filtered {} stops ---", stops_filtered);

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
        target_sample_rate: u32,
        progress_tx: &Option<mpsc::Sender<(f32, String)>>,
    ) -> Result<Self> {
        let logical_path = Self::normalize_path_preserve_symlinks(path)?;
        println!("Loading GrandOrgue organ from: {:?}", logical_path);
        if let Some(tx) = progress_tx { let _ = tx.send((0.0, "Parsing GrandOrgue INI...".to_string())); }

        let base_path = logical_path.parent().ok_or_else(|| anyhow!("Invalid file path"))?;
        
        let file_content = Self::read_file_tolerant(&logical_path)?;
        
        let safe_content = file_content.replace('#', "__HASH__");
        let conf = inistr!(&safe_content);

        let organ_name = logical_path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let cache_path = Self::get_organ_cache_dir(&organ_name)?;

        let mut organ = Organ {
            base_path: base_path.to_path_buf(),
            cache_path: cache_path.clone(),
            name: organ_name,
            sample_cache: if pre_cache { Some(HashMap::new()) } else { None },
            metadata_cache: if pre_cache { Some(HashMap::new()) } else { None },
            ..Default::default()
        };

        // --- Collect Tasks Logic ---
        let mut conversion_tasks: HashSet<ConversionTask> = HashSet::new();

        for (section_name, props) in conf.iter() {
            let section_lower = section_name.to_lowercase();
            
            // Compatibility Fix: Check for [Rank...] OR [Stop...] sections that act as ranks
            let is_rank_def = section_lower.starts_with("rank");
            let is_stop_def = section_lower.starts_with("stop");
            
            // If it's a stop, verify it actually has pipes before treating it as a rank source
            let has_pipes = if is_stop_def {
                props.get("Pipe001").or_else(|| props.get("pipe001")).is_some()
            } else {
                false
            };

            if !is_rank_def && !has_pipes { continue; }

            let get_prop = |key_upper: &str, key_lower: &str, default: &str| {
                props.get(key_upper).or_else(|| props.get(key_lower)).and_then(|opt| opt.as_deref()).map(|s| s.to_string()).unwrap_or_else(|| default.to_string()).trim().replace("__HASH__", "#").to_string()
            };
            
            let pipe_count: usize = get_prop("NumberOfLogicalPipes", "numberoflogicalpipes", "0").parse().unwrap_or(0);
            
            for i in 1..=pipe_count {
                let pipe_key_prefix_upper = format!("Pipe{:03}", i);
                let pipe_key_prefix_lower = format!("pipe{:03}", i);

                if let Some(attack_path_str) = get_prop(&pipe_key_prefix_upper, &pipe_key_prefix_lower, "").non_empty_or(None) {
                    
                    // Ignore "REF:" entries (GrandOrgue aliases)
                    if attack_path_str.starts_with("REF:") { continue; }

                    let mut pitch_tuning_cents: f32 = get_prop(&format!("{}PitchTuning", pipe_key_prefix_upper), &format!("{}pitchtuning", pipe_key_prefix_lower), "0.0").parse().unwrap_or(0.0);

                    if !attack_path_str.contains("BlankLoop") {
                        let attack_path_str = attack_path_str.replace('\\', "/");
                        if original_tuning && pitch_tuning_cents.abs() <= 20.0 { pitch_tuning_cents = 0.0; }

                        conversion_tasks.insert(ConversionTask {
                            relative_path: PathBuf::from(&attack_path_str),
                            tuning_cents_int: (pitch_tuning_cents * 100.0) as i32,
                            to_16bit: convert_to_16_bit,
                        });
                    }

                    let release_count: usize = get_prop(&format!("{}ReleaseCount", pipe_key_prefix_upper), &format!("{}releasecount", pipe_key_prefix_lower), "0").parse().unwrap_or(0);
                    for r_idx in 1..=release_count {
                        let rel_key_upper = format!("{}Release{:03}", pipe_key_prefix_upper, r_idx);
                        let rel_key_lower = format!("{}release{:03}", pipe_key_prefix_lower, r_idx);
                        if let Some(rel_path_str) = get_prop(&rel_key_upper, &rel_key_lower, "").non_empty_or(None) {
                             // Ignore "REF:"
                             if rel_path_str.starts_with("REF:") { continue; }

                             conversion_tasks.insert(ConversionTask {
                                relative_path: PathBuf::from(rel_path_str.replace('\\', "/")),
                                tuning_cents_int: (pitch_tuning_cents * 100.0) as i32,
                                to_16bit: convert_to_16_bit,
                            });
                        }
                    }
                }
            }
        }

        // --- Parallel Execution ---
        Self::process_tasks_parallel(&organ.base_path, &organ.cache_path, conversion_tasks, target_sample_rate, progress_tx)?;

        // --- Assembly ---
        if let Some(tx) = progress_tx { let _ = tx.send((1.0, "Assembling organ...".to_string())); }

        let mut stops_map: HashMap<String, Stop> = HashMap::new();
        let mut ranks_map: HashMap<String, Rank> = HashMap::new();
        let mut windchest_groups_map: HashMap<String, WindchestGroup> = HashMap::new();
        let mut tremulants_map: HashMap<String, Tremulant> = HashMap::new();

        // --- Build Tremulants ---
        for (section_name, props) in conf.iter() {
             let section_lower = section_name.to_lowercase();
             if section_lower.starts_with("tremulant") {
                 let get_prop = |key_upper: &str, key_lower: &str, default: &str| {
                    props.get(key_upper).or_else(|| props.get(key_lower)).and_then(|opt| opt.as_deref()).map(|s| s.to_string()).unwrap_or_else(|| default.to_string()).trim().replace("__HASH__", "#").to_string()
                };

                 let id_str = section_name.trim_start_matches("tremulant").trim_start_matches("Tremulant").to_string();
                 let name = get_prop("Name", "name", "");
                 let period: f32 = get_prop("Period", "period", "250").parse().unwrap_or(250.0);
                 let start_rate: f32 = get_prop("StartRate", "startrate", "0").parse().unwrap_or(0.0);
                 let stop_rate: f32 = get_prop("StopRate", "stoprate", "0").parse().unwrap_or(0.0);
                 let amp_mod_depth: f32 = get_prop("AmpModDepth", "ampmoddepth", "0").parse().unwrap_or(0.0);
                 
                 let switch_count: usize = get_prop("SwitchCount", "switchcount", "0").parse().unwrap_or(0);
                 let mut switch_ids = Vec::new();
                 for i in 1..=switch_count {
                     if let Some(sw_id) = get_prop(&format!("Switch{:03}", i), &format!("switch{:03}", i), "").non_empty_or(None) {
                        switch_ids.push(sw_id);
                     }
                 }
                 log::info!("Loaded Tremulant '{}' (ID: {}) with {} switches.", name, id_str, switch_ids.len());

                 tremulants_map.insert(id_str.clone(), Tremulant {
                     id_str,
                     name,
                     period,
                     start_rate,
                     stop_rate,
                     amp_mod_depth,
                     switch_ids,
                 });
             }
        }

        // --- Build Windchest Groups ---
        for (section_name, props) in conf.iter() {
            let section_lower = section_name.to_lowercase();
            if section_lower.starts_with("windchestgroup") {
                let get_prop = |key_upper: &str, key_lower: &str, default: &str| {
                    props.get(key_upper).or_else(|| props.get(key_lower)).and_then(|opt| opt.as_deref()).map(|s| s.to_string()).unwrap_or_else(|| default.to_string()).trim().replace("__HASH__", "#").to_string()
                };
                
                let id_str = section_name.trim_start_matches("windchestgroup").trim_start_matches("WindchestGroup").to_string();
                let name = get_prop("Name", "name", "");
                 
                let tremulant_count: usize = get_prop("NumberOfTremulants", "numberoftremulants", "0").parse().unwrap_or(0);
                let mut tremulant_ids = Vec::new();
                for i in 1..=tremulant_count {
                    // Usually formatted as Tremulant001=001
                    if let Some(trem_id) = get_prop(&format!("Tremulant{:03}", i), &format!("tremulant{:03}", i), "").non_empty_or(None) {
                        tremulant_ids.push(trem_id);
                    }
                }

                log::info!("Loaded Windchest Group '{}' (ID: {}) with {} tremulants.", name, id_str, tremulant_ids.len());

                windchest_groups_map.insert(id_str.clone(), WindchestGroup {
                    id_str,
                    name,
                    tremulant_ids,
                });
            }
        }

        // --- Build Ranks ---
        // We look for sections that act as Ranks (either [Rank...] or [Stop...] with pipes)
        for (section_name, props) in conf.iter() {
            let section_lower = section_name.to_lowercase();
            
            // Determine if this section defines a rank
            let is_explicit_rank = section_lower.starts_with("rank");
            let is_stop_as_rank = section_lower.starts_with("stop") && 
                                 (props.get("Pipe001").is_some() || props.get("pipe001").is_some());

            if !is_explicit_rank && !is_stop_as_rank { continue; }

            let get_prop = |key_upper: &str, key_lower: &str, default: &str| {
                props.get(key_upper).or_else(|| props.get(key_lower)).and_then(|opt| opt.as_deref()).map(|s| s.to_string()).unwrap_or_else(|| default.to_string()).trim().replace("__HASH__", "#").to_string()
            };

            let is_percussive = get_prop("Percussive", "percussive", "N").eq_ignore_ascii_case("Y");

            // Extract ID: "Rank001" -> "001", "Stop001" -> "001"
            let id_str = if is_explicit_rank {
                section_name.trim_start_matches("rank").trim_start_matches("Rank").to_string()
            } else {
                section_name.trim_start_matches("stop").trim_start_matches("Stop").to_string()
            };

            let name = get_prop("Name", "name", "");
            let first_midi_note: u8 = get_prop("FirstAccessiblePipeLogicalKeyNumber", "firstaccessiblepipelogicalkeynumber", "1").parse().unwrap_or(1) 
                .max(1) - 1 + 36; // Very rough approximation for GO mapping if MIDI numbers aren't explicit

            let pipe_count: usize = get_prop("NumberOfLogicalPipes", "numberoflogicalpipes", "0").parse().unwrap_or(0);
            let gain_db: f32 = get_prop("AmplitudeLevel", "amplitudelevel", "100.0").parse::<f32>().unwrap_or(100.0);
            // Convert GO "AmplitudeLevel" (usually %) to dB? Or just treat as gain factor. 
            // Rough approx: 100 = 0dB. Log scale. For now, let's normalize 100 -> 0.0dB.
            let gain_db = if gain_db > 0.0 { 20.0 * (gain_db / 100.0).log10() } else { -96.0 };
            
            let windchest_group_id = get_prop("WindchestGroup", "windchestgroup", "").non_empty_or(None);

            let tracker_delay_ms: u32 = 0; 
            let mut pipes = HashMap::new();

            for i in 1..=pipe_count {
                let pipe_key_prefix_upper = format!("Pipe{:03}", i);
                let pipe_key_prefix_lower = format!("pipe{:03}", i);
                let midi_note = first_midi_note + (i as u8 - 1);

                if let Some(attack_path_str) = get_prop(&pipe_key_prefix_upper, &pipe_key_prefix_lower, "").non_empty_or(None) {
                    
                    // Compatibility: Skip REF pipes in assembly for now
                    if attack_path_str.starts_with("REF:") { continue; }

                    let attack_path_str = attack_path_str.replace('\\', "/");
                    let attack_sample_path_relative = PathBuf::from(&attack_path_str);
                    let mut pitch_tuning_cents: f32 = get_prop(&format!("{}PitchTuning", pipe_key_prefix_upper), &format!("{}pitchtuning", pipe_key_prefix_lower), "0.0").parse().unwrap_or(0.0);
                    if original_tuning && pitch_tuning_cents.abs() <= 20.0 { pitch_tuning_cents = 0.0; }

                    // Process Attack (This will find it in cache instantly)
                    let final_attack_path = match wav_converter::process_sample_file(
                        &attack_sample_path_relative,
                        &organ.base_path,
                        &organ.cache_path,
                        pitch_tuning_cents,
                        convert_to_16_bit,
                        target_sample_rate
                    ) {
                        Ok(path) => path,
                        Err(e) => {
                            log::warn!("GrandOrgue: Skipping Pipe {:?} due to sample error: {}", attack_sample_path_relative, e);
                            continue;
                        }
                    };

                    let release_count: usize = get_prop(&format!("{}ReleaseCount", pipe_key_prefix_upper), &format!("{}releasecount", pipe_key_prefix_lower), "0").parse().unwrap_or(0);
                    let mut releases = Vec::new();
                    for r_idx in 1..=release_count {
                        let rel_key_upper = format!("{}Release{:03}", pipe_key_prefix_upper, r_idx);
                        let rel_key_lower = format!("{}release{:03}", pipe_key_prefix_lower, r_idx);
                        
                        if let Some(rel_path_str) = get_prop(&rel_key_upper, &rel_key_lower, "").non_empty_or(None) {
                            if rel_path_str.starts_with("REF:") { continue; }
                            
                            let rel_path_clean = rel_path_str.replace('\\', "/");
                            let rel_path_buf = PathBuf::from(&rel_path_clean);

                            // Check if release sample points to the same file as the attack sample
                            // Compare the raw strings from the INI (normalized slashes)
                            let is_self_reference = rel_path_clean == attack_path_str;

                            if is_self_reference {
                                if let Ok(Some(extracted_path)) = wav_converter::try_extract_release_sample(
                                    &rel_path_buf,
                                    &organ.base_path,
                                    &organ.cache_path,
                                    pitch_tuning_cents,
                                    convert_to_16_bit,
                                    target_sample_rate
                                ) {
                                    let max_time: i64 = get_prop(&format!("{}MaxKeyPressTime", rel_key_upper), &format!("{}maxkeypresstime", rel_key_lower), "-1").parse().unwrap_or(-1);
                                    releases.push(ReleaseSample { 
                                        path: extracted_path, 
                                        max_key_press_time_ms: max_time,
                                        preloaded_bytes: None,
                                     });
                                }
                            } else {
                                // Standard Processing
                                match wav_converter::process_sample_file(
                                    &rel_path_buf,
                                    &organ.base_path,
                                    &organ.cache_path,
                                    pitch_tuning_cents,
                                    convert_to_16_bit,
                                    target_sample_rate
                                ) {
                                    Ok(final_rel_path) => {
                                        let max_time: i64 = get_prop(&format!("{}MaxKeyPressTime", rel_key_upper), &format!("{}maxkeypresstime", rel_key_lower), "-1").parse().unwrap_or(-1);
                                        releases.push(ReleaseSample { 
                                            path: final_rel_path, 
                                            max_key_press_time_ms: max_time,
                                            preloaded_bytes: None,
                                         });
                                    },
                                    Err(e) => {
                                        log::warn!("GrandOrgue: Skipping release sample {:?} due to error: {}", rel_path_buf, e);
                                    }
                                 }
                            }
                        }
                    }

                    releases.sort_by_key(|r| if r.max_key_press_time_ms == -1 { i64::MAX } else { r.max_key_press_time_ms });

                    // If no releases were found in INI, try to extract from the attack sample's CUE markers
                    if releases.is_empty() {
                        log::info!("GrandOrgue: No release samples defined for Pipe {:?}. Checking for embedded releases...", attack_sample_path_relative);
                        if let Ok(Some(extracted_path)) = wav_converter::try_extract_release_sample(
                            &attack_sample_path_relative,
                            &organ.base_path,
                            &organ.cache_path,
                            pitch_tuning_cents,
                            convert_to_16_bit,
                            target_sample_rate
                        ) {
                            log::info!("Found embedded release sample for Pipe MIDI Note {}", midi_note);
                            releases.push(ReleaseSample {
                                path: extracted_path,
                                max_key_press_time_ms: -1,
                                preloaded_bytes: None,
                            });
                        }
                    }

                    pipes.insert(midi_note, 
                        Pipe { 
                            attack_sample_path: 
                            final_attack_path, 
                            gain_db: 0.0, 
                            pitch_tuning_cents: 
                            0.0, 
                            releases,
                            preloaded_bytes: None,
                        }
                    );
                }
            }
            let division_id = String::new(); // GrandOrgue does not have DivisionIDs
            ranks_map.insert(id_str.clone(), Rank { 
                name, 
                id_str, 
                division_id, 
                first_midi_note, 
                pipe_count, 
                gain_db, 
                tracker_delay_ms, 
                windchest_group_id,
                pipes,
                is_percussive,
            });
        }

        log::info!("Scanning for Key Action noise pairs to merge...");

        // Identify pairs. Map key is the "Base Name" (e.g., "Key action Manual 1").
        // Value is (Option<AttackRankID>, Option<ReleaseRankID>)
        let mut noise_pairs: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();

        for rank in ranks_map.values() {
            if rank.name.contains("Key action") {
                let name_lower = rank.name.to_lowercase();
                // Determine base name by stripping " attack" or " release"
                let base_name = if name_lower.ends_with(" attack") {
                    rank.name[..rank.name.len() - 7].trim().to_string()
                } else if name_lower.ends_with(" release") {
                    rank.name[..rank.name.len() - 8].trim().to_string()
                } else {
                    rank.name.clone()
                };

                let entry = noise_pairs.entry(base_name).or_insert((None, None));
                if name_lower.contains("attack") {
                    entry.0 = Some(rank.id_str.clone());
                } else if name_lower.contains("release") {
                    entry.1 = Some(rank.id_str.clone());
                }
            }
        }

        // Perform Merge
        let mut ranks_to_remove = Vec::new();

        for (base_name, (attack_id_opt, release_id_opt)) in noise_pairs {
            if let (Some(attack_id), Some(release_id)) = (attack_id_opt, release_id_opt) {
                log::info!("Merging Noise Ranks: '{}' <- '{}' (Base: {})", attack_id, release_id, base_name);
        
                // We need to extract the release rank to steal its pipes
                // Note: We cannot borrow ranks_map mutably twice, so we remove the release rank now.
                if let Some(mut release_rank) = ranks_map.remove(&release_id) {
                    if let Some(attack_rank) = ranks_map.get_mut(&attack_id) {
                        // Update the name to the base name (remove " Attack")
                        attack_rank.name = base_name;
                
                        // Merge pipes
                        for (note, release_pipe) in release_rank.pipes.drain() {
                            if let Some(attack_pipe) = attack_rank.pipes.get_mut(&note) {
                                // Move the releases from the release_rank pipe to the attack_rank pipe
                                attack_pipe.releases.extend(release_pipe.releases);
                        
                                // Sort releases by time again to be safe
                                attack_pipe.releases.sort_by_key(|r| if r.max_key_press_time_ms == -1 { i64::MAX } else { r.max_key_press_time_ms });
                            } else {
                                // If attack rank doesn't have this key, but release does, we might want to add it.
                                // However, for key actions, they usually match 1:1.
                                // If we add it, we must ensure the attack_sample_path is valid (not blank) or handled by engine.
                            }
                        }
                    }
                    ranks_to_remove.push(release_id); // Track ID to clean up Stop references later
                }
            }
        }


        // --- Build Stops ---
        for (section_name, props) in conf.iter() {
            let get_prop = |key_upper: &str, key_lower: &str, default: &str| {
                props.get(key_upper).or_else(|| props.get(key_lower)).and_then(|opt| opt.as_deref()).map(|s| s.to_string()).unwrap_or_else(|| default.to_string()).trim().replace("__HASH__", "#").to_string()
            };

            if section_name.to_lowercase().starts_with("stop") {
                let id_str = section_name.trim_start_matches("stop").trim_start_matches("Stop").to_string();
                let mut name = get_prop("Name", "name", "");
                
                if name.contains("noise") || name.is_empty() { continue; }
                
                let rank_count: usize = get_prop("NumberOfRanks", "numberofranks", "0").parse().unwrap_or(0);
                let mut rank_ids = Vec::new();
                for i in 1..=rank_count {
                    if let Some(rank_id) = get_prop(&format!("Rank{:03}", i), &format!("rank{:03}", i), "").non_empty_or(None) {
                        rank_ids.push(rank_id.to_string());
                    }
                }

                // Try to find explicitly linked ranks (Standard GO)
                let rank_count: usize = get_prop("NumberOfRanks", "numberofranks", "0").parse().unwrap_or(0);
                let mut rank_ids = Vec::new();
                for i in 1..=rank_count {
                    if let Some(rank_id) = get_prop(&format!("Rank{:03}", i), &format!("rank{:03}", i), "").non_empty_or(None) {
                        rank_ids.push(rank_id.to_string());
                    }
                }

                // Compatibility Fix: If no explicit ranks, check if this stop matches an auto-generated rank (from Phase 1)
                if rank_ids.is_empty() {
                    if ranks_map.contains_key(&id_str) {
                        rank_ids.push(id_str.clone());
                    }
                }

                if rank_ids.len() == 1 {
                    if let Some(rank) = ranks_map.get(&rank_ids[0]) {
                        // If the Rank is percussive/noise, use the Rank's clean name
                        // (e.g. "Key action Manual 1" instead of "Key action Manual 1 Attack")
                        if rank.is_percussive {
                            name = rank.name.clone();
                        }
                    }
                }
                // Only add stop if it actually triggers something
                if !rank_ids.is_empty() {
                    stops_map.insert(id_str.clone(), Stop { name, id_str, rank_ids });
                }
            } 
        }

        let mut stops: Vec<Stop> = stops_map.into_values().collect();
        stops.sort_by(|a, b| a.id_str.cmp(&b.id_str));
        organ.stops = stops;
        organ.ranks = ranks_map;
        organ.windchest_groups = windchest_groups_map;
        organ.tremulants = tremulants_map;

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