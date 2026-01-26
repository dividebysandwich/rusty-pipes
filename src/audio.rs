use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::TuiMessage;
use crate::app::{ActiveNote, AppMessage};
use crate::midi_recorder::MidiRecorder;
use crate::organ::Organ;

use crate::audio_convolver::StereoConvolver;
use crate::audio_event::{enforce_voice_limit, process_message, process_note_on};
use crate::audio_loader::run_loader_job;
use crate::audio_recorder::AudioRecorder;
use crate::voice::{
    CHANNEL_COUNT, MAX_NEW_VOICES_PER_BLOCK, SpawnJob, TREMULANT_AM_BOOST, TremulantLfo, Voice,
};

// Handle struct that manages the lifecycle for the audio thread
#[allow(dead_code)]
pub struct AudioHandle {
    stream: Stream,
    stop_signal: Arc<AtomicBool>,
}

// When main.rs drops this handle, we signal the thread to quit
impl Drop for AudioHandle {
    fn drop(&mut self) {
        log::info!("[Audio] Stopping audio processing thread...");
        self.stop_signal.store(true, Ordering::SeqCst);
    }
}

pub fn get_supported_sample_rates(device_name: Option<String>) -> Result<Vec<u32>> {
    let host = get_cpal_host();
    let device = if let Some(name) = device_name {
        host.output_devices()?
            .find(|d| d.id().map_or(false, |n| n.to_string() == name))
            .ok_or_else(|| anyhow!("Device not found"))?
    } else {
        host.default_output_device()
            .ok_or_else(|| anyhow!("No default device"))?
    };
    let supported_configs = device.supported_output_configs()?;
    let mut available_rates = Vec::new();
    let standard_rates = [44100, 48000, 88200, 96000, 176400, 192000];
    for config_range in supported_configs {
        let min = config_range.min_sample_rate();
        let max = config_range.max_sample_rate();
        for &rate in &standard_rates {
            if rate >= min && rate <= max && !available_rates.contains(&rate) {
                available_rates.push(rate);
            }
        }
    }
    available_rates.sort();
    if available_rates.is_empty() {
        available_rates.push(48000);
    }
    Ok(available_rates)
}

