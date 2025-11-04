use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, Stream, StreamConfig};
use decibel::{AmplitudeRatio, DecibelRatio};
use ringbuf::traits::{Observer, Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapRb};
use rubato::{Resampler, FastFixedIn, PolynomialDegree, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::sync::{mpsc, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Instant, Duration};
use rodio::source::Source;
use rodio::Decoder;
use num_traits::cast::ToPrimitive;
use std::path::{Path, PathBuf};
use std::mem;

use crate::app::{ActiveNote, AppMessage};
use crate::organ::Organ;

const BUFFER_SIZE_MS: u32 = 5; 
const CHANNEL_COUNT: usize = 2; // Stereo
const RESAMPLER_CHUNK_SIZE: usize = 512;
/// 2 seconds of stereo, resampled audio.
const VOICE_BUFFER_FRAMES: usize = 48000; 

/// Represents one playing sample, either attack or release.
struct Voice {
    gain: f32, // Linear amplitude
    debug_path: std::path::PathBuf, // For debugging
    
    // The main thread *only* interacts with these:
    consumer: HeapCons<f32>, // <-- Use concrete type HeapCons
    is_finished: Arc<AtomicBool>, // Has the loader thread finished?
    is_cancelled: Arc<AtomicBool>, // Has NoteOff told the loader to stop?
    
    // The loader thread is held so it can be detached
    loader_handle: Option<thread::JoinHandle<()>>,

    fade_level: f32, // 1.0 = full volume, 0.0 = silent
    is_fading_out: bool, // Is the attack sample fading out?
    is_fading_in: bool, // Is the release sample fading in?
}

impl Voice {
    // --- Refactored Voice::new to be non-blocking ---
    fn new(path: &Path, sample_rate: u32, pitch_cents: f32, gain_db: f32, start_fading_in: bool) -> Result<Self> {
        
        let amplitude_ratio: AmplitudeRatio<f64> = DecibelRatio(gain_db as f64).into();
        let gain = amplitude_ratio.amplitude_value() as f32;

        // --- 1. Create the Ring Buffer ---
        let ring_buf = HeapRb::<f32>::new(VOICE_BUFFER_FRAMES * CHANNEL_COUNT);
        let (mut producer, consumer) = ring_buf.split(); // consumer is HeapCons<f32>

        // --- 2. Create communication atomics ---
        let is_finished = Arc::new(AtomicBool::new(false));
        let is_cancelled = Arc::new(AtomicBool::new(false));
        
        // Clone variables to move into the loader thread
        let path_buf = path.to_path_buf();
        let is_finished_clone = Arc::clone(&is_finished);
        let is_cancelled_clone = Arc::clone(&is_cancelled);
        
        // --- 3. Spawn the Loader Thread ---
        // All blocking work now happens inside this thread.
        let loader_handle = thread::spawn(move || {

            // We wrap the fallible logic in a function
            let result: Result<()> = (|| {
                // 3a. Open the file
                let file = File::open(&path_buf)
                    .map_err(|e| anyhow!("[LoaderThread] Failed to open {:?}: {}", path_buf, e))?;
                let reader = BufReader::new(file);
                let mut decoder = Decoder::new_wav(reader)
                    .map_err(|e| anyhow!("[LoaderThread] Failed to decode {:?}: {}", path_buf, e))?;

                // 3b. Create the resampler
                let input_sample_rate = decoder.sample_rate();
                let input_channels = decoder.channels() as usize;
                let is_mono = input_channels == 1;

                let pitch_factor = 2.0f64.powf(pitch_cents as f64 / 1200.0);
                let effective_input_rate = input_sample_rate as f64 / pitch_factor;
                let resample_ratio = sample_rate as f64 / effective_input_rate;
                
                let mut resampler = FastFixedIn::<f32>::new(
                    resample_ratio, 1.01, PolynomialDegree::Septic, RESAMPLER_CHUNK_SIZE, CHANNEL_COUNT,
                ).map_err(|e| anyhow!("[LoaderThread] Failed to create resampler for {:?}: {}", path_buf, e))?;

                // 3c. Create buffers (local to this thread)
                let max_input_frames = resampler.input_frames_max();
                let mut input_buffer = vec![vec![0.0f32; max_input_frames]; CHANNEL_COUNT];
                let max_output_frames = resampler.output_frames_max();
                let mut output_buffer = vec![vec![0.0f32; max_output_frames]; CHANNEL_COUNT];
                let mut interleaved_output = vec![0.0f32; max_output_frames * CHANNEL_COUNT];
                
                let mut source = decoder.filter_map(|s| s.to_f32());
                let mut source_is_finished = false;
                
                // --- 3d. The (Corrected) Loader Loop ---
                'loader_loop: loop {
                    // Check if NoteOff was called
                    if is_cancelled_clone.load(Ordering::Relaxed) {
                        break 'loader_loop;
                    }
                    
                    let input_frames_needed = resampler.input_frames_next();
                    
                    // Clear input buffers
                    for buf in input_buffer.iter_mut() { buf.clear(); }
                    
                    let mut frames_read = 0;
                    if input_frames_needed > 0 && !source_is_finished {
                        for _ in 0..input_frames_needed {
                            if let Some(sample_l) = source.next() {
                                input_buffer[0].push(sample_l);
                                if is_mono {
                                    input_buffer[1].push(sample_l);
                                } else {
                                    if let Some(sample_r) = source.next() {
                                        input_buffer[1].push(sample_r);
                                    } else {
                                        input_buffer[1].push(sample_l); // Fallback
                                        source_is_finished = true;
                                        frames_read += 1;
                                        break;
                                    }
                                }
                            } else {
                                source_is_finished = true;
                                break;
                            }
                            frames_read += 1;
                        }
                    }
                    
                    let in_buf_slices: Vec<&[f32]> = input_buffer.iter().map(|v| v.as_slice()).collect();
                    let mut out_buf_slices: Vec<&mut [f32]> = output_buffer.iter_mut().map(|v| v.as_mut_slice()).collect();

                    let (frames_consumed, frames_produced) = if source_is_finished {
                        if frames_read > 0 {
                             resampler.process_partial_into_buffer(Some(&in_buf_slices), &mut out_buf_slices, None)?
                        } else {
                            break 'loader_loop;
                        }
                    } else if frames_read > 0 { 
                        resampler.process_into_buffer(&in_buf_slices, &mut out_buf_slices, None)?
                    } else {
                        let empty_input: [Vec<f32>; CHANNEL_COUNT] = [vec![], vec![]];
                        let empty_slices: Vec<&[f32]> = empty_input.iter().map(|v| v.as_slice()).collect();
                        resampler.process_partial_into_buffer(Some(&empty_slices), &mut out_buf_slices, None)?
                    };

                    if frames_produced > 0 {
                        // Interleave the output
                        for i in 0..frames_produced {
                            interleaved_output[i * CHANNEL_COUNT] = output_buffer[0][i];
                            interleaved_output[i * CHANNEL_COUNT + 1] = output_buffer[1][i];
                        }
                        
                        // Push to the ring buffer (this will block if the buffer is full)
                        let needed = frames_produced * CHANNEL_COUNT;
                        let mut offset = 0usize;
                        while offset < needed {
                            let pushed = producer.push_slice(&interleaved_output[offset..needed]);
                            offset += pushed;
                            if offset < needed {
                                if is_cancelled_clone.load(Ordering::Relaxed) {
                                    break;
                                }
                                thread::sleep(Duration::from_millis(1));
                            }
                        }
                    }
                } // --- End of 'loader_loop ---

                // --- 3e. Flush the resampler ---
                'flush_loop: loop {
                    if is_cancelled_clone.load(Ordering::Relaxed) {
                        break 'flush_loop;
                    }
                    let mut out_buf_slices: Vec<&mut [f32]> = output_buffer.iter_mut().map(|v| v.as_mut_slice()).collect();
                    
                    let (frames_consumed, frames_produced) = resampler.process_partial_into_buffer(None::<&[&[f32]]>, &mut out_buf_slices, None)?;

                    if frames_produced > 0 {
                        // Interleave and push
                        for i in 0..frames_produced {
                            interleaved_output[i * CHANNEL_COUNT] = output_buffer[0][i];
                            interleaved_output[i * CHANNEL_COUNT + 1] = output_buffer[1][i];
                        }

                        let needed = frames_produced * CHANNEL_COUNT;
                        let mut offset = 0usize;
                        while offset < needed {
                            let pushed = producer.push_slice(&interleaved_output[offset..needed]);
                            offset += pushed;
                            if offset < needed {
                                if is_cancelled_clone.load(Ordering::Relaxed) {
                                    break;
                                }
                                thread::sleep(Duration::from_millis(1));
                            }
                        }
                    } else {
                        break 'flush_loop;
                    }
                }
                
                Ok(()) // Success
            })(); // Immediately invoke the closure
            
            // --- 4. Handle result and ALWAYS set finished ---
            if let Err(e) = result {
                log::error!("{}", e);
            }
            
            // Signal to the main thread that we are done, no matter what.
            is_finished_clone.store(true, Ordering::SeqCst);
        });

        // --- 4. Return the non-blocking Voice struct ---
        Ok(Self {
            gain,
            debug_path: path.to_path_buf(),
            consumer,
            is_finished,
            is_cancelled,
            loader_handle: Some(loader_handle),
            fade_level: if start_fading_in { 0.0 } else { 1.0 }, 
            is_fading_out: false,
            is_fading_in: start_fading_in,
        })
    }
}

