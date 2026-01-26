use anyhow::{anyhow, Context, Result};
use bytemuck::{cast_slice, cast_slice_mut};
use rayon::prelude::*;
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use crate::wav_converter;
use crate::wav_converter::SampleMetadata;

use crate::organ_grandorgue;
use crate::organ_hauptwerk;

/// Top-level structure for the entire organ definition.
#[derive(Debug, Default)]
pub struct Organ {
    pub name: String,
    pub stops: Vec<Stop>,
    pub ranks: HashMap<String, Rank>, // Keyed by rank ID (e.g., "013")
    pub windchest_groups: HashMap<String, WindchestGroup>, // Keyed by group ID (e.g. "001")
    pub tremulants: HashMap<String, Tremulant>, // Keyed by tremulant ID (e.g. "001")
    pub base_path: PathBuf,           // The directory containing the .organ file
    pub cache_path: PathBuf,          // The directory for cached converted samples
    pub sample_cache: Option<HashMap<PathBuf, Arc<Vec<f32>>>>, // Cache for loaded samples
    pub metadata_cache: Option<HashMap<PathBuf, Arc<SampleMetadata>>>, // Cache for loop points etc.
}

/// Represents a single stop (a button on the TUI).
#[derive(Debug, Clone)]
pub struct Stop {
    pub name: String,
    pub id_str: String,        // e.g., "013"
    pub rank_ids: Vec<String>, // IDs of ranks it triggers
}

/// Represents a rank (a set of pipes).
#[allow(dead_code)]
#[derive(Debug)]
pub struct Rank {
    pub name: String,
    pub id_str: String,      // e.g., "013"
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
    pub period: f32, // Period in ms
    pub start_rate: f32,
    pub stop_rate: f32,
    pub amp_mod_depth: f32,      // Amplitude modulation depth
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
    ///
    /// `max_preload_ram_mb`: The maximum amount of RAM (in MB) to dedicate to preloading attack transients.
    pub fn load(
        path: &Path,
        convert_to_16_bit: bool,
        pre_cache: bool,
        original_tuning: bool,
        target_sample_rate: u32,
        progress_tx: Option<mpsc::Sender<(f32, String)>>,
        max_preload_ram_mb: usize,
    ) -> Result<Self> {
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let loader_tx = progress_tx.clone();

        // Dispatch to specific loader modules
        let mut organ = if extension == "organ" {
            organ_grandorgue::load_grandorgue_dir(
                path,
                convert_to_16_bit,
                original_tuning,
                target_sample_rate,
                &loader_tx,
            )?
        } else if extension == "orgue" {
            organ_grandorgue::load_grandorgue_zip(
                path,
                convert_to_16_bit,
                original_tuning,
                target_sample_rate,
                &loader_tx,
            )?
        } else if extension == "Organ_Hauptwerk_xml" || extension == "xml" {
            organ_hauptwerk::load_hauptwerk(
                path,
                convert_to_16_bit,
                false,
                original_tuning,
                target_sample_rate,
                &loader_tx,
            )?
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
            // Dynamically calculate frame count based on RAM budget
            organ.preload_attack_samples(
                target_sample_rate,
                progress_tx,
                max_preload_ram_mb,
                original_tuning,
                convert_to_16_bit,
            )?;
        }
        Ok(organ)
    }

    /// Normalizes a path to an absolute path without resolving symlinks.
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
        let bytes = fs::read(path).with_context(|| format!("Failed to read file {:?}", path))?;

