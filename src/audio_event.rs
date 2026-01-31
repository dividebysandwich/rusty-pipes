use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::TuiMessage;
use crate::app::{ActiveNote, AppMessage};
use crate::audio_convolver::StereoConvolver;
use crate::audio_recorder::AudioRecorder;
use crate::midi_recorder::MidiRecorder;
use crate::organ::Organ;
use crate::voice::{SpawnJob, VOICE_STEALING_FADE_TIME, Voice};

/// If voice limit is exceeded, this finds the oldest *release* samples
/// and forces them to fade out quickly.
pub fn enforce_voice_limit(voices: &mut HashMap<u64, Voice>, sample_rate: u32, polyphony: usize) {
    let active_musical_voices = voices.values().filter(|v| !v.is_fading_out).count();

    if active_musical_voices <= polyphony {
        return;
    }

    let voices_to_steal = active_musical_voices - polyphony;
    let min_age = Duration::from_millis(50);

    let mut candidates: Vec<(u64, Instant)> = voices
        .iter()
        .filter(|(_, v)| {
            !v.is_attack_sample && !v.is_fading_out && v.note_on_time.elapsed() > min_age
        })
        .map(|(id, v)| (*id, v.note_on_time))
        .collect();

    candidates.sort_by_key(|(_, time)| *time);

    for (voice_id, _) in candidates.iter().take(voices_to_steal) {
        if let Some(voice) = voices.get_mut(voice_id) {
            log::warn!("[AudioThread] Stealing Voice ID {}", voice_id);
            voice.is_fading_out = true;
            voice.is_fading_in = false;

            let steal_fade_frames = (sample_rate as f32 * VOICE_STEALING_FADE_TIME) as usize;
            voice.fade_increment = if steal_fade_frames > 0 {
                1.0 / steal_fade_frames as f32
            } else {
                1.0
            };
        }
    }
}

pub fn trigger_note_release(
    stopped_note: ActiveNote,
    organ: &Arc<Organ>,
    voices: &mut HashMap<u64, Voice>,
    sample_rate: u32,
    voice_counter: &mut u64,
    spawner_tx: &mpsc::Sender<SpawnJob>,
) {
    let press_duration = stopped_note.start_time.elapsed().as_millis() as i64;
    let note = stopped_note.note;

    if let Some(rank) = organ.ranks.get(&stopped_note.rank_id) {
        if let Some(pipe) = rank.pipes.get(&note) {
            let release_sample = pipe
                .releases
                .iter()
                .find(|r| {
                    r.max_key_press_time_ms == -1 || press_duration <= r.max_key_press_time_ms
                })
                .or_else(|| pipe.releases.last());

            let mut release_created = false;

            if let Some(release) = release_sample {
                let total_gain = rank.gain_db + pipe.gain_db;
                match Voice::new(
                    &release.path,
                    Arc::clone(&organ),
                    sample_rate,
                    total_gain,
                    false,
                    false,
                    Instant::now(),
                    release.preloaded_bytes.clone(),
                    spawner_tx,
                    rank.windchest_group_id.clone(),
                ) {
                    Ok(mut voice) => {
                        voice.fade_level = 0.0;
                        let release_voice_id = *voice_counter;
                        *voice_counter += 1;
                        voices.insert(release_voice_id, voice);

                        if let Some(attack_voice) = voices.get_mut(&stopped_note.voice_id) {
                            attack_voice.is_cancelled.store(true, Ordering::SeqCst);
                            attack_voice.is_awaiting_release_sample = true;
                            attack_voice.release_voice_id = Some(release_voice_id);
                        } else {
                            if let Some(rv) = voices.get_mut(&release_voice_id) {
                                rv.is_fading_in = true;
                            }
                        }
                        release_created = true;
                    }
                    Err(e) => log::error!("Error creating release: {}", e),
                }
            }

            if !release_created {
                if let Some(voice) = voices.get_mut(&stopped_note.voice_id) {
                    voice.is_cancelled.store(true, Ordering::SeqCst);
                    voice.is_fading_out = true;
                }
            }
        }
    }
}

pub fn handle_note_off(
    note: u8,
    organ: &Arc<Organ>,
    voices: &mut HashMap<u64, Voice>,
    active_notes: &mut HashMap<u8, Vec<ActiveNote>>,
    sample_rate: u32,
    voice_counter: &mut u64,
    spawner_tx: &mpsc::Sender<SpawnJob>,
) {
    if let Some(notes_to_stop) = active_notes.remove(&note) {
        for stopped_note in notes_to_stop {
            trigger_note_release(
                stopped_note,
                organ,
                voices,
                sample_rate,
                voice_counter,
                spawner_tx,
            );
        }
    }
}

pub fn process_note_on(
    msg: AppMessage,
    active_notes: &mut HashMap<u8, Vec<ActiveNote>>,
    organ: &Arc<Organ>,
    voices: &mut HashMap<u64, Voice>,
    voice_counter: &mut u64,
    stop_map: &HashMap<String, usize>,
    sample_rate: u32,
    spawner_tx: &mpsc::Sender<SpawnJob>,
) {
    if let AppMessage::NoteOn(note, _vel, stop_name) = msg {
        let note_on_time = Instant::now();
        if let Some(stop_index) = stop_map.get(&stop_name) {
            let stop = &organ.stops[*stop_index];
            let mut new_notes = Vec::new();

            for rank_id in &stop.rank_ids {
                if let Some(rank) = organ.ranks.get(rank_id) {
                    if let Some(pipe) = rank.pipes.get(&note) {
                        let total_gain = rank.gain_db + pipe.gain_db;
                        match Voice::new(
                            &pipe.attack_sample_path,
                            Arc::clone(&organ),
                            sample_rate,
                            total_gain,
                            false,
                            true,
                            note_on_time,
                            pipe.preloaded_bytes.clone(),
                            spawner_tx,
                            rank.windchest_group_id.clone(),
                        ) {
                            Ok(voice) => {
                                let voice_id = *voice_counter;
                                *voice_counter += 1;
                                voices.insert(voice_id, voice);
                                new_notes.push(ActiveNote {
                                    note,
                                    start_time: note_on_time,
                                    stop_index: *stop_index,
                                    rank_id: rank_id.clone(),
                                    voice_id,
                                });
                            }
                            Err(e) => log::error!("Error creating attack voice: {}", e),
                        }
                    }
                }
            }
            if !new_notes.is_empty() {
                active_notes.entry(note).or_default().extend(new_notes);
            }
        }
    }
}

