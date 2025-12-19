use anyhow::{anyhow, Result};
use ini::inistr;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::wav_converter;
use crate::organ::{Organ, Stop, Rank, Pipe, ReleaseSample, Tremulant, WindchestGroup, ConversionTask};

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

/// Loads and parses a GrandOrgue (.organ) file.
pub fn load_grandorgue(
    path: &Path, 
    convert_to_16_bit: bool, 
    pre_cache: bool, 
    original_tuning: bool,
    target_sample_rate: u32,
    progress_tx: &Option<mpsc::Sender<(f32, String)>>,
) -> Result<Organ> {
    let logical_path = Organ::normalize_path_preserve_symlinks(path)?;
    println!("Loading GrandOrgue organ from: {:?}", logical_path);
    if let Some(tx) = progress_tx { let _ = tx.send((0.0, "Parsing GrandOrgue INI...".to_string())); }

    let base_path = logical_path.parent().ok_or_else(|| anyhow!("Invalid file path"))?;
    
    // Access helper from Organ
    let file_content = Organ::read_file_tolerant(&logical_path)?;
    
    let safe_content = file_content.replace('#', "__HASH__");
    let conf = inistr!(&safe_content);

    let organ_name = logical_path.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let cache_path = Organ::get_organ_cache_dir(&organ_name)?;

    let mut organ = Organ {
        base_path: base_path.to_path_buf(),
        cache_path: cache_path.clone(),
        name: organ_name,
        sample_cache: if pre_cache { Some(HashMap::new()) } else { None },
        metadata_cache: if pre_cache { Some(HashMap::new()) } else { None },
        ..Default::default()
    };

    // Collect Tasks Logic
    let mut conversion_tasks: HashSet<ConversionTask> = HashSet::new();

    for (section_name, props) in conf.iter() {
        let section_lower = section_name.to_lowercase();
        
        let is_rank_def = section_lower.starts_with("rank");
        let is_stop_def = section_lower.starts_with("stop");
        
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

    // Parallel Execution
    Organ::process_tasks_parallel(&organ.base_path, &organ.cache_path, conversion_tasks, target_sample_rate, progress_tx)?;

    // Assembly
    if let Some(tx) = progress_tx { let _ = tx.send((1.0, "Assembling organ...".to_string())); }

    let mut stops_map: HashMap<String, Stop> = HashMap::new();
    let mut ranks_map: HashMap<String, Rank> = HashMap::new();
    let mut windchest_groups_map: HashMap<String, WindchestGroup> = HashMap::new();
    let mut tremulants_map: HashMap<String, Tremulant> = HashMap::new();

    // Build Tremulants
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

    // Build Windchest Groups
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

    // Build Ranks
    for (section_name, props) in conf.iter() {
        let section_lower = section_name.to_lowercase();
        
        let is_explicit_rank = section_lower.starts_with("rank");
        let is_stop_as_rank = section_lower.starts_with("stop") && 
                                (props.get("Pipe001").is_some() || props.get("pipe001").is_some());

        if !is_explicit_rank && !is_stop_as_rank { continue; }

        let get_prop = |key_upper: &str, key_lower: &str, default: &str| {
            props.get(key_upper).or_else(|| props.get(key_lower)).and_then(|opt| opt.as_deref()).map(|s| s.to_string()).unwrap_or_else(|| default.to_string()).trim().replace("__HASH__", "#").to_string()
        };

        let is_percussive = get_prop("Percussive", "percussive", "N").eq_ignore_ascii_case("Y");

        let id_str = if is_explicit_rank {
            section_name.trim_start_matches("rank").trim_start_matches("Rank").to_string()
        } else {
            section_name.trim_start_matches("stop").trim_start_matches("Stop").to_string()
        };

        let name = get_prop("Name", "name", "");
        let first_midi_note: u8 = get_prop("FirstAccessiblePipeLogicalKeyNumber", "firstaccessiblepipelogicalkeynumber", "1").parse().unwrap_or(1) 
            .max(1) - 1 + 36; 

        let pipe_count: usize = get_prop("NumberOfLogicalPipes", "numberoflogicalpipes", "0").parse().unwrap_or(0);
        let gain_db: f32 = get_prop("AmplitudeLevel", "amplitudelevel", "100.0").parse::<f32>().unwrap_or(100.0);
        let gain_db = if gain_db > 0.0 { 20.0 * (gain_db / 100.0).log10() } else { -96.0 };
        
        let windchest_group_id = get_prop("WindchestGroup", "windchestgroup", "").non_empty_or(None);

        let tracker_delay_ms: u32 = 0; 
        let mut pipes = HashMap::new();

        for i in 1..=pipe_count {
            let pipe_key_prefix_upper = format!("Pipe{:03}", i);
            let pipe_key_prefix_lower = format!("pipe{:03}", i);
            let midi_note = first_midi_note + (i as u8 - 1);

            if let Some(attack_path_str) = get_prop(&pipe_key_prefix_upper, &pipe_key_prefix_lower, "").non_empty_or(None) {
                
                if attack_path_str.starts_with("REF:") { continue; }

                let attack_path_str = attack_path_str.replace('\\', "/");
                let attack_sample_path_relative = PathBuf::from(&attack_path_str);
                let mut pitch_tuning_cents: f32 = get_prop(&format!("{}PitchTuning", pipe_key_prefix_upper), &format!("{}pitchtuning", pipe_key_prefix_lower), "0.0").parse().unwrap_or(0.0);
                if original_tuning && pitch_tuning_cents.abs() <= 20.0 { pitch_tuning_cents = 0.0; }

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
        let division_id = String::new(); 
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

    let mut noise_pairs: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();

    for rank in ranks_map.values() {
        if rank.name.contains("Key action") {
            let name_lower = rank.name.to_lowercase();
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

    let mut ranks_to_remove = Vec::new();

    for (base_name, (attack_id_opt, release_id_opt)) in noise_pairs {
        if let (Some(attack_id), Some(release_id)) = (attack_id_opt, release_id_opt) {
            log::info!("Merging Noise Ranks: '{}' <- '{}' (Base: {})", attack_id, release_id, base_name);
    
            if let Some(mut release_rank) = ranks_map.remove(&release_id) {
                if let Some(attack_rank) = ranks_map.get_mut(&attack_id) {
                    attack_rank.name = base_name;
            
                    for (note, release_pipe) in release_rank.pipes.drain() {
                        if let Some(attack_pipe) = attack_rank.pipes.get_mut(&note) {
                            attack_pipe.releases.extend(release_pipe.releases);
                    
                            attack_pipe.releases.sort_by_key(|r| if r.max_key_press_time_ms == -1 { i64::MAX } else { r.max_key_press_time_ms });
                        } 
                    }
                }
                ranks_to_remove.push(release_id); 
            }
        }
    }


    // Build Stops
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

            let rank_count: usize = get_prop("NumberOfRanks", "numberofranks", "0").parse().unwrap_or(0);
            let mut rank_ids = Vec::new();
            for i in 1..=rank_count {
                if let Some(rank_id) = get_prop(&format!("Rank{:03}", i), &format!("rank{:03}", i), "").non_empty_or(None) {
                    rank_ids.push(rank_id.to_string());
                }
            }

            if rank_ids.is_empty() {
                if ranks_map.contains_key(&id_str) {
                    rank_ids.push(id_str.clone());
                }
            }

            if rank_ids.len() == 1 {
                if let Some(rank) = ranks_map.get(&rank_ids[0]) {
                    if rank.is_percussive {
                        name = rank.name.clone();
                    }
                }
            }
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