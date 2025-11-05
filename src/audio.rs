use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, Stream, StreamConfig};
use decibel::{AmplitudeRatio, DecibelRatio};
use ringbuf::traits::{Observer, Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapRb};
use rubato::{Resampler, FastFixedIn, PolynomialDegree};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom, Read, Cursor};
use std::sync::{mpsc, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Instant, Duration};
use std::path::PathBuf;
use rodio::source::Source;
use rodio::Decoder;
use num_traits::cast::ToPrimitive;
use std::path::{Path};
use std::mem;
use byteorder::{ReadBytesExt as OtherReadBytesExt, LittleEndian}; 

use crate::app::{ActiveNote, AppMessage};
use crate::organ::Organ;

const BUFFER_SIZE_MS: u32 = 10; 
const CHANNEL_COUNT: usize = 2; // Stereo
const RESAMPLER_CHUNK_SIZE: usize = 512;
const VOICE_BUFFER_FRAMES: usize = 14400; 
const GAIN_FACTOR: f32 = 0.6; // Prevent clipping when multiple voices mix
const CROSSFADE_TIME: f32 = 0.20; // How long to crossfade from attack to release samples, in seconds

/// Parses a 'smpl' chunk's data. Returns (loop_start, loop_end) in samples.
fn parse_smpl_chunk(data: &[u8]) -> Option<(u32, u32)> {
    // A 'smpl' chunk has a 36-byte header, followed by an array of loops.
    // Each loop entry is 24 bytes.
    if data.len() < 36 {
        log::warn!("[parse_smpl_chunk] 'smpl' data is too short for header: {} bytes", data.len());
        return None;
    }
    let mut cursor = Cursor::new(data);
    
    // Seek to num_sample_loops (offset 28)
    if cursor.seek(SeekFrom::Start(28)).is_err() {
        return None; // Should not happen
    }
    let num_sample_loops = match cursor.read_u32::<LittleEndian>() {
        Ok(n) => n,
        Err(e) => {
            log::warn!("[parse_smpl_chunk] Failed to read num_sample_loops: {}", e);
            return None;
        }
    };

    if num_sample_loops == 0 {
        log::trace!("[parse_smpl_chunk] File has 'smpl' chunk but 0 loops.");
        return None;
    }

    // Seek to start of first loop entry (offset 36)
    if cursor.seek(SeekFrom::Start(36)).is_err() {
        return None;
    }
    
    if data.len() < 36 + 24 {
        log::warn!("[parse_smpl_chunk] 'smpl' data is too short for one loop entry: {} bytes", data.len());
        return None;
    }

    // We only care about the first loop.
    let _cue_point_id = cursor.read_u32::<LittleEndian>().ok()?;
    let _loop_type = cursor.read_u32::<LittleEndian>().ok()?; // 0 = forward, 1 = alternating, 2 = backward
    let loop_start = cursor.read_u32::<LittleEndian>().ok()?;
    let loop_end = cursor.read_u32::<LittleEndian>().ok()?; // This is the *sample after* the loop
    let _fraction = cursor.read_u32::<LittleEndian>().ok()?;
    let _play_count = cursor.read_u32::<LittleEndian>().ok()?; // 0 = infinite
    
    log::debug!("[parse_smpl_chunk] Found loop: {} -> {}", loop_start, loop_end);

    // The 'end' sample is exclusive, so `loop_end - 1` is the last sample.
    // We'll use a check `current_frame >= loop_end`
    Some((loop_start, loop_end))
}

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
    fn new(path: &Path, sample_rate: u32, pitch_cents: f32, gain_db: f32, start_fading_in: bool, is_attack_sample: bool) -> Result<Self> {
        
        let amplitude_ratio: AmplitudeRatio<f64> = DecibelRatio(gain_db as f64).into();
        let gain = amplitude_ratio.amplitude_value() as f32;

        // --- Create the Ring Buffer ---
        let ring_buf = HeapRb::<f32>::new(VOICE_BUFFER_FRAMES * CHANNEL_COUNT);
        let (mut producer, consumer) = ring_buf.split(); // consumer is HeapCons<f32>

        // --- Create communication atomics ---
        let is_finished = Arc::new(AtomicBool::new(false));
        let is_cancelled = Arc::new(AtomicBool::new(false));
        let is_attack_sample_clone = is_attack_sample;
        
        // Clone variables to move into the loader thread
        let path_buf = path.to_path_buf();
        let is_finished_clone = Arc::clone(&is_finished);
        let is_cancelled_clone = Arc::clone(&is_cancelled);
        
        // --- Spawn the Loader Thread ---
        let loader_handle = thread::spawn(move || {
            let path_buf_clone = path_buf.clone();
            let path_str = path_buf_clone.file_name().unwrap_or_default().to_string_lossy();
            let path_str_clone = path_str.clone();
            log::trace!("[LoaderThread] START: {:?}", path_str);
            
            // --- Use catch_unwind to handle ALL panics ---
            let panic_result = std::panic::catch_unwind(move || {

                let mut loader_loop_counter = 0u64;
                let mut log_throttle = 0u64;
                let mut cancelled_log_sent = false;

                // This inner closure contains all the fallible logic
                let result: Result<()> = (|| {
                    // Open the file
                    let file = File::open(&path_buf.clone())
                        .map_err(|e| anyhow!("[LoaderThread] Failed to open {:?}: {}", path_buf.clone(), e))?;
                    let mut reader = BufReader::new(file);

                    // --- 1. Manually parse for 'smpl' chunk if it's an attack sample ---
                    let mut loop_info: Option<(u32, u32)> = None;

                    if is_attack_sample_clone {
                        log::trace!("[LoaderThread] Scanning for 'smpl' chunk in {:?}", path_str);
                        // This block will read headers, find 'smpl', then rewind.
                        // We wrap it in a Result to easily handle IO errors.
                        let parse_result: Result<()> = (|| {
                            let mut riff_header = [0; 4];
                            reader.read_exact(&mut riff_header)?;
                            if &riff_header != b"RIFF" {
                                return Err(anyhow!("Not a RIFF file"));
                            }
                            let _file_size = reader.read_u32::<LittleEndian>()?;
                            let mut wave_header = [0; 4];
                            reader.read_exact(&mut wave_header)?;
                            if &wave_header != b"WAVE" {
                                return Err(anyhow!("Not a WAVE file"));
                            }

                            'chunk_loop: while let Ok(chunk_id) = reader.read_u32::<LittleEndian>().map(|id| id.to_le_bytes()) {
                                let chunk_size = reader.read_u32::<LittleEndian>()?;
                                let chunk_data_start_pos = reader.stream_position()?;
                                let next_chunk_aligned_pos =
                                    chunk_data_start_pos + (chunk_size as u64 + (chunk_size % 2) as u64);

                                match &chunk_id {
                                    b"smpl" => {
                                        log::trace!("[LoaderThread] Found 'smpl' chunk in {:?}", path_str);
                                        let mut chunk_data = vec![0; chunk_size as usize];
                                        reader.read_exact(&mut chunk_data)?;
                                        loop_info = parse_smpl_chunk(&chunk_data);
                                    }
                                    b"data" => {
                                        // Found data, we can stop. 'smpl' chunk (if it exists)
                                        // almost always comes before 'data'.
                                        break 'chunk_loop;
                                    }
                                    _ => {
                                        // Other chunk, skip it
                                    }
                                }
                                // Seek to the *start* of the next chunk
                                reader.seek(SeekFrom::Start(next_chunk_aligned_pos))?;
                            }
                            Ok(())
                        })(); // End of header parse closure
                        
                        if let Err(e) = parse_result {
                            log::warn!("[LoaderThread] Header parse failed for {:?}: {}. Proceeding without loop.", path_str, e);
                            loop_info = None;
                        }
                        
                        // --- REWIND the reader for rodio::Decoder ---
                        reader.seek(SeekFrom::Start(0))
                            .map_err(|e| anyhow!("[LoaderThread] Failed to rewind reader for {:?}: {}", path_buf.clone(), e))?;
                    }

                    let decoder = Decoder::new_wav(reader)
                        .map_err(|e| anyhow!("[LoaderThread] Failed to decode {:?}: {}", path_buf.clone(), e))?;

                    // Create the resampler
                    let input_sample_rate = decoder.sample_rate();
                    let input_channels = decoder.channels() as usize;
                    let is_mono = input_channels == 1;

                    let pitch_factor = 2.0f64.powf(pitch_cents as f64 / 1200.0);
                    let effective_input_rate = input_sample_rate as f64 / pitch_factor;
                    let resample_ratio = sample_rate as f64 / effective_input_rate;
                    
                    let mut resampler = FastFixedIn::<f32>::new(
                        resample_ratio, 1.01, PolynomialDegree::Septic, RESAMPLER_CHUNK_SIZE, CHANNEL_COUNT,
                    ).map_err(|e| anyhow!("[LoaderThread] Failed to create resampler for {:?}: {}", path_buf.clone(), e))?;

                    // Create buffers (local to this thread)
                    let max_input_frames = resampler.input_frames_max();
                    let mut input_buffer = vec![vec![0.0f32; max_input_frames]; CHANNEL_COUNT];
                    let max_output_frames = resampler.output_frames_max();
                    let mut output_buffer = vec![vec![0.0f32; max_output_frames]; CHANNEL_COUNT];
                    let mut interleaved_output = vec![0.0f32; max_output_frames * CHANNEL_COUNT];
                    
                    let mut source: Option<Box<dyn Iterator<Item = f32>>> =
                        Some(Box::new(decoder.filter_map(|s| s.to_f32())));
                    let mut source_is_finished = false;

                    // Variables for looping attack samples
                    let mut samples_in_memory: Vec<f32> = Vec::new(); 
                    let mut current_frame_index: usize = 0;
                    let mut loop_start_frame: usize = 0;
                    let mut loop_end_frame: usize = 0;
                    
                    let mut is_looping_sample = is_attack_sample_clone && loop_info.is_some();
                    let mut use_memory_reader = false;
                    
                    if is_looping_sample {
                        log::debug!("[LoaderThread] Reading {:?} into memory for looping.", path_str);
                        // --- Read ALL samples into memory ---
                        samples_in_memory = source.take().unwrap().collect();
                        use_memory_reader = true;
                        source_is_finished = true; // The 'source' iterator is now consumed
                        
                        let (start, end) = loop_info.unwrap();
                        loop_start_frame = start as usize;
                        let total_frames = samples_in_memory.len() / input_channels;
                        
                        // 'end' is exclusive. 0 often means 'end of file'.
                        loop_end_frame = if end == 0 { total_frames } else { end as usize };

                        // Sanity check loop points
                        if loop_start_frame >= loop_end_frame || loop_end_frame > total_frames {
                            log::warn!(
                                "[LoaderThread] Invalid loop points for {:?}: start {}, end {}, total {}. Disabling loop.",
                                path_str, loop_start_frame, loop_end_frame, total_frames
                            );
                            is_looping_sample = false; // It's now a one-shot, but still from memory
                            current_frame_index = 0; // Reset index to play from start
                        } else {
                            log::debug!(
                                "[LoaderThread] Loop active for {:?}: {} -> {} ({} frames)",
                                path_str, loop_start_frame, loop_end_frame, total_frames
                            );
                        }
                    }

                    // --- The Loader Loop ---
                    'loader_loop: loop {
                        loader_loop_counter += 1;

                        if is_cancelled_clone.load(Ordering::Relaxed) {
                            if !cancelled_log_sent {
                                log::trace!("[LoaderThread] CANCELLED: {:?} (in loader_loop)", path_str);
                                cancelled_log_sent = true;
                            }
                            break 'loader_loop;
                        }

                        log_throttle += 1;
                        if log_throttle % 100 == 0 { // Log every 100 iterations
                            log::trace!("[LoaderThread] ALIVE: {:?} (Loop {})", path_str, loader_loop_counter);
                        }

                        // Get frames needed by resampler
                        let input_frames_needed = resampler.input_frames_next();
                        let mut frames_read = 0;

                        if use_memory_reader {
                            // --- READING FROM MEMORY (Looping OR One-Shot) ---
                            for _ in 0..input_frames_needed {
                                if is_looping_sample {
                                    // Check for loop point
                                    if current_frame_index >= loop_end_frame {
                                        current_frame_index = loop_start_frame;
                                    }
                                } else {
                                    // One-shot from memory
                                    if current_frame_index >= (samples_in_memory.len() / input_channels) {
                                        source_is_finished = true; // True EOF
                                        break; // Stop adding frames
                                    }
                                }
                                
                                let sample_l_idx = current_frame_index * input_channels;
                                let sample_l = samples_in_memory.get(sample_l_idx).cloned().unwrap_or(0.0);
                                let sample_r = if is_mono {
                                    sample_l
                                } else {
                                    samples_in_memory.get(sample_l_idx + 1).cloned().unwrap_or(0.0)
                                };
                                
                                input_buffer[0].push(sample_l);
                                input_buffer[1].push(sample_r);
                                current_frame_index += 1;
                                frames_read += 1; // Increment frames *read*
                            }
                        } else {
                            // --- ONE-SHOT LOGIC (streaming from Decoder) ---
                            // This branch is only entered if `use_memory_reader` is false,
                            // meaning `source.take()` was never called, so `source` is `Some`.
                            if input_frames_needed > 0 && !source_is_finished {
                                // The borrow checker is happy because `if let Some`
                                // proves `source` is still valid.
                                if let Some(ref mut s_iter) = source {
                                    for _ in 0..input_frames_needed {
                                        if let Some(sample_l) = s_iter.next() {
                                            input_buffer[0].push(sample_l);
                                            if is_mono {
                                                input_buffer[1].push(sample_l);
                                            } else {
                                                if let Some(sample_r) = s_iter.next() {
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
                                } else {
                                    // This should not happen if use_memory_reader is false,
                                    // but it's good to be safe.
                                    source_is_finished = true;
                                }
                            }
                        }
                        
                        // Process the data
                        let in_buf_slices: Vec<&[f32]> = input_buffer.iter().map(|v| v.as_slice()).collect();
                        let mut out_buf_slices: Vec<&mut [f32]> = output_buffer.iter_mut().map(|v| v.as_mut_slice()).collect();

                        let (_frames_consumed, frames_produced) = if source_is_finished {
                            if frames_read > 0 {
                                resampler.process_partial_into_buffer(Some(&in_buf_slices), &mut out_buf_slices, None)?
                            } else {
                                break 'loader_loop;
                            }
                        } else if frames_read > 0 { 
                            resampler.process_into_buffer(&in_buf_slices, &mut out_buf_slices, None)?
                        } else {
                            // No frames read. Either we need 0, or we're at EOF.
                            // Call process_partial_into_buffer to flush output.
                            let empty_input: [Vec<f32>; CHANNEL_COUNT] = [vec![], vec![]];
                            let empty_slices: Vec<&[f32]> = empty_input.iter().map(|v| v.as_slice()).collect();
                            resampler.process_partial_into_buffer(Some(&empty_slices), &mut out_buf_slices, None)?
                        };

                        // Push to buffer
                        if frames_produced > 0 {
                            for i in 0..frames_produced {
                                interleaved_output[i * CHANNEL_COUNT] = output_buffer[0][i];
                                interleaved_output[i * CHANNEL_COUNT + 1] = output_buffer[1][i];
                            }
                            
                            let needed = frames_produced * CHANNEL_COUNT;
                            let mut offset = 0usize;
                            while offset < needed {
                                if is_cancelled_clone.load(Ordering::Relaxed) {
                                    if !cancelled_log_sent {
                                        log::trace!("[LoaderThread] CANCELLED: {:?} (in push_loop)", path_str);
                                        cancelled_log_sent = true;
                                    }
                                    break 'loader_loop; 
                                }
                                let pushed = producer.push_slice(&interleaved_output[offset..needed]);
                                offset += pushed;
                                if offset < needed {
                                    thread::sleep(Duration::from_millis(1));
                                }
                            }
                        }

                        // Decide to sleep or exit
                        if is_looping_sample {
                            // For a looping sample, we NEVER exit the loop unless cancelled.
                            // We just sleep if the resampler or ringbuf is full.
                            if input_frames_needed == 0 && frames_produced == 0 {
                                // Resampler input is full, and output is full.
                                // We *must* sleep to wait for the mixer.
                                thread::sleep(Duration::from_millis(1));
                            }
                        } else {
                            // --- exit logic for one-shot samples ---
                            if source_is_finished && frames_produced == 0 && resampler.output_frames_next() == 0 {
                                // File is done, nothing was produced, and resampler has no more frames.
                                // We are 100% finished.
                                log::trace!("[LoaderThread] FINISHED_SOURCE_AND_RESAMPLER: {:?}", path_str);
                                break 'loader_loop;
                            }
                            
                            if input_frames_needed == 0 && frames_produced == 0 {
                                // Resampler input is full, and output is full.
                                // We *must* sleep to wait for the mixer.
                                thread::sleep(Duration::from_millis(1));
                            }
                        }
                        
                        // --- Clear input buffers for next loop ---
                        for buf in input_buffer.iter_mut() { buf.clear(); }

                    } // --- End of 'loader_loop ---

                    log::trace!("[LoaderThread] EXITED_MAIN_LOOP: {:?}", path_str);

                    let mut flush_loop_counter = 0u64;

                    // --- Flush the resampler ---
                    'flush_loop: loop {
                        flush_loop_counter += 1;
                        if flush_loop_counter > 100 { // 100 loops is *more* than enough
                            log::trace!("[LoaderThread] Flush loop stuck, forcing exit: {:?}", path_str);
                            break 'flush_loop;
                        }

                        if is_cancelled_clone.load(Ordering::Relaxed) {
                            if !cancelled_log_sent {
                                log::trace!("[LoaderThread] CANCELLED: {:?} (in flush_loop)", path_str);
                                cancelled_log_sent = true;
                            }
                            break 'flush_loop;
                        }

                        let mut out_buf_slices: Vec<&mut [f32]> = output_buffer.iter_mut().map(|v| v.as_mut_slice()).collect();
                        let (_frames_consumed, frames_produced) = resampler.process_partial_into_buffer(None::<&[&[f32]]>, &mut out_buf_slices, None)?;

                        if frames_produced > 0 {
                            // ... (interleave and push logic)
                            for i in 0..frames_produced {
                                interleaved_output[i * CHANNEL_COUNT] = output_buffer[0][i];
                                interleaved_output[i * CHANNEL_COUNT + 1] = output_buffer[1][i];
                            }
                            let needed = frames_produced * CHANNEL_COUNT;
                            let mut offset = 0usize;
                            while offset < needed {
                                if is_cancelled_clone.load(Ordering::Relaxed) {
                                    if !cancelled_log_sent {
                                        log::trace!("[LoaderThread] CANCELLED: {:?} (in flush_loop)", path_str);
                                        cancelled_log_sent = true;
                                    }
                                    break 'flush_loop;
                                }

                                let pushed = producer.push_slice(&interleaved_output[offset..needed]);
                                offset += pushed;
                                if offset < needed {
                                    thread::sleep(Duration::from_millis(1));
                                }
                            }
                        } else {
                            break 'flush_loop;
                        }
                    }
                    
                    log::trace!("[LoaderThread] EXITED_FLUSH_LOOP: {:?}", path_str);

                    Ok(()) // Success
                })(); // End of fallible closure
                
                // Log any Result::Err
                if let Err(e) = result {
                    log::error!("{}", e);
                }
            }); // --- End of catch_unwind ---

            // Log any panics
            if panic_result.is_err() {
                log::error!("[LoaderThread] PANICKED. This is a bug. Path: {:?}", path_str_clone);
            }
            
            log::trace!("[LoaderThread] SETTING_FINISHED: {:?}", path_str_clone);

            // This line is *outside* the unwind block and will
            // execute *even if* the code inside it panicked.
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
            log::warn!("[Voice::Drop] Leaking thread handle for {:?}.", self.debug_path.file_name().unwrap_or_default());
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

    let (reaper_tx, reaper_rx) = mpsc::channel::<thread::JoinHandle<()>>();
    spawn_reaper_thread(reaper_rx);

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

        let mut voices_to_remove: Vec<u64> = Vec::with_capacity(32);

        loop {
            // --- Handle incoming messages ---
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    AppMessage::NoteOn(note, _vel) => {
                        // Check if note is already active
                        if let Some(notes) = active_notes.get_mut(&note) {
                            if !notes.is_empty() {
                                log::warn!("[AudioThread] NoteOn received for already active note {}. Ignoring.", note);
                                continue; // Ignore this NoteOn
                            }
                        }
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
                                                true,
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
                                // insert() returns the old Vec if one existed
                                let old_notes = active_notes.insert(note, new_notes);

                                // // If there were old notes, we MUST kill them
                                // if let Some(notes_to_stop) = old_notes {
                                //     log::warn!("[AudioThread] NoteOn re-trigger on note {}. Fading old voices.", note);
                                //     for stopped_note in notes_to_stop {
                                //         // This is the same logic from handle_note_off
                                //         if let Some(voice) = voices.get_mut(&stopped_note.voice_id) {
                                //             voice.is_cancelled.store(true, Ordering::SeqCst);
                                //             voice.is_fading_out = true;
                                //             // We do NOT add a release sample here, as this is a
                                //             // re-trigger, not a release. The new voice takes over.
                                //         }
                                //     }
                                // }
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
                    AppMessage::AllNotesOff => {
                        let notes: Vec<u8> = active_notes.keys().cloned().collect();
                        for note in notes {
                            handle_note_off(
                                note, &organ, &mut voices, &mut active_notes,
                                sample_rate, &mut voice_counter,
                            );
                        }
                    }
                    AppMessage::StopToggle(stop_index, is_active) => {
                        if is_active {
                            active_stops.insert(stop_index);
                        } else {
                            // Remove the desired stop from set to prevent future notes being played
                            active_stops.remove(&stop_index); 
                        
                            // Find all currently playing notes on this stop
                            let mut notes_to_stop: Vec<ActiveNote> = Vec::new();
                        
                            // Iterate over all active notes (e.g., C4, G#5, etc.)
                            active_notes.values_mut().for_each(|note_list| {
                                // Use retain to keep notes that *don't* match the stop_index
                                note_list.retain(|an| {
                                    if an.stop_index == stop_index {
                                        // If it matches, add it to our stop list...
                                        notes_to_stop.push(an.clone()); // We need to own it
                                        // ...and return false to remove it from note_list
                                        false 
                                    } else {
                                        // Keep it
                                        true
                                    }
                                });
                            });

                            // Clean up any note keys that now have empty lists
                            active_notes.retain(|_note, note_list| !note_list.is_empty());

                            // Process each note that needs to be stopped
                            for an in notes_to_stop {
                                // First, remove the sustaining attack/loop voice
                                voices.remove(&an.voice_id); 

                                // Find the correct rank and pipe to get release samples
                                if let Some(rank) = organ.ranks.get(&an.rank_id) {
                                    if let Some(pipe) = rank.pipes.get(&an.note) {
                                        
                                        // Calculate how long the key was held in milliseconds
                                        let key_press_duration_ms = an.start_time.elapsed().as_millis() as i64;
                                        
                                        let mut default_release: Option<&PathBuf> = None;
                                        let mut best_fit_release: Option<&PathBuf> = None;
                                        let mut min_time_diff = i64::MAX;

                                        for release in &pipe.releases {
                                            if release.max_key_press_time_ms == -1 {
                                                default_release = Some(&release.path);
                                            } else if release.max_key_press_time_ms >= key_press_duration_ms {
                                                // This is a candidate for a timed release
                                                let diff = release.max_key_press_time_ms - key_press_duration_ms;
                                                if diff < min_time_diff {
                                                    min_time_diff = diff;
                                                    best_fit_release = Some(&release.path);
                                                }
                                            }
                                        }

                                        // Prioritize the best-fit timed release, but fall back to default
                                        let release_path = best_fit_release.or(default_release);

                                        if let Some(path) = release_path {
                                            let total_gain = rank.gain_db + pipe.gain_db;
                                            // Create a new one-shot voice for the release
                                            match Voice::new(
                                                path, // Use the dynamically found release path
                                                sample_rate,
                                                pipe.pitch_tuning_cents,
                                                total_gain,
                                                false, // Don't loop release sample
                                                false, // This is not an attack sample
                                            ) {
                                                Ok(voice) => {
                                                    let voice_id = voice_counter;
                                                    voice_counter += 1;
                                                    voices.insert(voice_id, voice);
                                                    // We don't add this to active_notes
                                                    // as it's just a one-shot sample.
                                                }
                                                Err(e) => {
                                                    log::error!("[AudioThread] Error creating release voice: {}", e)
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    AppMessage::Quit => {
                        drop(reaper_tx);
                        return; // Exit thread
                    }
                }
            }

            // --- Process all active voices ---
            // Clear mix buffer
            for ch_buf in mix_buffer_stereo.iter_mut() {
                ch_buf.fill(0.0);
            }

            let mut max_abs_sample = 0.0f32;

            // --- mixing loop ---
            // 20ms fade-out
            let fade_frames = (sample_rate as f32 * CROSSFADE_TIME) as usize; 
            let fade_increment = if fade_frames > 0 { 1.0 / fade_frames as f32 } else { 1.0 };

            // --- process voices ---
            for (voice_id, voice) in voices.iter_mut() {
                let is_loader_finished = voice.is_finished.load(Ordering::Relaxed);
                let mut is_buffer_empty = voice.consumer.is_empty();

                let frames_to_read = buffer_size_frames;
                let samples_to_read = frames_to_read * CHANNEL_COUNT;
                
                let samples_read = voice.consumer.pop_slice(&mut voice_read_buffer[..samples_to_read]);
                let frames_read = samples_read / CHANNEL_COUNT;

                // --- Mix / Crossfade Logic (unchanged) ---
                for i in 0..frames_read {
                    if voice.is_fading_in {
                        voice.fade_level += fade_increment;
                        if voice.fade_level >= 1.0 {
                            voice.fade_level = 1.0;
                            voice.is_fading_in = false;
                        }
                    } else if voice.is_fading_out {
                        voice.fade_level -= fade_increment;
                        if voice.fade_level <= 0.0 {
                            voice.fade_level = 0.0;
                        }
                    }
                    let current_gain = voice.gain * voice.fade_level;
                    let l_sample = voice_read_buffer[i * CHANNEL_COUNT] * current_gain * GAIN_FACTOR;
                    let r_sample = voice_read_buffer[i * CHANNEL_COUNT + 1] * current_gain * GAIN_FACTOR;
                    mix_buffer_stereo[0][i] += l_sample;
                    mix_buffer_stereo[1][i] += r_sample;
                    if l_sample.abs() > max_abs_sample {
                        max_abs_sample = l_sample.abs();
                    }
                }

                // --- Decide whether to REMOVE the voice ---
                let is_faded_out = voice.is_fading_out && voice.fade_level == 0.0;

                if is_faded_out && !is_buffer_empty {
                    let _ = voice.consumer.pop_slice(&mut tmp_drain_buffer);
                    is_buffer_empty = voice.consumer.is_empty();
                }
                
                let is_done_playing = is_loader_finished && is_buffer_empty;

                // We must remove a voice if:
                // 1. It's a "normal" voice (like a release) and it has finished playing.
                //    (is_done_playing && !voice.is_fading_out)
                // OR
                // 2. It's a "fading" voice (an attack) and it has finished fading out.
                if (is_done_playing && !voice.is_fading_out) || (is_faded_out && is_buffer_empty) {
                    voices_to_remove.push(*voice_id);
                }
            } // --- End of voice processing loop ---

            // --- Perform deferred removal ---
            if !voices_to_remove.is_empty() {
                for voice_id in voices_to_remove.iter() {
                    // Remove the voice from the active map, gaining ownership
                    if let Some(mut voice) = voices.remove(voice_id) {
                        // We now own the voice.
                        // Take the handle and send it to the reaper.
                        if let Some(handle) = voice.loader_handle.take() {
                            
                            log::debug!("[AudioThread] Sending handle for {:?} to reaper", voice.debug_path.file_name().unwrap_or_default());

                            if let Err(e) = reaper_tx.send(handle) {
                                // This should only happen if the reaper died.
                                // Fall back to forgetting the handle to avoid blocking.
                                log::error!("[AudioThread] Failed to send handle to reaper: {}", e);
                                mem::forget(e.0);
                            }
                        }
                        // `voice` is dropped here, but its handle is now None,
                        // so the Drop impl (see below) does nothing.
                    }
                }
                voices_to_remove.clear();
            }

            // --- Interleave and push to ring buffer ---
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
                log::trace!(
                    "[AudioThread] STATUS: Loop {}. Active voices: {}. Total buffered samples: {}. Main ringbuf: {}/{}",
                    loop_counter,
                    voices.len(),
                    total_voice_buffered,
                    producer.occupied_len(),
                    producer.capacity()
                );
            }

            thread::sleep(Duration::from_millis((BUFFER_SIZE_MS / 3) as u64));

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
                            false,
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
    log::debug!("[Cpal] Supported output configs:");
    for config in device.supported_output_configs()? {
        log::debug!(
            "  - Channels: {}, Sample Rate: {}-{}, Format: {:?}",
            config.channels(),
            config.min_sample_rate().0,
            config.max_sample_rate().0,
            config.sample_format()
        );
    }

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
    log::debug!(
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

/// Spawns a low-priority "reaper" thread.
/// This thread's only job is to receive finished thread handles
/// and call .join() on them, freeing their resources.
/// This prevents the real-time audio thread from ever blocking.
fn spawn_reaper_thread(rx: mpsc::Receiver<JoinHandle<()>>) {
    thread::spawn(move || {
        log::debug!("[ReaperThread] Starting...");
        
        // This loop will block on .recv() until a handle is sent.
        // It will then block on .join() until that thread finishes.
        // This is perfectly safe as it's not on the audio path.
        for handle in rx {
            if let Err(e) = handle.join() {
                log::warn!("[ReaperThread] A voice loader thread panicked: {:?}", e);
            } else {
                log::debug!("[ReaperThread] Cleaned up a voice thread.");
            }
        }
        
        // The loop exits when the sender (in AudioThread) is dropped.
        log::debug!("[ReaperThread] Shutting down.");
    });
}