        match String::from_utf8(bytes) {
            Ok(s) => Ok(s),
            Err(e) => {
                log::warn!(
                    "File {:?} is not valid UTF-8. Falling back to Latin-1 decoding.",
                    path
                );
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
        let config_dir = settings_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Could not get cache directory"))?;
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
            Err(_) => None,
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
        if total == 0 {
            return Ok(());
        }

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
                target_sample_rate,
            ) {
                Ok(_) => {}
                Err(e) => {
                    log::error!(
                        "Failed to process audio file {:?}: {}",
                        task.relative_path,
                        e
                    );
                }
            }

            if let Some(tx) = progress_tx {
                let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
                if current % 5 == 0 || current == total {
                    let progress = current as f32 / total as f32;
                    let _ = tx.send((progress, t!("gui.progress_processing").to_string()));
                }
            }
        });

        Ok(())
    }

    /// Helper to get the transient cache directory (~/.config/transientcache/)
    fn get_transient_cache_path(&self) -> Result<PathBuf> {
        let settings_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;
        let config_dir = settings_path
            .parent()
            .ok_or_else(|| anyhow!("Could not determine config directory"))?;

        let cache_dir = config_dir.join("transientcache");
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        // Sanitize organ name for filename
        let safe_name: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();

        Ok(cache_dir.join(format!("{}.bin", safe_name)))
    }

    /// Tries to load the consolidated cache from disk.
    /// Returns None if file doesn't exist, has invalid header, or frame count mismatches.
    fn load_transient_cache(
        &self,
        path: &Path,
        expected_frames: usize,
        expected_original_tuning: bool,
        expected_sample_rate: u32,
        expected_16bit: bool,
        progress_tx: &Option<mpsc::Sender<(f32, String)>>,
    ) -> Option<HashMap<PathBuf, Arc<Vec<f32>>>> {
        let file = fs::File::open(path).ok()?;
        let mut reader = BufReader::with_capacity(1_024 * 1_024, file);

        // Validate Magic Header
        let mut magic = [0u8; 4];
        if reader.read_exact(&mut magic).is_err() || &magic != b"TRNS" {
            log::warn!("[Cache] Cache file corrupted or invalid format.");
            return None;
        }

        // Validate Frame Count
        let mut frames_buf = [0u8; 8];
        reader.read_exact(&mut frames_buf).ok()?;
        let stored_frames = u64::from_le_bytes(frames_buf) as usize;

        if stored_frames != expected_frames {
            log::info!(
                "[Cache] RAM settings changed (old: {}, new: {}). Invalidating cache.",
                stored_frames,
                expected_frames
            );
            return None;
        }

        // Validate Tuning Setting
        let mut bool_buf = [0u8; 1];
        if reader.read_exact(&mut bool_buf).is_err() {
            return None;
        }
        let stored_tuning = bool_buf[0] != 0;

        if stored_tuning != expected_original_tuning {
            log::info!("[Cache] Tuning setting changed. Invalidating.");
            return None;
        }

        // Validate Sample Rate (New)
        let mut sr_buf = [0u8; 4];
        if reader.read_exact(&mut sr_buf).is_err() {
            return None;
        }
        let stored_sr = u32::from_le_bytes(sr_buf);

        if stored_sr != expected_sample_rate {
            log::info!(
                "[Cache] Sample rate changed (old: {}, new: {}). Invalidating.",
                stored_sr,
                expected_sample_rate
            );
            return None;
        }

        // Validate 16-bit Setting (New)
        if reader.read_exact(&mut bool_buf).is_err() {
            return None;
        }
        let stored_16bit = bool_buf[0] != 0;

        if stored_16bit != expected_16bit {
            log::info!("[Cache] 16-bit conversion setting changed. Invalidating.");
            return None;
        }

        // Read Item Count
        let mut count_buf = [0u8; 8];
        reader.read_exact(&mut count_buf).ok()?;
        let total_count = u64::from_le_bytes(count_buf) as usize;

        log::info!(
            "[Cache] Fast-loading {} samples from cache file...",
            total_count
        );

        let mut map = HashMap::with_capacity(total_count);
        let mut path_buffer = Vec::new();

        for i in 0..total_count {
            // Read Path Length
            let mut len_buf = [0u8; 8];
            if reader.read_exact(&mut len_buf).is_err() {
                break;
            }
            let path_len = u64::from_le_bytes(len_buf) as usize;

            // Read Path String
            path_buffer.resize(path_len, 0);
            if reader.read_exact(&mut path_buffer).is_err() {
                break;
            }

            // Use lossy utf8 conversion to avoid crashing on random bytes
            let path_str = String::from_utf8_lossy(&path_buffer).to_string();
            let path = PathBuf::from(path_str);

            // Read Data Length (number of f32s)
            if reader.read_exact(&mut len_buf).is_err() {
                break;
            }
            let data_len = u64::from_le_bytes(len_buf) as usize;

            // Allocate the memory as f32s (ensures correct alignment)
            let mut samples = vec![0.0f32; data_len];

            // Safely cast the f32 slice to a mutable u8 slice
            // bytemuck checks that f32 is "Pod" (Plain Old Data) and safe to write bytes into.
            let byte_slice: &mut [u8] = cast_slice_mut(&mut samples);

            // Read directly from file into the vector's memory
            if reader.read_exact(byte_slice).is_err() {
                break;
            }

            map.insert(path, Arc::new(samples));

            if let Some(tx) = progress_tx {
                if i % 1000 == 0 || i == total_count - 1 {
                    let progress = i as f32 / total_count as f32;
                    let _ = tx.send((progress, t!("gui.progress_cache_read").to_string()));
                }
            }
        }

        if map.len() != total_count {
            log::warn!("[Cache] Truncated cache file. Rebuilding.");
            return None;
        }

        Some(map)
    }

    /// Writes the loaded chunks to a single binary file.
    fn save_transient_cache(
        &self,
        path: &Path,
        data: &HashMap<PathBuf, Arc<Vec<f32>>>,
        frames_per_sample: usize,
        original_tuning: bool,
        sample_rate: u32,
        to_16bit: bool,
        progress_tx: &Option<mpsc::Sender<(f32, String)>>,
    ) -> Result<()> {
        let file = fs::File::create(path)?;
        let mut writer = BufWriter::with_capacity(1_024 * 1_024, file);

        // Write Magic Header
        writer.write_all(b"TRNS")?;
        // Write Frames Per Sample
        writer.write_all(&(frames_per_sample as u64).to_le_bytes())?;

        // Config: Tuning (1 byte)
        writer.write_all(&[if original_tuning { 1u8 } else { 0u8 }])?;

        // Config: Sample Rate (4 bytes)
        writer.write_all(&sample_rate.to_le_bytes())?;

        // Config: 16-bit (1 byte)
        writer.write_all(&[if to_16bit { 1u8 } else { 0u8 }])?;

        // Write Item Count
        let total_count = data.len();
        writer.write_all(&(total_count as u64).to_le_bytes())?;

        let mut i = 0;
        for (path_buf, samples) in data {
            let path_str = path_buf.to_string_lossy();
            let path_bytes = path_str.as_bytes();
            writer.write_all(&(path_bytes.len() as u64).to_le_bytes())?;
            writer.write_all(path_bytes)?;

            writer.write_all(&(samples.len() as u64).to_le_bytes())?;

            // Safely cast the f32 slice to a u8 slice for writing
            let byte_slice: &[u8] = cast_slice(samples);
            writer.write_all(byte_slice)?;

            if let Some(tx) = progress_tx {
                i += 1;
                if i % 1000 == 0 || i == total_count {
                    let progress = i as f32 / total_count as f32;
                    let _ = tx.send((progress, t!("gui.progress_cache_write").to_string()));
                }
            }
        }

        writer.flush()?;
        log::info!(
            "[Cache] Wrote {} samples to transient cache at {:?}",
            total_count,
            path
        );
        Ok(())
    }

    fn preload_attack_samples(
        &mut self,
        target_sample_rate: u32,
        progress_tx: Option<mpsc::Sender<(f32, String)>>,
        max_preload_ram_mb: usize,
        original_tuning: bool,
        convert_to_16bit: bool,
    ) -> Result<()> {
        log::info!(
            "[Cache] Calculating pre-load budget based on {} MB limit...",
            max_preload_ram_mb
        );

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
        let total_files = unique_paths.len();

        if total_files == 0 {
            log::debug!("[Cache] No samples found to preload.");
            return Ok(());
        }

        // Calculate frames per file based on RAM budget
        // Total bytes available
        let total_bytes_budget = max_preload_ram_mb as usize * 1024 * 1024;

        // Bytes available per unique file
        let bytes_per_file = total_bytes_budget / total_files;

        // Size of one f32 sample
        let bytes_per_float = std::mem::size_of::<f32>();

        // Heuristic: Assume Stereo (2 channels) to be safe.
        // If files are mono, we simply load less duration than we could have, but we won't crash RAM.
        // If files are stereo, we hit the target exactly.
        let assumed_channels = 2;
        let bytes_per_frame = bytes_per_float * assumed_channels;

        let frames_to_preload = bytes_per_file / bytes_per_frame;

        // Convert to milliseconds for logging (just for user info)
        let ms_preload = (frames_to_preload as f32 / target_sample_rate as f32) * 1000.0;

        log::info!(
            "[Cache] Found {} unique samples. RAM Budget: {} MB.",
            total_files,
            max_preload_ram_mb
        );
        log::info!(
            "[Cache] Allocation: ~{} bytes/file -> Preloading {} frames (~{:.1} ms) per sample.",
            bytes_per_file,
            frames_to_preload,
            ms_preload
        );

        if frames_to_preload == 0 {
            log::warn!("[Cache] RAM budget is too low to preload any meaningful data per file.");
            return Ok(());
        }

        // Check transient cache first
        let mut loaded_chunks: Option<HashMap<PathBuf, Arc<Vec<f32>>>> = None;
        let cache_path_result = self.get_transient_cache_path();

        if let Ok(cache_path) = &cache_path_result {
            if cache_path.exists() {
                if let Some(cached_data) = self.load_transient_cache(
                    cache_path,
                    frames_to_preload,
                    original_tuning,
                    target_sample_rate,
                    convert_to_16bit,
                    &progress_tx,
                ) {
                    if let Some(tx) = &progress_tx {
                        let _ = tx.send((1.0, t!("gui.progress_cache_done").to_string()));
                    }
                    loaded_chunks = Some(cached_data);
                }
            }
        }

        // Cache miss, load wav files
        let chunks_map = if let Some(map) = loaded_chunks {
            map
        } else {
            // Load them in parallel
            let loaded_count = AtomicUsize::new(0);
            let map: HashMap<PathBuf, Arc<Vec<f32>>> = unique_paths
                .par_iter()
                .filter_map(|path| {
                    // Load just the start using a helper from wav_converter
                    match wav_converter::load_sample_head(
                        path,
                        target_sample_rate,
                        frames_to_preload,
                    ) {
                        Ok(data) => {
                            let current = loaded_count.fetch_add(1, Ordering::Relaxed);
                            if let Some(tx) = &progress_tx {
                                if current % 50 == 0 {
                                    let _ = tx.send((
                                        current as f32 / total_files as f32,
                                        t!("gui.progress_load_transients").to_string(),
                                    ));
                                }
                            }
                            Some((path.clone(), Arc::new(data)))
                        }
                        Err(e) => {
                            log::warn!("Failed to preload {:?}: {}", path, e);
                            None
                        }
                    }
                })
                .collect();

            // Save to cache for next time
            if let Ok(cache_path) = &cache_path_result {
                if let Err(e) = self.save_transient_cache(
                    cache_path,
                    &map,
                    frames_to_preload,
                    original_tuning,
                    target_sample_rate,
                    convert_to_16bit,
                    &progress_tx,
                ) {
                    log::error!("Failed to save transient cache: {}", e);
                }
            }

            map
        };

        // Assign the loaded chunks back to the pipes
        for rank in self.ranks.values_mut() {
            for pipe in rank.pipes.values_mut() {
                if let Some(data) = chunks_map.get(&pipe.attack_sample_path) {
                    pipe.preloaded_bytes = Some(data.clone());
                }
                for release in &mut pipe.releases {
                    if let Some(data) = chunks_map.get(&release.path) {
                        release.preloaded_bytes = Some(data.clone());
                    }
                }
            }
        }

        log::info!(
            "[Cache] Successfully pre-loaded {} attack headers.",
            chunks_map.len()
        );
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
        progress_tx: Option<mpsc::Sender<(f32, String)>>,
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
                let (samples, metadata) =
                    wav_converter::load_sample_as_f32(path, target_sample_rate)
                        .with_context(|| format!("Failed to load sample {:?}", path))?;

                // Report progress atomically
                let count = loaded_sample_count.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some(tx) = &progress_tx {
                    let progress = count as f32 / total_samples as f32;
                    // Only update every few files
                    if count % 10 == 0 || count == total_samples {
                        let _ = tx.send((progress, t!("gui.progress_load_ram").to_string()));
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