fn get_cpal_host() -> cpal::Host {
    let available_hosts = cpal::available_hosts();
    let host_names: Vec<_> = available_hosts.iter().map(|id| id.name()).collect();
    log::info!("[Audio] Available Hosts: {:?}", host_names);

    let priority_order = ["asio","jack", "alsa"];

    for target_name in priority_order {
        if let Some(host_id) = available_hosts
            .iter()
            .find(|id| id.name().to_lowercase().contains(target_name))
        {
            if let Ok(host) = cpal::host_from_id(*host_id) {
                log::info!("[Audio] Probing Host: {}", host.id().name());

                match host.output_devices() {
                    Ok(mut devices) => {
                        // Get the first device (or fail if none)
                        if let Some(device) = devices.next() {
                            // Interrogate the device!
                            // Merely existing isn't enough. We try to query its supported configurations.
                            // If the JACK server is down, this call will fail or return an empty iterator.
                            match device.supported_output_configs() {
                                Ok(mut configs) => {
                                    if configs.next().is_some() {
                                        log::info!(
                                            "[Audio] Selected Host: {} (Validated)",
                                            host.id().name()
                                        );
                                        return host;
                                    } else {
                                        log::warn!(
                                            "[Audio] Host '{}' found a device, but it has 0 supported configs. Skipping.",
                                            host.id().name()
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::warn!(
                                        "[Audio] Host '{}' device failed config query: {}. Skipping.",
                                        host.id().name(),
                                        e
                                    );
                                }
                            }
                        } else {
                            log::warn!(
                                "[Audio] Host '{}' initialized but found NO devices. Skipping.",
                                host.id().name()
                            );
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "[Audio] Host '{}' failed to list devices: {}. Skipping.",
                            host.id().name(),
                            e
                        );
                    }
                }
            }
        }
    }

    // Final Fallback
    log::info!("[Audio] No prioritized hosts were usable. Falling back to system default.");
    cpal::default_host()
}

pub fn get_audio_device_names() -> Result<Vec<String>> {
    let host = get_cpal_host();
    let devices = host.output_devices()?;
    let mut names = Vec::new();
    for device in devices {
        if let Ok(name) = device.id() {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

#[allow(non_snake_case)]
pub fn get_default_audio_device_name() -> Result<Option<String>> {
    let host = get_cpal_host();
    match host.default_output_device() {
        Some(device) => Ok(Some(device.id()?.to_string())),
        None => Ok(None),
    }
}

/// Spawns the dedicated audio processing thread.
fn spawn_audio_processing_thread<P>(
    rx: mpsc::Receiver<AppMessage>,
    mut producer: P,
    organ: Arc<Organ>,
    sample_rate: u32,
    buffer_size_frames: usize,
    mut system_gain: f32,
    mut polyphony: usize,
    tui_tx: mpsc::Sender<TuiMessage>,
    shared_midi_recorder: Arc<Mutex<Option<MidiRecorder>>>,
    stop_signal: Arc<AtomicBool>,
) where
    P: Producer<Item = f32> + Send + 'static,
{
    let (ir_loader_tx, ir_loader_rx) = mpsc::channel::<Result<StereoConvolver>>();
    let (spawner_tx, spawner_rx) = mpsc::channel::<SpawnJob>();

    // Background Thread: Spawner / Loader
    thread::spawn(move || {
        log::info!("[SpawnerThread] Started.");
        for job in spawner_rx {
            // Detached thread for each voice load to allow concurrency
            thread::spawn(move || {
                run_loader_job(job);
            });
        }
        log::info!("[SpawnerThread] Shutting down.");
    });

    // Real-time Audio Processing Thread
    thread::spawn(move || {
        let stop_name_to_index_map: HashMap<String, usize> = organ
            .stops
            .iter()
            .enumerate()
            .map(|(i, stop)| (stop.name.clone(), i))
            .collect();

        let mut active_notes: HashMap<u8, Vec<ActiveNote>> = HashMap::new();
        let mut voices: HashMap<u64, Voice> = HashMap::with_capacity(128);
        let mut voice_counter: u64 = 0;

        let mut mix_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT];
        // Scratch buffers for Reverb
        let mut reverb_dry_l: Vec<f32> = vec![0.0; buffer_size_frames];
        let mut reverb_dry_r: Vec<f32> = vec![0.0; buffer_size_frames];
        let mut wet_buffer_l: Vec<f32> = vec![0.0; buffer_size_frames];
        let mut wet_buffer_r: Vec<f32> = vec![0.0; buffer_size_frames];

        let mut convolver = StereoConvolver::new(buffer_size_frames);
        let mut wet_dry_ratio: f32 = 0.0;

        let mut voices_to_remove: Vec<u64> = Vec::with_capacity(32);
        let buffer_duration_secs = buffer_size_frames as f32 / sample_rate as f32;

        let mut last_ui_update = Instant::now();
        let ui_update_interval = Duration::from_millis(250);
        let mut last_reported_voice_count: usize = usize::MAX;
        let mut max_load_accumulator = 0.0f32;

        let mut pending_note_queue: VecDeque<AppMessage> = VecDeque::with_capacity(64);
        let mut active_tremulants_ids: HashMap<String, bool> = HashMap::new();
        let mut tremulant_lfos: HashMap<String, TremulantLfo> = HashMap::new();
        let mut prev_windchest_mods: HashMap<String, f32> = HashMap::new();
        let mut scratch_read_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT * 2];
        let mut audio_recorder: Option<AudioRecorder> = None;

        loop {
            // Check for stop signal
            if stop_signal.load(Ordering::Relaxed) {
                log::info!("[AudioThread] Stop signal received. Exiting.");
                break;
            }

            let start_time = Instant::now();

            // Drain incoming messages from UI to internal queue
            // We differentiate "Immediate" vs "Deferrable" events
            while let Ok(msg) = rx.try_recv() {
                if stop_signal.load(Ordering::Relaxed) {
                    log::info!("[AudioThread] Stop signal received. Exiting.");
                    break;
                }
                match msg {
                    AppMessage::NoteOn(..) => pending_note_queue.push_back(msg),
                    _ => process_message(
                        msg,
                        &mut wet_dry_ratio,
                        &mut system_gain,
                        &mut polyphony,
                        &ir_loader_tx,
                        sample_rate,
                        buffer_size_frames,
                        &mut active_notes,
                        &organ,
                        &mut voices,
                        &mut voice_counter,
                        &stop_name_to_index_map,
                        &spawner_tx,
                        &mut pending_note_queue,
                        &mut active_tremulants_ids,
                        &mut audio_recorder,
                        &tui_tx,
                        &shared_midi_recorder,
                    ),
                }
            }

            // Throttle Note Ons
            let mut new_voice_count = 0;
            while new_voice_count < MAX_NEW_VOICES_PER_BLOCK {
                if let Some(msg) = pending_note_queue.pop_front() {
                    process_note_on(
                        msg,
                        &mut active_notes,
                        &organ,
                        &mut voices,
                        &mut voice_counter,
                        &stop_name_to_index_map,
                        sample_rate,
                        &spawner_tx,
                    );
                    new_voice_count += 1;
                } else {
                    break;
                }
            }

            // Receive Reverb IR
            if let Ok(Ok(conv)) = ir_loader_rx.try_recv() {
                convolver = conv;
                if wet_dry_ratio == 0.0 {
                    wet_dry_ratio = 0.3;
                }
            }

            mix_buffer.fill(0.0);
            enforce_voice_limit(&mut voices, sample_rate, polyphony);

            // Update Tremulants
            let dt = buffer_size_frames as f32 / sample_rate as f32;
            let mut current_windchest_mods: HashMap<String, f32> = HashMap::new();

            for (trem_id, trem_def) in &organ.tremulants {
                let is_active = *active_tremulants_ids.get(trem_id).unwrap_or(&false);
                let target_level = if is_active { 1.0 } else { 0.0 };
                let lfo = tremulant_lfos
                    .entry(trem_id.clone())
                    .or_insert(TremulantLfo {
                        phase: 0.0,
                        current_level: 0.0,
                    });

                if lfo.current_level != target_level {
                    let rate = if is_active {
                        if trem_def.start_rate > 0.0 {
                            trem_def.start_rate
                        } else {
                            1000.0
                        }
                    } else {
                        if trem_def.stop_rate > 0.0 {
                            trem_def.stop_rate
                        } else {
                            1000.0
                        }
                    };
                    let change = rate * dt;
                    if lfo.current_level < target_level {
                        lfo.current_level = (lfo.current_level + change).min(target_level);
                    } else {
                        lfo.current_level = (lfo.current_level - change).max(target_level);
                    }
                }

                if lfo.current_level <= 0.0 && !is_active {
                    continue;
                }

                let freq = if trem_def.period > 0.0 {
                    1000.0 / trem_def.period
                } else {
                    0.0
                };
                let phase_inc = (freq * buffer_size_frames as f32) / sample_rate as f32;
                lfo.phase = (lfo.phase + phase_inc) % 1.0;

                let sine_val = (lfo.phase * std::f32::consts::TAU).sin();
                let am_swing = trem_def.amp_mod_depth * 0.01 * TREMULANT_AM_BOOST;
                let active_am = 1.0 + (sine_val * am_swing * 0.5);
                let final_am = 1.0 + (active_am - 1.0) * lfo.current_level;

                for wc_group in organ.windchest_groups.values() {
                    if wc_group.tremulant_ids.contains(trem_id) {
                        let existing =
                            *current_windchest_mods.get(&wc_group.id_str).unwrap_or(&1.0);
                        let new_mod = existing * final_am;
                        current_windchest_mods.insert(wc_group.id_str.clone(), new_mod);
                        // Handle unpadded ID (e.g. "1" vs "01")
                        let unpadded = wc_group.id_str.trim_start_matches('0');
                        let key_unpadded = if unpadded.is_empty() { "0" } else { unpadded };
                        if key_unpadded != wc_group.id_str {
                            current_windchest_mods.insert(key_unpadded.to_string(), new_mod);
                        }
                    }
                }
            }

            // Crossfade Logic
            // Checks if any attack voices are waiting for their release samples to be ready
            let mut crossfades_to_start: Vec<(u64, u64)> = Vec::with_capacity(16);
            for (attack_id, attack_voice) in voices.iter() {
                if attack_voice.is_awaiting_release_sample {
                    if let Some(release_id) = attack_voice.release_voice_id {
                        if let Some(rv) = voices.get(&release_id) {
                            // Check if the release voice has buffered enough data to start playing
                            // We need at least one buffer worth of data to be safe
                            let frames_buffered = rv.input_buffer.len() / CHANNEL_COUNT;
                            let rb_available = rv.consumer.occupied_len() / CHANNEL_COUNT;

                            // Condition: Either we have data in the input buffer,
                            // OR the ringbuffer has enough to fill it.
                            if frames_buffered > 0 || rb_available > buffer_size_frames {
                                crossfades_to_start.push((*attack_id, release_id));
                            } else if rv.is_finished.load(Ordering::Relaxed) {
                                // If the loader finished but gave us no data, abort the wait
                                crossfades_to_start.push((*attack_id, u64::MAX));
                            }
                        } else {
                            // Release voice died?
                            crossfades_to_start.push((*attack_id, u64::MAX));
                        }
                    }
                }
            }

            // Apply the crossfade state changes
            for (aid, rid) in crossfades_to_start {
                if let Some(av) = voices.get_mut(&aid) {
                    av.is_fading_out = true;
                    av.is_awaiting_release_sample = false;
                    av.release_voice_id = None;
                }
                if rid != u64::MAX {
                    if let Some(rv) = voices.get_mut(&rid) {
                        rv.is_fading_in = true;
                    }
                }
            }

            // Voice Processing Loop
            for (voice_id, voice) in voices.iter_mut() {
                if voice.is_fading_out && voice.fade_level <= 0.0001 {
                    voices_to_remove.push(*voice_id);
                    continue;
                }

                // Calculate Tremulant Impact
                let (trem_start_am, trem_end_am) = if let Some(wc_id) = &voice.windchest_group_id {
                    let start = *prev_windchest_mods.get(wc_id).unwrap_or(&1.0);
                    let end = *current_windchest_mods.get(wc_id).unwrap_or(&1.0);
                    (start, end)
                } else {
                    (1.0, 1.0)
                };

                let pitch_start = 1.0 + (trem_start_am - 1.0) * 0.1;
                let pitch_end = 1.0 + (trem_end_am - 1.0) * 0.1;
                let avg_pitch = (pitch_start + pitch_end) * 0.5;

                // Buffer Management (Lazy Compaction)
                let needed_frames_float = buffer_size_frames as f32 * avg_pitch;
                let needed_frames = needed_frames_float.ceil() as usize + 2; // +2 for interpolation safety
                let needed_samples = needed_frames * CHANNEL_COUNT;

                // If the buffer is getting too full/fragmented, compact it now.
                // We keep valid data from buffer_start_idx onwards.
                if voice.buffer_start_idx + needed_samples > voice.input_buffer.capacity() {
                    let remaining = voice.input_buffer.len() - voice.buffer_start_idx;
                    voice.input_buffer.copy_within(voice.buffer_start_idx.., 0);
                    voice.input_buffer.truncate(remaining);
                    voice.buffer_start_idx = 0;
                }

                // Fill Buffer
                let available = voice.consumer.occupied_len() / CHANNEL_COUNT;
                let to_read = available.min(needed_frames * 2);

                if to_read > 0 {
                    let read_samples = to_read * CHANNEL_COUNT;
                    if scratch_read_buffer.len() < read_samples {
                        scratch_read_buffer.resize(read_samples, 0.0);
                    }
                    let _ = voice
                        .consumer
                        .pop_slice(&mut scratch_read_buffer[..read_samples]);
                    voice
                        .input_buffer
                        .extend_from_slice(&scratch_read_buffer[..read_samples]);
                }

                // Check actual available data
                let total_valid_samples = voice.input_buffer.len() - voice.buffer_start_idx;
                if total_valid_samples < needed_samples {
                    if voice.is_finished.load(Ordering::Relaxed) {
                        voices_to_remove.push(*voice_id);
                    }
                    continue;
                }

                // Create a SAFE slice of the valid data we are about to read.
                let input_slice = &voice.input_buffer[voice.buffer_start_idx..];

                // Envelope
                let env_start = voice.fade_level;
                let mut env_end = env_start;
                if voice.is_fading_in {
                    env_end =
                        (env_start + voice.fade_increment * buffer_size_frames as f32).min(1.0);
                    if env_end >= 1.0 {
                        voice.is_fading_in = false;
                    }
                } else if voice.is_fading_out {
                    env_end =
                        (env_start - voice.fade_increment * buffer_size_frames as f32).max(0.0);
                }
                voice.fade_level = env_end;

                let gain_delta =
                    (trem_end_am * env_end - trem_start_am * env_start) / buffer_size_frames as f32;
                let mut current_gain_scalar = trem_start_am * env_start * voice.gain;

                let mix_chunks = mix_buffer.chunks_exact_mut(CHANNEL_COUNT);
                let is_fast_path = (avg_pitch - 1.0).abs() < 0.00001;

                if is_fast_path {
                    // Safe Fast Path
                    // No resampling. We map input samples 1:1 to output samples.
                    let start_offset = voice.cursor_pos.round() as usize * CHANNEL_COUNT;
                    let end_offset = start_offset + buffer_size_frames * CHANNEL_COUNT;

                    // Ensure we don't read past the end (should be covered by needed_samples check, but strict safety requires this)
                    if let Some(valid_chunk) = input_slice.get(start_offset..end_offset) {
                        // ZIP allows the compiler to remove bounds checks and use SIMD
                        for (mix, input_frame) in
                            mix_chunks.zip(valid_chunk.chunks_exact(CHANNEL_COUNT))
                        {
                            // input_frame is guaranteed to have 2 elements [L, R]
                            let l = input_frame[0];
                            let r = input_frame[1];
                            mix[0] += l * current_gain_scalar;
                            mix[1] += r * current_gain_scalar;

                            current_gain_scalar += gain_delta;
                        }
                    }

                    // Advance cursor
                    voice.cursor_pos =
                        (voice.cursor_pos.round() as usize + buffer_size_frames) as f32;
                } else {
                    // Safe Slow Path
                    // Linear Interpolation.
                    let pitch_delta = (pitch_end - pitch_start) / buffer_size_frames as f32;
                    let mut current_pitch_rate = pitch_start;

                    // We hinted to the compiler earlier that we have 'needed_samples'.
                    // This assert helps the optimizer hoist bounds checks out of the loop.
                    assert!(input_slice.len() >= needed_samples);

                    for mix in mix_chunks {
                        let idx = voice.cursor_pos.floor() as usize;
                        let frac = voice.cursor_pos - idx as f32;
                        let idx_stereo = idx * CHANNEL_COUNT;

                        // Standard indexing is safe here.
                        // Because of the assert above, LLVM knows these indices are valid.
                        let s0_l = input_slice[idx_stereo];
                        let s0_r = input_slice[idx_stereo + 1];
                        let s1_l = input_slice[idx_stereo + 2];
                        let s1_r = input_slice[idx_stereo + 3];

                        let out_l = s0_l + (s1_l - s0_l) * frac;
                        let out_r = s0_r + (s1_r - s0_r) * frac;

                        mix[0] += out_l * current_gain_scalar;
                        mix[1] += out_r * current_gain_scalar;

                        voice.cursor_pos += current_pitch_rate;
                        current_gain_scalar += gain_delta;
                        current_pitch_rate += pitch_delta;
                    }
                }

                // Lazy Cleanup
                // Instead of draining, just advance the integer start index
                let samples_consumed_int = voice.cursor_pos.floor() as usize;
                if samples_consumed_int > 0 {
                    // Move the "virtual" start of the buffer forward
                    voice.buffer_start_idx += samples_consumed_int * CHANNEL_COUNT;
                    // Adjust cursor to be relative to the new start
                    voice.cursor_pos -= samples_consumed_int as f32;
                }

                if voice.is_fading_out && voice.fade_level == 0.0 {
                    voices_to_remove.push(*voice_id);
                }
            }

            prev_windchest_mods = current_windchest_mods;

            // Remove voices
            if !voices_to_remove.is_empty() {
                for vid in voices_to_remove.iter() {
                    voices.remove(vid);
                }
                voices_to_remove.clear();
            }

            // Apply Reverb & Global Gain
            let apply_reverb = wet_dry_ratio > 0.0 && convolver.is_loaded;
            if apply_reverb {
                for i in 0..buffer_size_frames {
                    reverb_dry_l[i] = mix_buffer[i * 2];
                    reverb_dry_r[i] = mix_buffer[i * 2 + 1];
                }
                convolver.process(
                    &reverb_dry_l,
                    &reverb_dry_r,
                    &mut wet_buffer_l,
                    &mut wet_buffer_r,
                );
                let dl = (1.0 - wet_dry_ratio) * system_gain;
                let wl = wet_dry_ratio * system_gain;
                for i in 0..buffer_size_frames {
                    mix_buffer[i * 2] = (mix_buffer[i * 2] * dl) + (wet_buffer_l[i] * wl);
                    mix_buffer[i * 2 + 1] = (mix_buffer[i * 2 + 1] * dl) + (wet_buffer_r[i] * wl);
                }
            } else {
                for s in mix_buffer.iter_mut() {
                    *s *= system_gain;
                }
            }

            // Recording & Monitoring
            if let Some(rec) = &mut audio_recorder {
                rec.push(&mix_buffer);
            }

            let duration = start_time.elapsed();
            let load = duration.as_secs_f32() / buffer_duration_secs;
            if load > max_load_accumulator {
                max_load_accumulator = load;
            }

            if last_ui_update.elapsed() >= ui_update_interval {
                let current_voice_count = voices.len();
                if current_voice_count != last_reported_voice_count {
                    let _ = tui_tx.send(TuiMessage::ActiveVoicesUpdate(current_voice_count));
                    last_reported_voice_count = current_voice_count;
                }
                let _ = tui_tx.send(TuiMessage::CpuLoadUpdate(max_load_accumulator));
                max_load_accumulator = 0.0;
                last_ui_update = Instant::now();
            }

            // Push to Audio Driver
            let mut offset = 0;
            let needed = mix_buffer.len();
            while offset < needed {
                if stop_signal.load(Ordering::Relaxed) {
                    log::info!("[AudioThread] Stop signal received. Exiting.");
                    break;
                }
                let pushed = producer.push_slice(&mix_buffer[offset..needed]);
                offset += pushed;
                if offset < needed {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    });
}

pub fn start_audio_playback(
    rx: mpsc::Receiver<AppMessage>,
    organ: Arc<Organ>,
    buffer_size_frames: usize,
    gain: f32,
    polyphony: usize,
    audio_device_name: Option<String>,
    sample_rate: u32,
    tui_tx: mpsc::Sender<TuiMessage>,
    shared_midi_recorder: Arc<Mutex<Option<MidiRecorder>>>,
) -> Result<AudioHandle> {
    // Boilerplate Cpal setup
    let host = get_cpal_host();
    let device: Device = {
        if let Some(name) = audio_device_name {
            host.output_devices()?
                .find(|d| d.id().map_or(false, |n| n.to_string() == name))
                .ok_or_else(|| {
                    anyhow!("Audio device not found: {}. Falling back to default.", name)
                })
                .or_else(|e| {
                    log::warn!("{}", e);
                    host.default_output_device()
                        .ok_or_else(|| anyhow!("No default output device available"))
                })?
        } else {
            host.default_output_device()
                .ok_or_else(|| anyhow!("No default output device available"))?
        }
    };

    log::info!("[Cpal] Using output device: {}", device.id()?.to_string());

    let supported_configs = device.supported_output_configs()?;
    let target_sample_rate = sample_rate;

    let config_range = supported_configs
        .filter(|c| c.sample_format() == SampleFormat::F32 && c.channels() >= 2)
        .find(|c| {
            c.min_sample_rate() <= target_sample_rate && c.max_sample_rate() >= target_sample_rate
        })
        .ok_or_else(|| {
            anyhow!(
                "No supported F32 config found for sample rate {}Hz",
                target_sample_rate
            )
        })?;

    let config = config_range.with_sample_rate(target_sample_rate);
    let sample_format = config.sample_format();
    let stream_config: StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate;
    let device_channels = stream_config.channels as usize;

    let mix_channels = 2;
    let ring_buf_capacity = buffer_size_frames * mix_channels * 10;
    let ring_buf = HeapRb::<f32>::new(ring_buf_capacity);
    let (producer, consumer) = ring_buf.split();
    let stop_signal = Arc::new(AtomicBool::new(false));

    spawn_audio_processing_thread(
        rx,
        producer,
        organ,
        sample_rate,
        buffer_size_frames,
        gain,
        polyphony,
        tui_tx.clone(),
        shared_midi_recorder,
        stop_signal.clone(),
    );

    let err_callback = |err| {
        log::error!("[CpalCallback] Stream error: {}", err);
    };

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(
            &device,
            &stream_config,
            consumer,
            device_channels,
            tui_tx,
            err_callback,
        )?,
        SampleFormat::I16 => build_stream::<i16>(
            &device,
            &stream_config,
            consumer,
            device_channels,
            tui_tx,
            err_callback,
        )?,
        SampleFormat::U16 => build_stream::<u16>(
            &device,
            &stream_config,
            consumer,
            device_channels,
            tui_tx,
            err_callback,
        )?,
        _ => return Err(anyhow!("Unsupported sample format: {:?}", sample_format)),
    };

    stream.play()?;
    Ok(AudioHandle {
        stream,
        stop_signal,
    })
}

// Helper to handle different sample formats (F32, I16, U16) in a generic way
fn build_stream<T>(
    device: &Device,
    config: &StreamConfig,
    mut consumer: impl Consumer<Item = f32> + Send + 'static,
    device_channels: usize,
    tui_tx: mpsc::Sender<TuiMessage>,
    err_fn: impl Fn(cpal::StreamError) + Send + 'static,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let mut stereo_read_buffer: Vec<f32> = Vec::with_capacity(1024);

    let stream = device.build_output_stream(
        config,
        move |output: &mut [T], _: &cpal::OutputCallbackInfo| {
            let out_channels = device_channels;
            let in_channels = 2;
            let frames_to_write = output.len() / out_channels;
            let samples_to_read = frames_to_write * in_channels;

            if stereo_read_buffer.len() < samples_to_read {
                stereo_read_buffer.resize(samples_to_read, 0.0);
            }

            let read_count = consumer.pop_slice(&mut stereo_read_buffer[..samples_to_read]);
            let frames_processed = read_count / in_channels;

            let mut in_idx = 0;
            let mut out_idx = 0;

            for _ in 0..frames_processed {
                let l_f32 = stereo_read_buffer[in_idx + 0];
                let r_f32 = stereo_read_buffer[in_idx + 1];

                output[out_idx + 0] = T::from_sample(l_f32);
                output[out_idx + 1] = T::from_sample(r_f32);

                for ch in 2..out_channels {
                    output[out_idx + ch] = T::from_sample(0.0f32);
                }

                in_idx += in_channels;
                out_idx += out_channels;
            }

            if frames_processed < frames_to_write {
                let silence_start = frames_processed * out_channels;
                for sample in &mut output[silence_start..] {
                    *sample = T::from_sample(0.0f32);
                }
                if frames_processed == 0 {
                    let _ = tui_tx.send(TuiMessage::AudioUnderrun);
                }
            }
        },
        err_fn,
        None,
    )?;

    Ok(stream)
}