// This stops the audio thread from blocking when a voice is dropped.
impl Drop for Voice {
    fn drop(&mut self) {
        // Tell the loader thread to stop, just in case
        self.is_cancelled.store(true, Ordering::SeqCst);
        if let Some(handle) = self.loader_handle.take() {
            mem::forget(handle);
        }
    }
}


/// Spawns the dedicated audio processing thread.
fn spawn_audio_processing_thread<P>(
    rx: mpsc::Receiver<AppMessage>,
    mut producer: P,
    organ: Arc<Organ>,
    sample_rate: u32,
    buffer_size_frames: usize,
) where
    P: Producer<Item = f32> + Send + 'static,
{
    thread::spawn(move || {
        let mut active_stops: BTreeSet<usize> = BTreeSet::new();
        let mut active_notes: HashMap<u8, Vec<ActiveNote>> = HashMap::new();
        let mut voices: HashMap<u64, Voice> = HashMap::with_capacity(128);
        let mut voice_counter: u64 = 0;
        // Buffers for processing
        let mut mix_buffer_stereo: [Vec<f32>; CHANNEL_COUNT] = [
            vec![0.0; buffer_size_frames],
            vec![0.0; buffer_size_frames],
        ];
        
        let mut interleaved_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT];
        
        // --- This buffer is for popping from the voice's ringbuf ---
        let mut voice_read_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT];

        let mut tmp_drain_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT];

        let mut loop_counter: u64 = 0;

        loop {
            // --- 1. Handle incoming messages ---
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    AppMessage::NoteOn(note, _vel) => {
                        if _vel > 0 {
                            let mut new_notes = Vec::new();
                            for stop_index in &active_stops {
                                let stop = &organ.stops[*stop_index];
                                for rank_id in &stop.rank_ids {
                                    if let Some(rank) = organ.ranks.get(rank_id) {
                                        if let Some(pipe) = rank.pipes.get(&note) {
                                            let total_gain = rank.gain_db + pipe.gain_db;
                                            // Play attack sample
                                            match Voice::new(
                                                &pipe.attack_sample_path,
                                                sample_rate,
                                                pipe.pitch_tuning_cents,
                                                total_gain,
                                                false,
                                            ) {
                                                Ok(voice) => {
                                                    let voice_id = voice_counter;
                                                    voice_counter += 1;
                                                    voices.insert(voice_id, voice);

                                                    new_notes.push(ActiveNote {
                                                        note,
                                                        start_time: Instant::now(),
                                                        stop_index: *stop_index,
                                                        rank_id: rank_id.clone(),
                                                        voice_id,
                                                    });
                                                }
                                                Err(e) => {
                                                    log::error!("[AudioThread] Error creating attack voice: {}", e)
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if !new_notes.is_empty() {
                                active_notes.insert(note, new_notes);
                            }
                        } else {
                            handle_note_off(
                                note, &organ, &mut voices, &mut active_notes,
                                sample_rate, &mut voice_counter,
                            );
                        }
                    }
                    AppMessage::NoteOff(note) => {
                        handle_note_off(
                            note, &organ, &mut voices, &mut active_notes,
                            sample_rate, &mut voice_counter,
                        );
                    }
                    AppMessage::StopToggle(stop_index, is_active) => {
                        if is_active {
                            active_stops.insert(stop_index);
                        } else {
                            active_stops.remove(&stop_index);
                        }
                    }
                    AppMessage::Quit => {
                        return; // Exit thread
                    }
                }
            }

            // --- 2. Process all active voices ---
            // Clear mix buffer
            for ch_buf in mix_buffer_stereo.iter_mut() {
                ch_buf.fill(0.0);
            }

            let mut max_abs_sample = 0.0f32;

            // --- mixing loop ---
            // 10ms fade-out
            let fade_frames = (sample_rate as f32 * 0.10) as usize; 
            let fade_increment = if fade_frames > 0 { 1.0 / fade_frames as f32 } else { 1.0 };

            voices.retain(|_voice_id, voice| {
                let is_loader_finished = voice.is_finished.load(Ordering::Relaxed);
                let mut is_buffer_empty = voice.consumer.is_empty();

                let frames_to_read = buffer_size_frames;
                let samples_to_read = frames_to_read * CHANNEL_COUNT;
                
                let samples_read = voice.consumer.pop_slice(&mut voice_read_buffer[..samples_to_read]);
                let frames_read = samples_read / CHANNEL_COUNT;

                // --- Mix all samples, applying crossfade logic ---
                for i in 0..frames_read {
                    
                    // --- FADE LOGIC ---
                    if voice.is_fading_in {
                        voice.fade_level += fade_increment;
                        if voice.fade_level >= 1.0 {
                            voice.fade_level = 1.0;
                            voice.is_fading_in = false;
                        }
                    } else if voice.is_fading_out {
                        voice.fade_level -= fade_increment; // Use same value
                        if voice.fade_level <= 0.0 {
                            voice.fade_level = 0.0;
                        }
                    }
                    // --- END FADE LOGIC ---

                    // Final gain is the voice's gain * its fade level
                    let current_gain = voice.gain * voice.fade_level;

                    let l_sample = voice_read_buffer[i * CHANNEL_COUNT] * current_gain;
                    let r_sample = voice_read_buffer[i * CHANNEL_COUNT + 1] * current_gain;
                    
                    mix_buffer_stereo[0][i] += l_sample;
                    mix_buffer_stereo[1][i] += r_sample;
                    
                    if l_sample.abs() > max_abs_sample {
                        max_abs_sample = l_sample.abs();
                    }
                }

                // --- Decide whether to keep the voice ---
                let is_faded_out = voice.is_fading_out && voice.fade_level == 0.0;

                if is_faded_out && !is_buffer_empty {
                    // This voice is silent, but its buffer still has data.
                    // We must drain it completely so 'is_buffer_empty'
                    // becomes true and it can be collected.
                    while voice.consumer.pop_slice(&mut tmp_drain_buffer) > 0 {
                        // Draining...
                    }
                    is_buffer_empty = true; // Update status after drain
                }
                
                let is_done_playing = is_loader_finished && is_buffer_empty;

                if is_done_playing && (is_faded_out || !voice.is_fading_out) {
                    // Remove if:
                    // 1. It's finished loading AND its buffer is empty
                    // 2. AND (it's faded out OR it was never supposed to fade out)
                    return false; // Remove the voice
                }
                
                return true; // Keep the voice
            });


            // if max_abs_sample > 0.001 {
            //     log::debug!("[AudioThread] Loop complete. Active voices: {}. Max sample: {:.4}", voices.len(), max_abs_sample);
            // }

            // --- 3. Interleave and push to ring buffer ---
            for i in 0..buffer_size_frames {
                interleaved_buffer[i * CHANNEL_COUNT] = mix_buffer_stereo[0][i];
                interleaved_buffer[i * CHANNEL_COUNT + 1] = mix_buffer_stereo[1][i];
            }

            let mut offset = 0;
            let needed = interleaved_buffer.len();
            while offset < needed {
                // push_slice returns the number of samples *actually* pushed
                let pushed = producer.push_slice(&interleaved_buffer[offset..needed]);
                offset += pushed;

                // If we didn't push everything, the buffer is full.
                // We must sleep to yield to the consumer (cpal callback).
                if offset < needed {
                    // Sleep for a very short duration. 1ms is a good
                    // compromise. It's much better than a 100% CPU spin.
                    thread::sleep(Duration::from_millis(1));
                }
            }

            loop_counter += 1;
            if loop_counter % 100 == 0 { // Log approx. every 3 seconds
                let total_voice_buffered: usize = voices.values().map(|v| v.consumer.occupied_len()).sum();
                log::debug!(
                    "[AudioThread] STATUS: Loop {}. Active voices: {}. Total buffered samples: {}. Main ringbuf: {}/{}",
                    loop_counter,
                    voices.len(),
                    total_voice_buffered,
                    producer.occupied_len(),
                    producer.capacity()
                );
            }
        }
    });
}

/// Helper function to handle Note Off logic
fn handle_note_off(
    note: u8,
    organ: &Arc<Organ>,
    voices: &mut HashMap<u64, Voice>,
    active_notes: &mut HashMap<u8, Vec<ActiveNote>>,
    sample_rate: u32,
    voice_counter: &mut u64,
) {
    if let Some(notes_to_stop) = active_notes.remove(&note) {
        let press_duration = notes_to_stop
            .get(0)
            .map(|n| n.start_time.elapsed().as_millis() as i64)
            .unwrap_or(0);

        for stopped_note in notes_to_stop {
            // Tell the voice to cancel, but don't drop it yet ---
            if let Some(voice) = voices.get_mut(&stopped_note.voice_id) {
                log::debug!("[AudioThread] ...stopping attack voice ID {}", stopped_note.voice_id);
                 voice.is_cancelled.store(true, Ordering::SeqCst);
                 voice.is_fading_out = true;
            }
            // The `retain` loop will drop it once its buffer is empty.
            
            if let Some(rank) = organ.ranks.get(&stopped_note.rank_id) {
                if let Some(pipe) = rank.pipes.get(&note) {
                    // Find the correct release sample
                    let release_sample = pipe
                        .releases
                        .iter()
                        .find(|r| {
                            r.max_key_press_time_ms == -1
                                || press_duration <= r.max_key_press_time_ms
                        })
                        .or_else(|| pipe.releases.last()); // Fallback to last

                    if let Some(release) = release_sample {
                        let total_gain = rank.gain_db + pipe.gain_db;
                        // Play release sample
                        match Voice::new(
                            &release.path,
                            sample_rate,
                            pipe.pitch_tuning_cents,
                            total_gain,
                            true,
                        ) {
                            Ok(voice) => {
                                log::debug!("[AudioThread] -> Created RELEASE Voice for {:?} (Duration: {}ms, Gain: {:.2}dB)",
                                  release.path.file_name().unwrap_or_default(), press_duration, total_gain);
                                let release_voice_id = *voice_counter;
                                *voice_counter += 1;
                                voices.insert(release_voice_id, voice);
                            }
                            Err(e) => {
                                log::error!("[AudioThread] Error creating release sample: {}", e)
                            }
                        }
                    } else {
                        log::warn!("[AudioThread] ...but no release sample found for pipe on note {}.", note);
                    }
                }
            }
        }
    } else {
        log::warn!("[AudioThread] ...but no active notes found for note {}.", note);
    }
}

/// Sets up the cpal audio stream and spawns the processing thread.
pub fn start_audio_playback(rx: mpsc::Receiver<AppMessage>, organ: Arc<Organ>) -> Result<Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("No default output device available"))?;

    println!(
        "[Cpal] Default output device: {}",
        device.name().unwrap_or_else(|_| "Unknown".to_string())
    );

    // Find a supported config
    let mut supported_configs = device.supported_output_configs()?;

    // Show all supported configs
    // println!("[Cpal] Supported output configs:");
    // for config in device.supported_output_configs()? {
    //     println!(
    //         "  - Channels: {}, Sample Rate: {}-{}, Format: {:?}",
    //         config.channels(),
    //         config.min_sample_rate().0,
    //         config.max_sample_rate().0,
    //         config.sample_format()
    //     );
    // }

    let config = supported_configs
        .find(|c| c.channels() == 2 && c.sample_format() == SampleFormat::F32)
        .ok_or_else(|| anyhow!("No supported F32 stereo config found"))?
        .with_sample_rate(SampleRate(48000));

    let sample_format = config.sample_format();
    let stream_config: StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    println!(
        "[Cpal] Using config: SampleRate: {}, Channels: {}, Format: {:?}",
        sample_rate, channels, sample_format
    );

    // Calculate buffer size in frames
    let buffer_size_frames = (sample_rate * BUFFER_SIZE_MS / 1000) as usize;

    // Create the ring buffer
    let ring_buf_capacity = buffer_size_frames * channels * 10;
    let ring_buf = HeapRb::<f32>::new(ring_buf_capacity);
    println!(
        "[Cpal] Ring buffer created with capacity for {} frames.",
        ring_buf_capacity / channels
    );
    let (producer, mut consumer) = ring_buf.split();

    // Spawn the audio processing thread
    spawn_audio_processing_thread(rx, producer, organ, sample_rate, buffer_size_frames);

    // --- The cpal audio callback ---
    let data_callback = move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let frames_to_write = output.len() / channels;
        let frames_available = consumer.occupied_len() / channels;

        let frames_to_take = frames_to_write.min(frames_available);

        if frames_to_take > 0 {
            let samples_to_take = frames_to_take * channels;
            let samples_popped = consumer.pop_slice(&mut output[..samples_to_take]);
            if samples_popped < samples_to_take {
                for sample in &mut output[samples_popped..samples_to_take] {
                    *sample = 0.0;
                }
            }
            if output[0].abs() > 0.001 {
                log::debug!("[CpalCallback] Consumed {} frames ({} samples). First sample: {:.4}", frames_to_take, samples_popped, output[0]);
            }
        }

        // Fill remaining buffer with silence if we underrun
        if frames_to_take < frames_to_write {
            let silence_start_index = frames_to_take * channels;
            for sample in &mut output[silence_start_index..] {
                *sample = 0.0;
            }
            if frames_available > 0 {
                // This is a real underrun
                log::warn!("[CpalCallback] Audio buffer underrun! Wrote {} silent frames.", frames_to_write - frames_to_take);
            }
        }
    };

    let err_callback = |err| {
        log::error!("[CpalCallback] Stream error: {}", err);
    };

    // Build and play the stream
    let stream = match sample_format {
        SampleFormat::F32 => {
            device.build_output_stream(&stream_config, data_callback, err_callback, None)?
        }
        _ => return Err(anyhow!("Unsupported sample format")),
    };

    stream.play()?;
    Ok(stream)
}

