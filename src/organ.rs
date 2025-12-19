use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::fs;
use rayon::prelude::*;

use crate::wav_converter;
use crate::wav_converter::SampleMetadata;

use crate::organ_hauptwerk; 
use crate::organ_grandorgue;

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
pub struct ConversionTask {
    pub relative_path: PathBuf,
    // We store cents as an integer (x100) to allow hashing/equality checks
    pub tuning_cents_int: i32, 
    pub to_16bit: bool,
}

impl Organ {
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
        
        // Dispatch to specific loader modules
        let mut organ = if extension == "organ" {
            organ_grandorgue::load_grandorgue(path, convert_to_16_bit, false, original_tuning, target_sample_rate, &loader_tx)?
        } else if extension == "Organ_Hauptwerk_xml" || extension == "xml" {
            organ_hauptwerk::load_hauptwerk(path, convert_to_16_bit, false, original_tuning, target_sample_rate, &loader_tx)?
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

    /// Reads a file to a String, falling back to Latin-1 (ISO-8859-1) if UTF-8 fails.
    pub fn normalize_path_preserve_symlinks(path: &Path) -> Result<PathBuf> {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            // Join with current directory to make absolute, but do NOT call canonicalize()
            Ok(std::env::current_dir()?.join(path))
        }
    }

    /// Reads a file to a String, falling back to Latin-1 (ISO-8859-1) if UTF-8 fails.
    pub fn read_file_tolerant(path: &Path) -> Result<String> {
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
    pub fn get_organ_cache_dir(organ_name: &str) -> Result<PathBuf> {
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

    pub fn try_infer_midi_note_from_filename(path_str: &str) -> Option<f32> {
        let path = Path::new(path_str);
        let stem = path.file_stem().and_then(|s| s.to_str())?;
        let note_str = stem.split('-').next()?;
        match note_str.parse::<u8>() {
            Ok(midi_note) => Some(midi_note as f32),
            Err(_) => None
        }
    }

    /// Helper to execute a set of unique audio conversion tasks in parallel
    pub fn process_tasks_parallel(
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
}