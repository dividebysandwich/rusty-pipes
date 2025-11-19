use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, SampleRate, Stream, StreamConfig};
use decibel::{AmplitudeRatio, DecibelRatio};
use ringbuf::traits::{Observer, Consumer, Producer, Split};
use fft_convolver::FFTConvolver;
use ringbuf::{HeapCons, HeapRb};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::sync::{mpsc, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Instant, Duration};
use std::path::Path;
use std::mem;

use crate::app::{ActiveNote, AppMessage};
use crate::organ::Organ;
use crate::wav::{parse_wav_metadata, WavSampleReader, parse_smpl_chunk};
use crate::wav_converter::SampleMetadata;

const AUDIO_SAMPLE_RATE: u32 = 48000;
const CHANNEL_COUNT: usize = 2; // Stereo
const VOICE_BUFFER_FRAMES: usize = 14400; 
const GAIN_FACTOR: f32 = 0.5; // Prevent clipping when multiple voices mix
const CROSSFADE_TIME: f32 = 0.20; // How long to crossfade from attack to release samples, in seconds

/// Helper to get the cpal host, preferring JACK if available.
fn get_cpal_host() -> cpal::Host {
    let available_hosts = cpal::available_hosts();
    log::info!("[Cpal] Available audio hosts:");
    for host_id in &available_hosts {
        log::info!("  - {}", host_id.name());
    }
    cpal::available_hosts()
        .into_iter()
        .find(|id| id.name().to_lowercase().contains("jack"))
        .and_then(|id| cpal::host_from_id(id).ok())
        .unwrap_or_else(|| {
            log::info!("JACK host not found or failed to initialize. Falling back to default host.");
            cpal::default_host()
        })
}

/// Returns a list of available audio output device names.
pub fn get_audio_device_names() -> Result<Vec<String>> {
    let host = get_cpal_host();
    let devices = host.output_devices()?;
    let mut names = Vec::new();
    for device in devices {
        match device.name() {
            Ok(name) => names.push(name),
            Err(e) => log::warn!("[Cpal] Failed to get name for a device: {}", e),
        }
    }
    Ok(names)
}

/// Returns the name of the default audio output device, if available.
pub fn get_default_audio_device_name() -> Result<Option<String>> {
    let host = get_cpal_host();
    match host.default_output_device() {
        Some(device) => Ok(Some(device.name()?)),
        None => Ok(None),
    }
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
    is_awaiting_release_sample: bool, // Don't start the crossfade until release sample is loaded
    release_voice_id: Option<u64>,
    // Latency measurement
    note_on_time: Instant,
    has_reported_latency: bool,
    is_attack_sample: bool,
}

impl Voice {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn new(path: &Path, organ: Arc<Organ>, sample_rate: u32, gain_db: f32, start_fading_in: bool, is_attack_sample: bool, note_on_time: Instant) -> Result<Self> {
        
        let amplitude_ratio: AmplitudeRatio<f64> = DecibelRatio(gain_db as f64).into();
        let gain = amplitude_ratio.amplitude_value() as f32;

        // Create the Ring Buffer
        let ring_buf = HeapRb::<f32>::new(VOICE_BUFFER_FRAMES * CHANNEL_COUNT);
        let (mut producer, consumer) = ring_buf.split(); // consumer is HeapCons<f32>

        // Create communication atomics
        let is_finished = Arc::new(AtomicBool::new(false));
        let is_cancelled = Arc::new(AtomicBool::new(false));
        let is_attack_sample_clone = is_attack_sample;
        
        // Clone variables to move into the loader thread
        let path_buf = path.to_path_buf();
        let is_finished_clone = Arc::clone(&is_finished);
        let is_cancelled_clone = Arc::clone(&is_cancelled);
        let organ_clone = Arc::clone(&organ);
        
