use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::fs::File;
use std::io::BufReader;
use serde::Deserialize;
use quick_xml::events::{Event, BytesStart};
use quick_xml::reader::Reader;
use quick_xml::de::Deserializer;

use crate::wav_converter;
use crate::organ::{Organ, Stop, Rank, Pipe, ReleaseSample, ConversionTask};

// XML Helper Definitions

fn default_string() -> String { "".to_string() }
fn default_i64() -> i64 { -1 } 
fn default_u8() -> u8 { 0 }

#[derive(Debug, Deserialize, PartialEq)]
struct XmlV7Object {
    a: Option<String>,
    b: Option<String>,
    c: Option<String>,
    d: Option<String>,
    e: Option<String>,
    f: Option<String>,
    g: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlDivision {
    #[serde(rename = "DivisionID", alias = "a", default = "default_string")]
    id: String,
    #[serde(rename = "Name", alias = "b", default = "default_string")]
    name: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct XmlGeneral {
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
    #[serde(rename = "PipeID", default = "default_string")]
    id: String,
    #[serde(rename = "RankID", default = "default_string")]
    rank_id: String,
    #[serde(rename = "NormalMIDINoteNumber", default = "default_u8")]
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

// Logic

/// Loads and parses a Hauptwerk (.Organ_Hauptwerk_xml) file.
pub fn load_hauptwerk(
    path: &Path, 
    convert_to_16_bit: bool, 
    pre_cache: bool, 
    _original_tuning: bool,
    target_sample_rate: u32,
    progress_tx: &Option<mpsc::Sender<(f32, String)>>,
) -> Result<Organ> {
    let logical_path = Organ::normalize_path_preserve_symlinks(path)?;

    let file = File::open(&logical_path).with_context(|| format!("Failed to open {:?}", logical_path))?;
    let mut reader = Reader::from_reader(BufReader::new(file));
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = false; 

    println!("Loading Hauptwerk organ from: {:?}", logical_path);
    if let Some(tx) = progress_tx { let _ = tx.send((0.0, "Parsing XML...".to_string())); }

    let organ_root_path = logical_path.parent().and_then(|p| p.parent())
        .ok_or_else(|| anyhow!("Invalid Hauptwerk file path structure."))?;
    let organ_name = logical_path.file_stem().unwrap_or_default().to_string_lossy().replace(".Organ_Hauptwerk_xml", "");
    let cache_path = Organ::get_organ_cache_dir(&organ_name)?;

    let mut organ = Organ {
        base_path: organ_root_path.to_path_buf(),
        cache_path: cache_path.clone(),
        name: organ_name.clone(),
        sample_cache: if pre_cache { Some(HashMap::new()) } else { None },
        metadata_cache: if pre_cache { Some(HashMap::new()) } else { None },
        ..Default::default()
    };

    let mut xml_stops = Vec::new();
    let mut xml_ranks = Vec::new();
    let mut xml_stop_ranks = Vec::new();
    let mut xml_pipes = Vec::new();
    let mut xml_layers = Vec::new();
    let mut xml_attack_samples = Vec::new();
    let mut xml_release_samples = Vec::new();
    let mut xml_samples = Vec::new();
    let mut xml_divisions = Vec::new();
    let mut organ_defined_name = String::new();

    let mut buf = Vec::new();
    let mut current_object_type = String::new();

    fn parse_snippet<'a, T: Deserialize<'a>>(xml: &'a str) -> Result<T> {
        let mut reader = quick_xml::reader::NsReader::from_str(xml);
        reader.config_mut().trim_text(true);            
        reader.config_mut().expand_empty_elements = true;
        let mut deserializer = Deserializer::borrowing(reader);
        T::deserialize(&mut deserializer).map_err(|e| anyhow::anyhow!("{}", e))
    }

    fn read_element_raw(
        reader: &mut Reader<BufReader<File>>, 
        start_event: &BytesStart, 
        tag_name: &[u8]
    ) -> Result<String> {
        let mut depth = 1;
        let mut xml = String::from("<");
        let name_str = String::from_utf8_lossy(tag_name);
        xml.push_str(&name_str);

        for attr in start_event.attributes() {
            let attr = attr?;
            xml.push(' ');
            xml.push_str(&String::from_utf8_lossy(attr.key.as_ref()));
            xml.push_str("=\"");
            xml.push_str(&String::from_utf8_lossy(&attr.value));
            xml.push_str("\"");
        }
        xml.push('>');

        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf)? {
                Event::Start(e) => {
                    depth += 1;
                    xml.push('<');
                    xml.push_str(&String::from_utf8_lossy(e.name().as_ref()));
                    for attr in e.attributes() {
                        let attr = attr?;
                        xml.push(' ');
                        xml.push_str(&String::from_utf8_lossy(attr.key.as_ref()));
                        xml.push_str("=\"");
                        xml.push_str(&String::from_utf8_lossy(&attr.value));
                        xml.push_str("\"");
                    }
                    xml.push('>');
                }
                Event::End(e) => {
                    depth -= 1;
                    xml.push_str("</");
                    xml.push_str(&String::from_utf8_lossy(e.name().as_ref()));
                    xml.push('>');
                    if depth == 0 {
                        return Ok(xml);
                    }
                }
                Event::Empty(e) => {
                    xml.push('<');
                    xml.push_str(&String::from_utf8_lossy(e.name().as_ref()));
                    for attr in e.attributes() {
                        let attr = attr?;
                        xml.push(' ');
                        xml.push_str(&String::from_utf8_lossy(attr.key.as_ref()));
                        xml.push_str("=\"");
                        xml.push_str(&String::from_utf8_lossy(&attr.value));
                        xml.push_str("\"");
                    }
                    xml.push_str("/>");
                }
                Event::Text(e) => xml.push_str(&String::from_utf8_lossy(&e)),
                Event::CData(e) => {
                    xml.push_str("<![CDATA[");
                    xml.push_str(&String::from_utf8_lossy(&e));
                    xml.push_str("]]>");
                }
                Event::Eof => return Err(anyhow!("Unexpected EOF while reading {}", name_str)),
                _ => {} 
            }
            buf.clear();
        }
    }

    fn deserialize_empty_item<T: for<'de> Deserialize<'de>>(
        empty_event: &BytesStart, 
        tag_name: &[u8]
    ) -> Result<T> {
        let mut xml = String::from("<");
        let name_str = String::from_utf8_lossy(tag_name);
        xml.push_str(&name_str);
        for attr in empty_event.attributes() {
            let attr = attr?;
            xml.push(' ');
            xml.push_str(&String::from_utf8_lossy(attr.key.as_ref()));
            xml.push_str("=\"");
            xml.push_str(&String::from_utf8_lossy(&attr.value));
            xml.push_str("\"");
        }
        xml.push_str("/>");
        parse_snippet(&xml).map_err(|e| anyhow::anyhow!("DeError: {} | XML: {}", e, xml))
    }

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                let local = name.local_name();
                let tag_name = local.as_ref();

                match tag_name {
                    b"ObjectList" => {
                        if let Some(Ok(attr)) = e.attributes().find(|a| a.as_ref().map_or(false, |a| a.key.local_name().as_ref() == b"ObjectType")) {
                            current_object_type = String::from_utf8_lossy(&attr.value).to_string();
                        }
                    },
                    b"Stop" if current_object_type == "Stop" => {
                        if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(s) = parse_snippet(&raw) { xml_stops.push(s); }
                        }
                    },
                    b"Rank" if current_object_type == "Rank" => {
                        if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(r) = parse_snippet(&raw) { xml_ranks.push(r); }
                        }
                    },
                    b"StopRank" if current_object_type == "StopRank" => {
                        if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(sr) = parse_snippet(&raw) { xml_stop_ranks.push(sr); }
                        }
                    },
                    b"Pipe_SoundEngine01" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(p) = parse_snippet(&raw) { xml_pipes.push(p); }
                            }
                    },
                    b"Pipe_SoundEngine01_Layer" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(l) = parse_snippet(&raw) { xml_layers.push(l); }
                            }
                    },
                    b"Pipe_SoundEngine01_AttackSample" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(a) = parse_snippet(&raw) { xml_attack_samples.push(a); }
                            }
                    },
                    b"Pipe_SoundEngine01_ReleaseSample" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(r) = parse_snippet(&raw) { xml_release_samples.push(r); }
                            }
                    },
                    b"Sample" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            match parse_snippet::<XmlSample>(&raw) {
                                Ok(s) => xml_samples.push(s),
                                Err(e) => eprintln!("Sample Parse Err: {}", e),
                            }
                            }
                    },
                    b"Division" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(d) = parse_snippet(&raw) { xml_divisions.push(d); }
                            }
                    },
                    b"General" | b"_General" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                            if let Ok(g) = parse_snippet::<XmlGeneral>(&raw) {
                                if !g.name.is_empty() { organ_defined_name = g.name; }
                            }
                            }
                    },
                    b"o" => {
                            if let Ok(raw) = read_element_raw(&mut reader, e, tag_name) {
                                if let Ok(obj) = parse_snippet::<XmlV7Object>(&raw) {
                                    match current_object_type.as_str() {
                                        "Stop" => xml_stops.push(XmlStop { id: obj.a.unwrap_or_default(), name: obj.b.unwrap_or_default(), division_id: obj.c.unwrap_or_default() }),
                                        "Rank" => xml_ranks.push(XmlRank { id: obj.a.unwrap_or_default(), name: obj.b.unwrap_or_default(), division_id: "".to_string() }),
                                        "StopRank" => if let (Some(sid), Some(rid)) = (obj.a, obj.d) { xml_stop_ranks.push(XmlStopRank { stop_id: sid, rank_id: rid }) },
                                        "Pipe_SoundEngine01" => xml_pipes.push(XmlPipe { id: obj.a.unwrap_or_default(), rank_id: obj.b.unwrap_or_default(), midi_note: obj.d.as_deref().unwrap_or("0").parse().unwrap_or(0) }),
                                        "Pipe_SoundEngine01_Layer" => xml_layers.push(XmlLayer { id: obj.a.unwrap_or_default(), pipe_id: obj.b.unwrap_or_default() }),
                                        "Pipe_SoundEngine01_AttackSample" => xml_attack_samples.push(XmlAttackSample { layer_id: obj.b.unwrap_or_default(), sample_id: obj.c.unwrap_or_default() }),
                                        "Pipe_SoundEngine01_ReleaseSample" => xml_release_samples.push(XmlReleaseSample { layer_id: obj.b.unwrap_or_default(), sample_id: obj.c.unwrap_or_default(), max_key_press_time_ms: -1 }),
                                        "Sample" => xml_samples.push(XmlSample { id: obj.a.unwrap_or_default(), installation_package_id: obj.b.unwrap_or_default(), path: obj.c.unwrap_or_default(), pitch_exact_sample_pitch: None, pitch_normal_midi_note_number: None }),
                                        "Division" => xml_divisions.push(XmlDivision { id: obj.a.unwrap_or_default(), name: obj.b.unwrap_or_default() }),
                                        _ => {}
                                    }
                                }
                            }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = e.name(); 
                let local = name.local_name();
                let tag_name = local.as_ref();

                match tag_name {
                    b"Stop" if current_object_type == "Stop" => { if let Ok(s) = deserialize_empty_item::<XmlStop>(e, tag_name) { xml_stops.push(s); } },
                    b"Rank" if current_object_type == "Rank" => { if let Ok(r) = deserialize_empty_item::<XmlRank>(e, tag_name) { xml_ranks.push(r); } },
                    b"StopRank" if current_object_type == "StopRank" => { if let Ok(sr) = deserialize_empty_item::<XmlStopRank>(e, tag_name) { xml_stop_ranks.push(sr); } },
                    b"Pipe_SoundEngine01" => { if let Ok(p) = deserialize_empty_item::<XmlPipe>(e, tag_name) { xml_pipes.push(p); } },
                    b"Pipe_SoundEngine01_Layer" => { if let Ok(l) = deserialize_empty_item::<XmlLayer>(e, tag_name) { xml_layers.push(l); } },
                    b"Pipe_SoundEngine01_AttackSample" => { if let Ok(a) = deserialize_empty_item::<XmlAttackSample>(e, tag_name) { xml_attack_samples.push(a); } },
                    b"Pipe_SoundEngine01_ReleaseSample" => { if let Ok(r) = deserialize_empty_item::<XmlReleaseSample>(e, tag_name) { xml_release_samples.push(r); } },
                    b"Sample" => { if let Ok(s) = deserialize_empty_item::<XmlSample>(e, tag_name) { xml_samples.push(s); } },
                    b"Division" => { if let Ok(d) = deserialize_empty_item::<XmlDivision>(e, tag_name) { xml_divisions.push(d); } },
                    b"o" => {
                            if let Ok(obj) = deserialize_empty_item::<XmlV7Object>(e, tag_name) {
                                match current_object_type.as_str() {
                                    "Stop" => xml_stops.push(XmlStop { id: obj.a.unwrap_or_default(), name: obj.b.unwrap_or_default(), division_id: obj.c.unwrap_or_default() }),
                                    "Rank" => xml_ranks.push(XmlRank { id: obj.a.unwrap_or_default(), name: obj.b.unwrap_or_default(), division_id: "".to_string() }),
                                    "StopRank" => if let (Some(sid), Some(rid)) = (obj.a, obj.d) { xml_stop_ranks.push(XmlStopRank { stop_id: sid, rank_id: rid }) },
                                    "Pipe_SoundEngine01" => xml_pipes.push(XmlPipe { id: obj.a.unwrap_or_default(), rank_id: obj.b.unwrap_or_default(), midi_note: obj.d.as_deref().unwrap_or("0").parse().unwrap_or(0) }),
                                    "Pipe_SoundEngine01_Layer" => xml_layers.push(XmlLayer { id: obj.a.unwrap_or_default(), pipe_id: obj.b.unwrap_or_default() }),
                                    "Pipe_SoundEngine01_AttackSample" => xml_attack_samples.push(XmlAttackSample { layer_id: obj.b.unwrap_or_default(), sample_id: obj.c.unwrap_or_default() }),
                                    "Pipe_SoundEngine01_ReleaseSample" => xml_release_samples.push(XmlReleaseSample { layer_id: obj.b.unwrap_or_default(), sample_id: obj.c.unwrap_or_default(), max_key_press_time_ms: -1 }),
                                    "Sample" => xml_samples.push(XmlSample { id: obj.a.unwrap_or_default(), installation_package_id: obj.b.unwrap_or_default(), path: obj.c.unwrap_or_default(), pitch_exact_sample_pitch: None, pitch_normal_midi_note_number: None }),
                                    "Division" => xml_divisions.push(XmlDivision { id: obj.a.unwrap_or_default(), name: obj.b.unwrap_or_default() }),
                                    _ => {}
                                }
                            }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("XML Parse Error: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    println!("Loaded: {} Stops, {} Ranks, {} StopRanks, {} Pipes", xml_stops.len(), xml_ranks.len(), xml_stop_ranks.len(), xml_pipes.len());

    if !organ_defined_name.is_empty() { organ.name = organ_defined_name; }

    // Build Division Map (ID -> Name)
    let mut division_name_map: HashMap<String, String> = HashMap::new();
    for div in xml_divisions {
        division_name_map.insert(div.id, div.name);
    }

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
            return name.chars().take(3).collect::<String>();
        }
        "".to_string()
    };

    let mut stop_to_ranks_map: HashMap<String, Vec<String>> = HashMap::new();
    for sr in xml_stop_ranks {
        stop_to_ranks_map.entry(sr.stop_id).or_default().push(sr.rank_id);
    }

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
            windchest_group_id: None,
            is_percussive: false,
        });
    }

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
                Organ::try_infer_midi_note_from_filename(&attack_sample_info.path).unwrap_or(target_midi_note)
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

    Organ::process_tasks_parallel(&organ.base_path, &organ.cache_path, conversion_tasks, target_sample_rate, progress_tx)?;

    if let Some(tx) = progress_tx { let _ = tx.send((1.0, "Assembling organ...".to_string())); }

    for layer in &xml_layers {
        let Some(pipe_info) = pipe_map.get(&layer.pipe_id) else {
            log::warn!("Layer {} references non-existent PipeID {}", layer.id, layer.pipe_id);
            continue;
        };

        let Some(rank) = ranks_map.get_mut(&pipe_info.rank_id) else {
            log::debug!("Pipe {} references non-existent RankID {}", pipe_info.id, pipe_info.rank_id);
            continue;
        };

        if rank.pipes.contains_key(&pipe_info.midi_note) {
            continue;
        }

        let Some(attack_link) = attack_map.get(&layer.id) else {
            log::warn!("Layer {} has no attack sample link.", layer.id);
            continue;
        };

        let Some(attack_sample_info) = sample_map.get(&attack_link.sample_id) else {
            log::warn!("Layer {} references non-existent SampleID {}", layer.id, attack_link.sample_id);
            continue;
        };
        
        let target_midi_note = pipe_info.midi_note as f32;

        let original_midi_note = if let Some(pitch_hz) = attack_sample_info.pitch_exact_sample_pitch {
            if pitch_hz > 0.0 {
                12.0 * (pitch_hz / 440.0).log2() + 69.0
            } else {
                target_midi_note
            }
        } else if let Some(midi_note) = attack_sample_info.pitch_normal_midi_note_number {
            midi_note as f32
        } else {
            if let Some(inferred_note) = Organ::try_infer_midi_note_from_filename(&attack_sample_info.path) {
                inferred_note 
            } else {
                target_midi_note 
            }
        };

        let final_pitch_tuning_cents = (target_midi_note - original_midi_note) * 100.0;

        let attack_path_str = format!("OrganInstallationPackages/{:0>6}/{}", attack_sample_info.installation_package_id,attack_sample_info.path.replace('\\', "/"));
        let attack_sample_path_relative = PathBuf::from(&attack_path_str);
        
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

        let mut releases = Vec::new();
        if let Some(xml_release_links) = release_map.get(&layer.id) {
            for release_link in xml_release_links {
                if let Some(release_sample_info) = sample_map.get(&release_link.sample_id) {
                    let rel_path_str = format!("OrganInstallationPackages/{:0>6}/{}", release_sample_info.installation_package_id, release_sample_info.path.replace('\\', "/"));
                    let rel_path_buf = PathBuf::from(&rel_path_str);

                    let is_self_reference = release_sample_info.path == attack_sample_info.path 
                                            && release_sample_info.installation_package_id == attack_sample_info.installation_package_id;

                    if is_self_reference {
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
                        }
                    } else {
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

        if releases.is_empty() {
            if let Ok(Some(extracted_path)) = wav_converter::try_extract_release_sample(
                &attack_sample_path_relative,
                &organ.base_path,
                &organ.cache_path,
                final_pitch_tuning_cents,
                convert_to_16_bit,
                target_sample_rate
            ) {
                releases.push(ReleaseSample {
                    path: extracted_path,
                    max_key_press_time_ms: -1, 
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

        let mut rank_ids = stop_to_ranks_map.get(&xs.id).cloned().unwrap_or_default();
        let mut linkage_method = "Explicit".to_string();

        if rank_ids.is_empty() && ranks_map.contains_key(&xs.id) {
            rank_ids.push(xs.id.clone());
            linkage_method = "ID Match".to_string();
        }

        let has_pipes = rank_ids.iter().any(|rid| {
            ranks_map.get(rid).map(|r| !r.pipes.is_empty()).unwrap_or(false)
        });

        if rank_ids.is_empty() || !has_pipes {
            let (stop_words, stop_pitches) = tokenize(&xs.name);
            let stop_id_num = parse_id(&xs.id);

            let mut best_score = 0;
            let mut best_id_match = String::new();
            let mut min_distance = i32::MAX;

            for rank in ranks_map.values() {
                if rank.pipes.is_empty() { continue; }

                if !xs.division_id.is_empty() && !rank.division_id.is_empty() {
                    if xs.division_id != rank.division_id { continue; }
                }

                let (rank_words, rank_pitches) = tokenize(&rank.name);

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
            rank_ids.sort_by(|a_id, b_id| {
                let get_score = |id: &str| -> i32 {
                    let Some(r) = ranks_map.get(id) else { return -9999; };
                    let n = r.name.to_lowercase();
                    let mut score = 0;
                    
                    if n.contains("front") || n.contains("direct") || n.contains("main") || n.contains("dry") { score += 100; }
                    if n.contains("rear") || n.contains("diffuse") || n.contains("surround") || n.contains("wet") { score -= 100; }
                    if n.contains("trem") { score -= 20; }
                    
                    score
                };

                let score_a = get_score(a_id);
                let score_b = get_score(b_id);
                
                score_b.cmp(&score_a)
            });

            if let Some(winner) = rank_ids.first().cloned() {
                rank_ids = vec![winner];
            }
        }
        
        let final_has_pipes = rank_ids.iter().any(|rid| {
            ranks_map.get(rid).map(|r| !r.pipes.is_empty()).unwrap_or(false)
        });

        let prefix = get_division_prefix(&xs.division_id);
        let mut final_name = xs.name.clone();
        
        if !prefix.is_empty() {
            let name_lower = final_name.to_lowercase();
            let prefix_lower = prefix.to_lowercase();
            
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
            name: final_name, 
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