pub fn process_message(
    msg: AppMessage,
    wet_dry_ratio: &mut f32,
    system_gain: &mut f32,
    polyphony: &mut usize,
    ir_loader_tx: &mpsc::Sender<Result<StereoConvolver, anyhow::Error>>,
    sample_rate: u32,
    buffer_size_frames: usize,
    active_notes: &mut HashMap<u8, Vec<ActiveNote>>,
    organ: &Arc<Organ>,
    voices: &mut HashMap<u64, Voice>,
    voice_counter: &mut u64,
    stop_map: &HashMap<String, usize>,
    spawner_tx: &mpsc::Sender<SpawnJob>,
    pending_queue: &mut VecDeque<AppMessage>,
    active_tremulants: &mut HashMap<String, bool>,
    audio_recorder: &mut Option<AudioRecorder>,
    tui_tx: &mpsc::Sender<TuiMessage>,
    shared_midi_recorder: &Arc<Mutex<Option<MidiRecorder>>>,
) {
    match msg {
        AppMessage::NoteOff(n, s) => {
            let mut removed_from_queue = false;
            if !pending_queue.is_empty() {
                pending_queue.retain(|pending_msg| {
                    if let AppMessage::NoteOn(pending_note, _, pending_stop) = pending_msg {
                        if *pending_note == n && *pending_stop == s {
                            removed_from_queue = true;
                            return false;
                        }
                    }
                    true
                });
            }

            if let Some(idx) = stop_map.get(&s) {
                if let Some(list) = active_notes.get_mut(&n) {
                    // Partition into notes to release and notes to keep
                    let (to_release, to_keep) = 
                        list.drain(..).partition(|active_note| active_note.stop_index == *idx);
                    *list = to_keep;
                    
                    for stopped in to_release{
                        trigger_note_release(
                            stopped,
                            organ,
                            voices,
                            sample_rate,
                            voice_counter,
                            spawner_tx,
                        );
                    }

                    if list.is_empty() {
                        active_notes.remove(&n);
                    }
                }
            }
        }
        AppMessage::AllNotesOff => {
            pending_queue.clear();
            let notes: Vec<u8> = active_notes.keys().cloned().collect();
            for note in notes {
                handle_note_off(
                    note,
                    organ,
                    voices,
                    active_notes,
                    sample_rate,
                    voice_counter,
                    spawner_tx,
                );
            }
        }
        AppMessage::SetTremulantActive(id, active) => {
            active_tremulants.insert(id, active);
        }
        AppMessage::StartAudioRecording => {
            match AudioRecorder::start(organ.name.clone(), sample_rate) {
                Ok(rec) => {
                    *audio_recorder = Some(rec);
                    let _ = tui_tx.send(TuiMessage::MidiLog("Audio Recording Started".into()));
                }
                Err(e) => {
                    let _ = tui_tx.send(TuiMessage::Error(format!("Rec Error: {}", e)));
                }
            }
        }
        AppMessage::StopAudioRecording => {
            if let Some(rec) = audio_recorder.take() {
                rec.stop();
                let _ = tui_tx.send(TuiMessage::MidiLog("Audio Recording Stopped/Saved".into()));
            }
        }
        AppMessage::StartMidiRecording => {
            let mut guard = shared_midi_recorder.lock().unwrap();
            if guard.is_none() {
                *guard = Some(MidiRecorder::new(organ.name.clone()));
                let _ = tui_tx.send(TuiMessage::MidiLog("MIDI Recording Started".into()));
            }
        }
        AppMessage::StopMidiRecording => {
            let mut guard = shared_midi_recorder.lock().unwrap();
            if let Some(recorder) = guard.take() {
                match recorder.save() {
                    Ok(path) => {
                        let _ = tui_tx.send(TuiMessage::MidiLog(format!("Saved: {}", path)));
                    }
                    Err(e) => {
                        let _ = tui_tx.send(TuiMessage::Error(format!("MIDI Save Error: {}", e)));
                    }
                }
            }
        }
        AppMessage::SetReverbWetDry(r) => *wet_dry_ratio = r.clamp(0.0, 1.0),
        AppMessage::SetReverbIr(p) => {
            let tx = ir_loader_tx.clone();
            thread::spawn(move || {
                let _ = tx.send(StereoConvolver::from_file(
                    &p,
                    sample_rate,
                    buffer_size_frames,
                ));
            });
        }
        AppMessage::SetGain(g) => *system_gain = g,
        AppMessage::SetPolyphony(p) => *polyphony = p,
        AppMessage::Quit => {
            // tell the Logic Thread to close the Window.
            // This allows main.rs to finish the loop and handle the respawn.
            let _ = tui_tx.send(TuiMessage::ForceClose);
        }
        _ => {}
    }
}