        // --- Spawn the Loader Thread ---
        let loader_handle = thread::spawn(move || {
            let path_buf_clone = path_buf.clone();
            let path_str = path_buf_clone.file_name().unwrap_or_default().to_string_lossy();
            let path_str_clone = path_str.clone();
            log::trace!("[LoaderThread] START: {:?}", path_str);
            
            // Use catch_unwind to handle ALL panics
            let panic_result = std::panic::catch_unwind(move || {

                let mut loader_loop_counter = 0u64;
                let mut log_throttle = 0u64;
                let mut cancelled_log_sent = false;

                // This inner closure contains all the fallible logic
                let result: Result<()> = (|| {
                    // Check cache first
                    let maybe_cached_data: Option<Arc<Vec<f32>>> = 
                        organ_clone.sample_cache.as_ref().and_then(|cache| {
                            cache.get(&path_buf).cloned()
                        });

                    let maybe_cached_metadata: Option<Arc<SampleMetadata>> =
                        organ_clone.metadata_cache.as_ref().and_then(|cache| {
                            cache.get(&path_buf).cloned()
                        });

                    let loop_info: Option<(u32, u32)>;
                    let input_channels: usize;
                    let mut source: Option<Box<dyn Iterator<Item = f32>>> = None;
                    let mut source_is_finished;
                    let use_memory_reader;
                    let mut samples_in_memory: Vec<f32> = Vec::new();
                    
                    let mut interleaved_buffer = vec![0.0f32; 1024 * CHANNEL_COUNT];

                    if let (Some(cached_samples), Some(cached_metadata)) = (maybe_cached_data, maybe_cached_metadata) {
                        // --- CACHED PATH ---
                        log::trace!("[LoaderThread] Using CACHED samples for {:?}", path_str);
                        samples_in_memory = (*cached_samples).clone();
                        // Get metadata from cache
                        loop_info = if is_attack_sample_clone { cached_metadata.loop_info } else { None };
                        input_channels = cached_metadata.channel_count as usize;
                        
                        use_memory_reader = true;
                        source_is_finished = false; // This prevents the release sample from being prematurely marked as finished
                    
                    } else {
                        // --- STREAMING PATH ---
                        if organ_clone.sample_cache.is_some() {
                            log::warn!("[LoaderThread] CACHE MISS for {:?}. Falling back to streaming.", path_str);
                        } else {
                            log::trace!("[LoaderThread] STREAMING samples for {:?}", path_str);
                        }
                        
                        let file = File::open(&path_buf.clone())
                            .map_err(|e| anyhow!("[LoaderThread] Failed to open {:?}: {}", path_buf.clone(), e))?;
                        let mut reader = BufReader::new(file);

                        // Assuming parse_wav_metadata is in a shared wav_reader mod
                        let (fmt, other_chunks, data_start, data_size) = 
                            parse_wav_metadata(&mut reader, &path_buf)
                            .map_err(|e| anyhow!("[LoaderThread] Failed to parse WAV metadata for {:?}: {}", path_buf.clone(), e))?;

                        if fmt.sample_rate != sample_rate {
                            return Err(anyhow!(
                                "[LoaderThread] File {:?} has wrong sample rate: {} (expected {}). Please re-process samples.",
                                path_buf, fmt.sample_rate, sample_rate
                            ));
                        }
                        
                        let mut loop_info_from_file = None;
                        for chunk in other_chunks {
                            if &chunk.id == b"smpl" {
                                loop_info_from_file = parse_smpl_chunk(&chunk.data);
                                break;
                            }
                        }
                        // Set metadata from file
                        loop_info = if is_attack_sample_clone { loop_info_from_file } else { None };
                        input_channels = fmt.num_channels as usize;

                        // Assuming WavSampleReader is in a shared wav_reader mod
                        let decoder = WavSampleReader::new(reader, fmt, data_start, data_size)
                            .map_err(|e| anyhow!("[LoaderThread] Failed to create sample reader for {:?}: {}", path_buf.clone(), e))?;

                        let is_looping = is_attack_sample_clone && loop_info.is_some();
                        if is_looping {
                            log::debug!("[LoaderThread] Reading {:?} into memory for looping (streaming mode).", path_str);
                            samples_in_memory = decoder.collect();
                            use_memory_reader = true;
                            source_is_finished = false;
                        } else {
                            source = Some(Box::new(decoder));
                            source_is_finished = false;
                            use_memory_reader = false;
                        }
                    }

                    // --- Setup loop points (applies to both cached and streaming-loaded-to-memory) ---
                    let is_mono = input_channels == 1;
                    let mut current_frame_index: usize = 0;
                    let mut loop_start_frame: usize = 0;
                    let mut loop_end_frame: usize = 0;
                    let mut is_looping_sample = is_attack_sample_clone && loop_info.is_some();

                    if use_memory_reader && is_looping_sample {
                        let (start, end) = loop_info.unwrap(); // Safe
                        loop_start_frame = start as usize;
                        let total_frames = samples_in_memory.len() / input_channels;
                        loop_end_frame = if end == 0 { total_frames } else { end as usize };

                        if loop_start_frame >= loop_end_frame || loop_end_frame > total_frames {
                            log::warn!(
                                "[LoaderThread] Invalid loop points for {:?}: start {}, end {}, total {}. Disabling loop.",
                                path_str, loop_start_frame, loop_end_frame, total_frames
                            );
                            is_looping_sample = false;
                            current_frame_index = 0;
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

                        let frames_to_read = 1024;
                        let mut frames_read = 0;

                        if use_memory_reader {
                            // --- READING FROM MEMORY (Looping OR One-Shot) ---
                            for i in 0..frames_to_read {
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
                                
                                interleaved_buffer[i * CHANNEL_COUNT] = sample_l;
                                interleaved_buffer[i * CHANNEL_COUNT + 1] = sample_r;
                                current_frame_index += 1;
                                frames_read += 1; // Increment frames *read*
                            }
                        } else {
                            // --- ONE-SHOT LOGIC (streaming from File) ---
                            // This branch is only entered if `use_memory_reader` is false,
                            // meaning `source.take()` was never called, so `source` is `Some`.
                            if let Some(ref mut s_iter) = source {
                                for i in 0..frames_to_read {
                                    if let Some(sample_l) = s_iter.next() {
                                        let sample_r = if is_mono {
                                            sample_l
                                        } else if let Some(r) = s_iter.next() {
                                            r
                                        } else {
                                            source_is_finished = true;
                                            sample_l // fallback
                                        };

                                        interleaved_buffer[i * CHANNEL_COUNT] = sample_l;
                                        interleaved_buffer[i * CHANNEL_COUNT + 1] = sample_r;
                                        frames_read += 1;

                                        if source_is_finished { break; }
                                    } else {
                                        source_is_finished = true;
                                        break; // End of source
                                    }
                                }
                            } else {
                                source_is_finished = true;
                            }
                        }
                        
                        // Push whatever we read
                        if frames_read > 0 {
                            let samples_to_push = frames_read * CHANNEL_COUNT;
                            let mut offset = 0;
                            while offset < samples_to_push {
                                if is_cancelled_clone.load(Ordering::Relaxed) {
                                    break 'loader_loop;
                                }
                                let pushed = producer.push_slice(&interleaved_buffer[offset..samples_to_push]);
                                offset += pushed;
                                if offset < samples_to_push {
                                    thread::sleep(Duration::from_millis(1)); // Ringbuf is full
                                }
                            }
                        }

                        // Decide to sleep or exit
                        if source_is_finished && !is_looping_sample {
                            log::trace!("[LoaderThread] FINISHED (one-shot): {:?}", path_str);
                            break 'loader_loop;
                        }

                        if is_looping_sample && frames_read == 0 {
                            // This shouldn't happen, but as a fallback
                            thread::sleep(Duration::from_millis(1));
                        }
                        
                    } // --- End of 'loader_loop ---

                    log::trace!("[LoaderThread] EXITED_MAIN_LOOP: {:?}", path_str);
                
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

        // --- Return the non-blocking Voice struct ---
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
            is_awaiting_release_sample: false,
            release_voice_id: None,
            note_on_time,
            has_reported_latency: false,
            is_attack_sample,
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

/// Contains a stereo FFT convolver for reverb processing.
struct StereoConvolver {
    convolver_l: FFTConvolver<f32>,
    convolver_r: FFTConvolver<f32>,
    is_loaded: bool,
    block_size: usize, // Store block size for re-initialization
}

impl StereoConvolver {
    /// Creates a new, empty stereo convolver.
    fn new(block_size: usize) -> Self {
        Self {
            convolver_l: FFTConvolver::<f32>::default(),
            convolver_r: FFTConvolver::<f32>::default(),
            is_loaded: false,
            block_size,
        }
    }

    /// Loads a WAV file as an Impulse Response.
    fn load_ir(&mut self, path: &Path, sample_rate: u32) -> Result<()> {
        log::info!("[Convolver] Loading IR from {:?}", path);

        // --- Load IR file ---
        let file = File::open(path)
            .map_err(|e| anyhow!("[Convolver] Failed to open IR {:?}: {}", path, e))?;
        let mut reader = BufReader::new(file);

        let (fmt, _chunks, data_start, data_size) = 
            parse_wav_metadata(&mut reader, path)
            .map_err(|e| anyhow!("[Convolver] Failed to parse IR metadata for {:?}: {}", path, e))?;

        if fmt.sample_rate != sample_rate {
            // fft_convolver doesn't resample, so this is a problem.
            // For a real-world app, you'd need to resample the IR here.
            // For now, we'll log an error and refuse to load.
            return Err(anyhow!(
                "[Convolver] IR {:?} has sample rate {}Hz, but engine is {}Hz. Please resample the IR to {}Hz.",
                path.file_name().unwrap_or_default(), fmt.sample_rate, sample_rate, sample_rate
            ));
        }

        let decoder = WavSampleReader::new(reader, fmt, data_start, data_size)
            .map_err(|e| anyhow!("[Convolver] Failed to create IR reader for {:?}: {}", path, e))?;
        
        let ir_samples_interleaved: Vec<f32> = decoder.collect();
        if ir_samples_interleaved.is_empty() {
            return Err(anyhow!("[Convolver] IR file {:?} contains no samples.", path));
        }

        // --- De-interleave ---
        let mut ir_l: Vec<f32> = Vec::new();
        let mut ir_r: Vec<f32> = Vec::new();
        let ir_channels = fmt.num_channels as usize;

        if ir_channels == 1 {
            // Mono IR: copy to both L and R
            ir_l = ir_samples_interleaved;
            ir_r = ir_l.clone();
            log::debug!("[Convolver] Loaded mono IR ({} frames).", ir_l.len());
        } else {
            // Stereo IR: de-interleave
            let num_frames = ir_samples_interleaved.len() / ir_channels;
            ir_l.reserve(num_frames);
            ir_r.reserve(num_frames);
            for i in 0..num_frames {
                // We only care about the first two channels if it's > stereo
                ir_l.push(ir_samples_interleaved[i * ir_channels]); // L
                ir_r.push(ir_samples_interleaved[i * ir_channels + 1]); // R
            }
            log::debug!("[Convolver] Loaded stereo IR ({} frames).", ir_l.len());
        }

        // --- Set IR in convolvers ---
        // We must re-create the convolvers, as `init` is the only way
        // to set the IR, and it can only be called once.
        let mut new_convolver_l = FFTConvolver::<f32>::default();
        let mut new_convolver_r = FFTConvolver::<f32>::default();

        // init(block_size, impulse_response)
        let _ = new_convolver_l.init(self.block_size, &ir_l);
        let _ = new_convolver_r.init(self.block_size, &ir_r);
        
        // Replace the old convolvers
        self.convolver_l = new_convolver_l;
        self.convolver_r = new_convolver_r;
        
        self.is_loaded = true;
        
        log::info!("[Convolver] Successfully loaded IR: {:?}", path.file_name().unwrap_or_default());
        Ok(())
    }

    /// Processes a block of stereo audio.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn process(&mut self, dry_l: &[f32], dry_r: &[f32], wet_l: &mut [f32], wet_r: &mut [f32]) {
        if !self.is_loaded {
            // If no IR is loaded, fill output with silence
            wet_l.fill(0.0);
            wet_r.fill(0.0);
            return;
        }
        
        // fft-convolver::process(input_slice, output_slice)
        // These calls will panic if the slice lengths don't match self.block_size.
        // Our main loop ensures they do.
        let _ = self.convolver_l.process(dry_l, wet_l);
        let _ = self.convolver_r.process(dry_r, wet_r);
    }
}

/// Helper function to stop one specific ActiveNote (one pipe)
/// and trigger its corresponding release sample, linking them
/// for a safe crossfade.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn trigger_note_release(
    stopped_note: ActiveNote,
    organ: &Arc<Organ>,
    voices: &mut HashMap<u64, Voice>,
    sample_rate: u32,
    voice_counter: &mut u64,
) {
    let press_duration = stopped_note.start_time.elapsed().as_millis() as i64;
    let note = stopped_note.note; // Get the note number

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
                    Arc::clone(&organ),
                    sample_rate,
                    total_gain,
                    false,
                    false,
                    Instant::now()
                ) {
                    Ok(mut voice) => {
                        log::debug!("[AudioThread] -> Created RELEASE Voice for {:?} (Duration: {}ms, Gain: {:.2}dB)",
                            release.path.file_name().unwrap_or_default(), press_duration, total_gain);
                        
                        voice.fade_level = 0.0; // Start silent

                        let release_voice_id = *voice_counter;
                        *voice_counter += 1;
                        voices.insert(release_voice_id, voice);

                        // Now link the attack voice to this new release voice
                        if let Some(attack_voice) = voices.get_mut(&stopped_note.voice_id) {
                            log::debug!("[AudioThread] ...linking attack voice {} to release voice {}", stopped_note.voice_id, release_voice_id);
                            attack_voice.is_cancelled.store(true, Ordering::SeqCst);
                            attack_voice.is_awaiting_release_sample = true;
                            attack_voice.release_voice_id = Some(release_voice_id);
                        } else {
                            // Attack voice is already gone, just fade in the release voice
                            log::warn!("[AudioThread] ...attack voice {} already gone. Fading in release {} immediately.", stopped_note.voice_id, release_voice_id);
                            if let Some(rv) = voices.get_mut(&release_voice_id) {
                                rv.is_fading_in = true;
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("[AudioThread] Error creating release sample: {}", e)
                    }
                }
            } else {
                log::warn!("[AudioThread] ...but no release sample found for pipe on note {}.", note);
                // No release sample, so just fade out the attack voice
                if let Some(voice) = voices.get_mut(&stopped_note.voice_id) {
                    log::debug!("[AudioThread] ...no release, fading out attack voice ID {}", stopped_note.voice_id);
                    voice.is_cancelled.store(true, Ordering::SeqCst);
                    voice.is_fading_out = true;
                }
            }
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
        // Create a map from stop_name -> stop_index for fast lookup
        let stop_name_to_index_map: HashMap<String, usize> = organ.stops.iter().enumerate()
            .map(|(i, stop)| (stop.name.clone(), i))
            .collect();

        let mut active_notes: HashMap<u8, Vec<ActiveNote>> = HashMap::new();
        let mut voices: HashMap<u64, Voice> = HashMap::with_capacity(128);
        let mut voice_counter: u64 = 0;
        
        // This buffer holds the "dry" mix from all voices
        let mut mix_buffer_stereo: [Vec<f32>; CHANNEL_COUNT] = [
            vec![0.0; buffer_size_frames],
            vec![0.0; buffer_size_frames],
        ];
        
        // This buffer will hold the "wet" signal from the convolver
        let mut wet_buffer_stereo: [Vec<f32>; CHANNEL_COUNT] = [
            vec![0.0; buffer_size_frames],
            vec![0.0; buffer_size_frames],
        ];
        
        let mut interleaved_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT];
        
        // --- This buffer is for popping from the voice's ringbuf ---
        let mut voice_read_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT];

        // Initialize the StereoConvolver with the correct block size
        let mut convolver = StereoConvolver::new(buffer_size_frames);
        let mut wet_dry_ratio: f32 = 0.0; // Start 100% dry

        let mut loop_counter: u64 = 0;
        let mut voices_to_remove: Vec<u64> = Vec::with_capacity(32);

        // Calculate the duration of one audio buffer in microseconds.
        let buffer_duration_micros = (buffer_size_frames as u64 * 1_000_000) / sample_rate as u64;
        
        // We will sleep for 80% of the buffer duration. This gives the thread
        // 20% of the buffer time (~1ms for a 5.3ms buffer) to wake up
        // and prepare the next block before the hardware needs it.
        let sleep_duration = Duration::from_micros(
            (buffer_duration_micros * 8) / 10
        );
        log::info!(
            "[AudioThread] Buffer duration is {:.2}ms. Using adaptive sleep of {:?}.",
            buffer_duration_micros as f32 / 1000.0,
            sleep_duration
        );

        // Pre-calculate fade increment
        let fade_frames = (sample_rate as f32 * CROSSFADE_TIME) as usize; 
        let fade_increment = if fade_frames > 0 { 1.0 / fade_frames as f32 } else { 1.0 };

        loop {
            // --- Handle incoming messages ---
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    AppMessage::NoteOn(note, _vel, stop_name) => {
                        let note_on_time = Instant::now();
                        // Find the stop_index from the stop_name
                        if let Some(stop_index) = stop_name_to_index_map.get(&stop_name) {
                            let stop = &organ.stops[*stop_index];
                            let mut new_notes = Vec::new();

                            for rank_id in &stop.rank_ids {
                                if let Some(rank) = organ.ranks.get(rank_id) {
                                    if let Some(pipe) = rank.pipes.get(&note) {
                                        let total_gain = rank.gain_db + pipe.gain_db;
                                        log::debug!("[AudioThread] NoteOn received for note {} on stop '{}' (rank: '{}', gain: {:.2}dB)",
                                            note, stop_name, rank_id, total_gain);
                                        log::debug!("[AudioThread] -> Playing pipe sample: {:?}", pipe.attack_sample_path.file_name().unwrap_or_default());
                                        // Play attack sample
                                        match Voice::new(
                                            &pipe.attack_sample_path,
                                            Arc::clone(&organ),
                                            sample_rate,
                                            total_gain,
                                            false,
                                            true,
                                            note_on_time,
                                        ) {
                                            Ok(voice) => {
                                                let voice_id = voice_counter;
                                                voice_counter += 1;
                                                voices.insert(voice_id, voice);

                                                new_notes.push(ActiveNote {
                                                    note,
                                                    start_time: note_on_time, // Use the same start time
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
                            
                            if !new_notes.is_empty() {
                                // Add all new notes to the map entry for that note number
                                active_notes.entry(note).or_default().extend(new_notes);
                            }

                        } else {
                            log::warn!("[AudioThread] NoteOn for unknown stop: {}", stop_name);
                        }
                    }
                    AppMessage::NoteOff(note, stop_name) => {
                        // Find the stop_index from the stop_name
                        log::debug!("[AudioThread] NoteOff received for note {} on stop {}", note, stop_name);
                        if let Some(stop_index) = stop_name_to_index_map.get(&stop_name) {
                            let mut stopped_note_opt: Option<ActiveNote> = None;
                            log::debug!("[AudioThread] Mapped stop '{}' to index {}", stop_name, stop_index);
                            // Check if the note is active at all
                            if let Some(note_list) = active_notes.get_mut(&note) {
                                log::debug!("[AudioThread] Found active note {} on stop {}", note, stop_name);
                                // Find the index of the specific note to remove
                                if let Some(pos) = note_list.iter().position(|an| an.stop_index == *stop_index) {
                                    log::debug!("[AudioThread] removing active note {} on stop {} with index {}", note, stop_name, pos);
                                    // Remove it from the list and take ownership
                                    stopped_note_opt = Some(note_list.remove(pos));
                                }
                                log::debug!("[AudioThread] Active notes for {}: {:?}", stop_name, note_list);
                                // If list is now empty, remove the note key from the main map
                                if note_list.is_empty() {
                                    active_notes.remove(&note);
                                }
                            }

                            // If we successfully removed a note, trigger its release
                            if let Some(stopped_note) = stopped_note_opt {
                                log::debug!("[AudioThread] Triggering release for stopped note {} on stop {}", stopped_note.note, stop_name);
                                trigger_note_release(
                                    stopped_note,
                                    &organ,
                                    &mut voices,
                                    sample_rate,
                                    &mut voice_counter
                                );
                            } else {
                                // This is common if NoteOff is sent twice, etc.
                                log::debug!("[AudioThread] NoteOff for stop {} on note {}, but not found.", stop_name, note);
                            }

                        } else {
                             log::warn!("[AudioThread] NoteOff for unknown stop: {}", stop_name);
                        }
                    }
                    AppMessage::AllNotesOff => {
                        // This is a panic, stop all notes
                        let notes: Vec<u8> = active_notes.keys().cloned().collect();
                        for note in notes {
                            handle_note_off(
                                note, &organ, &mut voices, &mut active_notes,
                                sample_rate, &mut voice_counter,
                            );
                        }
                    }
                    // Handle new reverb messages
                    AppMessage::SetReverbWetDry(ratio) => {
                        wet_dry_ratio = ratio.clamp(0.0, 1.0);
                        log::info!("[AudioThread] Reverb wet/dry ratio set to {:.0}%", wet_dry_ratio * 100.0);
                    }
                    AppMessage::SetReverbIr(path) => {
                        match convolver.load_ir(&path, sample_rate) {
                            Ok(_) => {
                                if wet_dry_ratio == 0.0 {
                                    wet_dry_ratio = 0.3;
                                    log::info!("[AudioThread] IR loaded. Setting wet/dry to 30%.");
                                }
                            },
                            Err(e) => {
                                log::error!("[AudioThread] Failed to load IR: {}", e);
                                convolver.is_loaded = false;
                                wet_dry_ratio = 0.0;
                                log::warn!("[AudioThread] Reverb disabled due to IR load error.");
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

            // --- Crossfade management logic ---
            // Find voices that are ready to start crossfading.
            let mut crossfades_to_start: Vec<(u64, u64)> = Vec::with_capacity(16);
            for (attack_id, attack_voice) in voices.iter() { // Note: .iter()
                if attack_voice.is_awaiting_release_sample {
                    if let Some(release_id) = attack_voice.release_voice_id {
                        if let Some(release_voice) = voices.get(&release_id) {
                            // The release voice is "ready" if its consumer has any data
                            if !release_voice.consumer.is_empty() {
                                log::trace!("[AudioThread] Release voice {} is ready. Starting crossfade.", release_id);
                                crossfades_to_start.push((*attack_id, release_id));
                            }
                        } else {
                            // Release voice has disappeared? (e.g., finished instantly)
                            // Start fade-out anyway.
                            log::warn!("[AudioThread] Release voice {} not found for attack voice {}. Fading out attack.", release_id, *attack_id);
                            crossfades_to_start.push((*attack_id, u64::MAX)); // use u64::MAX to indicate no release
                        }
                    }
                }
            }
            // Apply the state changes for ready crossfades.
            for (attack_id, release_id) in crossfades_to_start {
                if let Some(attack_voice) = voices.get_mut(&attack_id) {
                    attack_voice.is_fading_out = true;
                    attack_voice.is_awaiting_release_sample = false; // Done waiting
                    attack_voice.release_voice_id = None;
                }
                
                if release_id != u64::MAX {
                    if let Some(release_voice) = voices.get_mut(&release_id) {
                        release_voice.is_fading_in = true;
                    }
                }
            }

            // --- Voice Processing Loop ---
            for (voice_id, voice) in voices.iter_mut() {
                let is_loader_finished = voice.is_finished.load(Ordering::Relaxed);
                
                let samples_to_read = buffer_size_frames * CHANNEL_COUNT;
                let samples_available = voice.consumer.occupied_len();

                // Check if we have enough data to fill a full block
                if samples_available < samples_to_read {
                    // Not enough data.
                    if is_loader_finished && samples_available == 0 && !voice.is_fading_out {
                        // Loader is done, buffer is empty, and not fading out: remove it.
                        voices_to_remove.push(*voice_id);
                    }
                    // Otherwise, just skip this voice for this block.
                    continue;
                }

                // Pop *exactly* the amount we need
                let frames_read = buffer_size_frames;
                let _ = voice.consumer.pop_slice(&mut voice_read_buffer[..samples_to_read]);

                // --- Latency Reporting ---
                if voice.is_attack_sample && !voice.has_reported_latency {
                    let latency = voice.note_on_time.elapsed();
                    log::debug!(
                        "[AudioThread] Latency for attack voice {} ({:?}): {:.2}ms",
                        voice_id, voice.debug_path.file_name().unwrap_or_default(), latency.as_secs_f32() * 1000.0
                    );
                    voice.has_reported_latency = true;
                }

                // --- Fade/Gain Logic ---
                let initial_fade_level = voice.fade_level;
                let mut final_fade_level = initial_fade_level;

                if voice.is_fading_in {
                    final_fade_level += fade_increment * (frames_read as f32);
                    if final_fade_level >= 1.0 {
                        final_fade_level = 1.0;
                        voice.is_fading_in = false; // State change for next block
                    }
                } else if voice.is_fading_out {
                    final_fade_level -= fade_increment * (frames_read as f32);
                    if final_fade_level <= 0.0 {
                        final_fade_level = 0.0;
                    }
                }
                
                // Store the final fade level. This is now the state for the *next* block.
                voice.fade_level = final_fade_level;

                let start_gain = voice.gain * initial_fade_level * GAIN_FACTOR;
                let end_gain = voice.gain * final_fade_level * GAIN_FACTOR;

                // --- Mixing Loop ---
                if (start_gain - end_gain).abs() < 1e-8 { // Check for float equality
                    // --- FAST PATH (No fade / constant gain) ---
                    // This is the common case.
                    if start_gain == 0.0 {
                        // Faded out, or gain is 0. Do nothing.
                    } else {
                        // Optimized loop with no branching.
                        // The compiler can vectorize this.
                        let gain = start_gain;
                        for i in 0..frames_read {
                            let read_idx = i * CHANNEL_COUNT;
                            mix_buffer_stereo[0][i] += voice_read_buffer[read_idx] * gain;
                            mix_buffer_stereo[1][i] += voice_read_buffer[read_idx + 1] * gain;
                        }
                    }
                } else {
                    // --- SLOW PATH (Fading) ---
                    // This only runs during a crossfade.
                    let gain_step = (end_gain - start_gain) / (frames_read as f32);
                    let mut current_gain = start_gain;
                    
                    for i in 0..frames_read {
                        let read_idx = i * CHANNEL_COUNT;
                        mix_buffer_stereo[0][i] += voice_read_buffer[read_idx] * current_gain;
                        mix_buffer_stereo[1][i] += voice_read_buffer[read_idx + 1] * current_gain;
                        current_gain += gain_step;
                    }
                }

                // --- Voice Removal Logic (simplified) ---
                // Check if the voice *finished* fading out *this block*.
                if voice.is_fading_out && voice.fade_level == 0.0 {
                    // Drain any remaining buffered samples just in case
                    voice.consumer.skip(voice.consumer.occupied_len());
                    voices_to_remove.push(*voice_id);
                }
            }
            // --- End of voice processing loop ---

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

            // Get mutable, non-overlapping slices *from the array itself*
            let (wet_l_slice, wet_r_slice) = wet_buffer_stereo.split_at_mut(1);
            let wet_l_vec = &mut wet_l_slice[0]; // This is &mut Vec<f32>
            let wet_r_vec = &mut wet_r_slice[0]; // This is &mut Vec<f32>
            // --- Apply Convolution Reverb ---
            // The input and output slices MUST match the block_size the
            // convolver was initialized with. Our buffers are already
            // correctly sized to `buffer_size_frames`.
            convolver.process(
                &mix_buffer_stereo[0],     // Dry L
                &mix_buffer_stereo[1],     // Dry R
                wet_l_vec,                 // Wet L (&mut [f32])
                wet_r_vec,                 // Wet R (&mut [f32])
            );

            // --- Interleave, Mix, and push to ring buffer ---
            let dry_level = 1.0 - wet_dry_ratio;
            let wet_level = wet_dry_ratio;

            for i in 0..buffer_size_frames {
                let dry_l = mix_buffer_stereo[0][i];
                let dry_r = mix_buffer_stereo[1][i];
                let wet_l = wet_buffer_stereo[0][i];
                let wet_r = wet_buffer_stereo[1][i];

                // Apply wet/dry mix (linear crossfade)
                let final_l = (dry_l * dry_level) + (wet_l * wet_level);
                let final_r = (dry_r * dry_level) + (wet_r * wet_level);

                interleaved_buffer[i * CHANNEL_COUNT] = final_l;
                interleaved_buffer[i * CHANNEL_COUNT + 1] = final_r;
            }

            // This loop will now only run if the buffer is *extremely* full
            // (e.g., if the sleep duration was too short and we lapped the consumer).
            // In the common case, it will push all at once and not sleep.
            let mut offset = 0;
            let needed = interleaved_buffer.len();
            while offset < needed {
                let pushed = producer.push_slice(&interleaved_buffer[offset..needed]);
                offset += pushed;
                if offset < needed {
                    // Buffer is full. Sleep for a short, defensive time.
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

            thread::sleep(sleep_duration);
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
    // This removes *all* active notes for this note number,
    // which is used for the panic function
    if let Some(notes_to_stop) = active_notes.remove(&note) {
        for stopped_note in notes_to_stop {
            // This `stopped_note` is an `ActiveNote`
            trigger_note_release(
                stopped_note, // Pass ownership
                organ,
                voices,
                sample_rate,
                voice_counter
            );
        }
    } else {
        log::warn!("[AudioThread] ...but no active notes found for note {}.", note);
    }
}

/// Sets up the cpal audio stream and spawns the processing thread.
pub fn start_audio_playback(
    rx: mpsc::Receiver<AppMessage>,
    organ: Arc<Organ>,
    buffer_size_frames: usize,
    audio_device_name: Option<String>
) -> Result<Stream> {
    let available_hosts = cpal::available_hosts();
    log::info!("[Cpal] Available audio hosts:");
    for host_id in &available_hosts {
        log::info!("  - {}", host_id.name());
    }
    let host = get_cpal_host();

    let device: Device = {
        if let Some(name) = audio_device_name {
            log::info!("[Cpal] Attempting to find device by name: {}", name);
            host.output_devices()?
                .find(|d| d.name().map_or(false, |n| n == name))
                .ok_or_else(|| anyhow!("Audio device not found: {}. Falling back to default.", name))
                // Fallback to default if not found
                .or_else(|e| {
                    log::warn!("{}", e);
                    host.default_output_device().ok_or_else(|| anyhow!("No default output device available"))
                })?
        } else {
            log::info!("[Cpal] Using default output device.");
            host.default_output_device()
                .ok_or_else(|| anyhow!("No default output device available"))?
        }
    };

    log::info!(
        "[Cpal] Using output device: {}",
        device.name().unwrap_or_else(|_| "Unknown".to_string())
    );

    // Find a supported config
    let supported_configs = device.supported_output_configs()?;

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

    let target_sample_rate = SampleRate(AUDIO_SAMPLE_RATE);
    let config_range = supported_configs
        .filter(|c| c.sample_format() == SampleFormat::F32 && c.channels() >= 2)
        .find(|c| {
            // Check if our target rate is within the config's range
            c.min_sample_rate() <= target_sample_rate 
            && c.max_sample_rate() >= target_sample_rate
        })
        .ok_or_else(|| {
            anyhow!(
                "No supported F32 config found for sample rate {}Hz",
                target_sample_rate.0
            )
        })?;
    
    let config = config_range.with_sample_rate(target_sample_rate);

    let sample_format = config.sample_format();
    let stream_config: StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate.0;
    let device_channels = stream_config.channels as usize;

    println!(
        "[Cpal] Using config: SampleRate: {}, Device channels: {}, Format: {:?}",
        sample_rate, device_channels, sample_format
    );

    let mix_channels = 2; // Our engine outputs stereo
    // Create the ring buffer
    let ring_buf_capacity = buffer_size_frames * mix_channels * 10;
    let ring_buf = HeapRb::<f32>::new(ring_buf_capacity);
    log::debug!(
        "[Cpal] Ring buffer created with capacity for {} frames.",
        ring_buf_capacity / mix_channels
    );
    let (producer, mut consumer) = ring_buf.split();

    // Spawn the audio processing thread
    spawn_audio_processing_thread(rx, producer, organ, sample_rate, buffer_size_frames);

    let mut stereo_read_buffer: Vec<f32> = vec![0.0; buffer_size_frames * 2];

    // --- The cpal audio callback ---
    let data_callback = move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let out_channels = device_channels;
        let in_channels = 2;
        // Calculate how many *frames* the device is asking for.
        let frames_to_write = output.len() / out_channels;

        // Check how many *frames* our engine has ready.
        let frames_available = consumer.occupied_len() / in_channels;

        // We'll process the minimum of the two.
        let frames_to_process = frames_to_write.min(frames_available);
        let samples_to_read = frames_to_process * in_channels;

        if frames_to_process > 0 {
            // Resize our temp buffer if needed and pop the stereo data.
            if stereo_read_buffer.len() < samples_to_read {
                stereo_read_buffer.resize(samples_to_read, 0.0);
            }
            let _ = consumer.pop_slice(&mut stereo_read_buffer[..samples_to_read]);

            // 5. Manually interleave the stereo data into the 6-channel output.
            let mut in_idx = 0;
            let mut out_idx = 0;
            for _ in 0..frames_to_process {
                // Copy L -> C1
                output[out_idx + 0] = stereo_read_buffer[in_idx + 0];
                // Copy R -> C2
                output[out_idx + 1] = stereo_read_buffer[in_idx + 1];
                
                for ch in 2..out_channels {
                    output[out_idx + ch] = 0.0; // Fill unused channels with silence
                }
                in_idx += in_channels;
                out_idx += out_channels;
            }
        }

        // Fill remaining buffer with silence if we underrun.
        let silence_start_frame = frames_to_process;
        if silence_start_frame < frames_to_write {
            let silence_start_sample = silence_start_frame * out_channels;
            for sample in &mut output[silence_start_sample..] {
                *sample = 0.0;
            }
            if frames_available > 0 {
                log::warn!("[CpalCallback] Audio buffer underrun! Wrote {} silent frames.", frames_to_write - frames_to_process);
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