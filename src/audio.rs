use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, SampleRate, Stream, StreamConfig};
use decibel::{AmplitudeRatio, DecibelRatio};
use ringbuf::traits::{Observer, Consumer, Producer, Split};
use fft_convolver::FFTConvolver;
use ringbuf::{HeapCons, HeapRb, HeapProd};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::BufReader;
use std::sync::{mpsc, Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Instant, Duration};
use std::path::{Path, PathBuf};
use chrono::Local;
use hound;

use crate::app::{ActiveNote, AppMessage};
use crate::organ::Organ;
use crate::wav::{parse_wav_metadata, WavSampleReader, parse_smpl_chunk};
use crate::TuiMessage;
use crate::midi::MidiRecorder;

const CHANNEL_COUNT: usize = 2; // Stereo
const VOICE_BUFFER_FRAMES: usize = 14400; 
const CROSSFADE_TIME: f32 = 0.10; // How long to crossfade from attack to release samples, in seconds
const VOICE_STEALING_FADE_TIME: f32 = 1.00; // Fade out stolen release samples over 1s
const MAX_NEW_VOICES_PER_BLOCK: usize = 28; // Limit how many new voices can be started per audio block
const TREMULANT_AM_BOOST: f32 = 1.0;

struct AudioRecorder {
    sender: mpsc::Sender<Vec<f32>>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl AudioRecorder {
    fn start(organ_name: String, sample_rate: u32) -> Result<Self> {
        let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;
        let parent = config_path.parent().ok_or_else(|| anyhow::anyhow!("No config parent dir"))?;
        let recording_dir = parent.join("recordings");
        if !recording_dir.exists() {
            fs::create_dir_all(&recording_dir)?;
        }

        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename = format!("{}_{}.wav", organ_name, timestamp);
        let path = recording_dir.join(filename);

        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let path_clone = path.clone();

        let handle = thread::spawn(move || {
            let mut writer = match hound::WavWriter::create(path_clone, spec) {
                Ok(w) => w,
                Err(e) => {
                    log::error!("Failed to create WAV writer: {}", e);
                    return;
                }
            };

            for buffer in rx {
                for sample in buffer {
                    if let Err(e) = writer.write_sample(sample) {
                        log::error!("Error writing sample: {}", e);
                    }
                }
            }
            // Writer finalizes on drop
            log::info!("WAV recording saved.");
        });
        
        log::info!("Started recording audio to {:?}", path);

        Ok(Self {
            sender: tx,
            thread_handle: Some(handle),
        })
    }

    fn push(&mut self, buffer: &[f32]) {
        // Send a clone of the buffer to the writer thread
        // In high-performance scenarios, we might recycle these vecs
        let _ = self.sender.send(buffer.to_vec());
    }

    fn stop(self) {
        drop(self.sender); // Close channel
        if let Some(h) = self.thread_handle {
            let _ = h.join();
        }
    }
}

// Helper struct to track phase for tremulants in audio thread
struct TremulantLfo {
    phase: f32, // 0.0 to 1.0 (Current position in the sine wave)
    current_level: f32, // 0.0 to 1.0 (Current spin-up/spin-down intensity)
}

struct SpawnJob {
    path: PathBuf,
    organ: Arc<Organ>,
    sample_rate: u32,
    is_attack_sample: bool,
    frames_to_skip: usize,
    // We pass the RingBuffer producer to the thread so it can fill it
    producer: HeapProd<f32>,
    is_finished: Arc<AtomicBool>,
    is_cancelled: Arc<AtomicBool>,
}

/// Returns a sorted list of standard sample rates supported by the device.
pub fn get_supported_sample_rates(device_name: Option<String>) -> Result<Vec<u32>> {
    let host = get_cpal_host();
    
    let device = if let Some(name) = device_name {
        host.output_devices()?
            .find(|d| d.name().map_or(false, |n| n == name))
            .ok_or_else(|| anyhow!("Device not found"))?
    } else {
        host.default_output_device().ok_or_else(|| anyhow!("No default device"))?
    };

    let supported_configs = device.supported_output_configs()?;
    let standard_rates = [44100, 48000, 88200, 96000, 176400, 192000];
    let mut available_rates = Vec::new();

    for config_range in supported_configs {
        log::info!("[Cpal] Device {:?} supports config: {:?} - Sample Format: {:?}", device.name()?, config_range, config_range.sample_format());
        // We only care about F32 usually, but let's be generous for checking rates
        let min = config_range.min_sample_rate().0;
        let max = config_range.max_sample_rate().0;

        for &rate in &standard_rates {
            if rate >= min && rate <= max {
                if !available_rates.contains(&rate) {
                    available_rates.push(rate);
        }
    }
        }
    }
    
    available_rates.sort();
    
    if available_rates.is_empty() {
        // Fallback if no standard rates match
        available_rates.push(48000); 
    }

    // Log the available rates
    log::info!("[Cpal] Supported sample rates for device {:?}: {:?}", device.name()?, available_rates);

    Ok(available_rates)
}

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
#[allow(non_snake_case)]
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
    
    // The main thread *only* interacts with these:
    consumer: HeapCons<f32>, // Use concrete type HeapCons
    is_finished: Arc<AtomicBool>, // Has the loader thread finished?
    is_cancelled: Arc<AtomicBool>, // Has NoteOff told the loader to stop?
    
    fade_level: f32, // 1.0 = full volume, 0.0 = silent
    is_fading_out: bool, // Is the attack sample fading out?
    is_fading_in: bool, // Is the release sample fading in?
    is_awaiting_release_sample: bool, // Don't start the crossfade until release sample is loaded
    release_voice_id: Option<u64>,
    // Latency measurement
    note_on_time: Instant,
    is_attack_sample: bool,
    fade_increment: f32,
    
    // Tracks which windchest group this voice belongs to, for tremulant effects
    windchest_group_id: Option<String>,

    input_buffer: Vec<f32>, 
    buffer_start_idx: usize, // Offset into input_buffer where valid data begins
    cursor_pos: f32, // Fractional position within input_buffer (in frames)
}

impl Voice {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn new(
        path: &Path, 
        organ: Arc<Organ>, 
        sample_rate: u32, 
        gain_db: f32, 
        start_fading_in: bool, 
        is_attack_sample: bool, 
        note_on_time: Instant,
        preloaded_bytes: Option<Arc<Vec<f32>>>,
        spawner_tx: &mpsc::Sender<SpawnJob>,
        windchest_group_id: Option<String>,
    ) -> Result<Self> {
        
        let fade_frames = (sample_rate as f32 * CROSSFADE_TIME) as usize;
        let fade_increment = if fade_frames > 0 { 1.0 / fade_frames as f32 } else { 1.0 };

        let amplitude_ratio: AmplitudeRatio<f64> = DecibelRatio(gain_db as f64).into();
        let gain = amplitude_ratio.amplitude_value() as f32;

        // Create the Ring Buffer
        let ring_buf = HeapRb::<f32>::new(VOICE_BUFFER_FRAMES * CHANNEL_COUNT);
        let (mut producer, consumer) = ring_buf.split(); // consumer is HeapCons<f32>

        // Create communication atomics
        let is_finished = Arc::new(AtomicBool::new(false));
        let is_cancelled = Arc::new(AtomicBool::new(false));
        
        // If we have pre-loaded attack bytes, push them to the ring buffer right away.
        let mut preloaded_frames_count = 0;
        if let Some(ref preloaded) = preloaded_bytes {
            // Push as much as fits (it should all fit given buffer size)
            let pushed = producer.push_slice(preloaded);
            // Calculate how many frames we skipped so the loader thread knows where to start
            preloaded_frames_count = pushed / CHANNEL_COUNT;
        }

        // Instead of calling thread::spawn here (heavy system call),
        // we push a lightweight struct to a channel.
        let job = SpawnJob {
            path: path.to_path_buf(),
            organ: Arc::clone(&organ),
            sample_rate,
            is_attack_sample,
            frames_to_skip: preloaded_frames_count,
            producer, // Move the producer to the job
            is_finished: Arc::clone(&is_finished),
            is_cancelled: Arc::clone(&is_cancelled),
        };

        // Send to background spawner. If channel is full/broken, we log error but don't panic.
        if let Err(e) = spawner_tx.send(job) {
            log::error!("Failed to queue voice spawn job: {}", e);
            // If we fail to spawn, we should probably mark finished so the voice gets cleaned up
            is_finished.store(true, Ordering::Relaxed);
        }
        
        // --- Return the non-blocking Voice struct ---
        Ok(Self {
            gain,
            consumer,
            is_finished,
            is_cancelled,
            fade_level: if start_fading_in { 0.0 } else { 1.0 }, 
            is_fading_out: false,
            is_fading_in: start_fading_in,
            is_awaiting_release_sample: false,
            release_voice_id: None,
            note_on_time,
            is_attack_sample,
            fade_increment,
            windchest_group_id,
            input_buffer: Vec::with_capacity(4096),
            buffer_start_idx: 0,
            cursor_pos: 0.0,
        })
    }
}

// This stops the audio thread from blocking when a voice is dropped.
impl Drop for Voice {
    fn drop(&mut self) {
        // Tell the loader thread to stop, just in case
        self.is_cancelled.store(true, Ordering::SeqCst);
    }
}

/// Simple Linear Interpolation Resampler.
/// Used to convert IR files to the engine's sample rate on the fly.
fn resample_interleaved(input: &[f32], channels: usize, from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }

    let ratio = from_rate as f64 / to_rate as f64;
    let input_frames = input.len() / channels;
    let output_frames = (input_frames as f64 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_frames * channels);

    for i in 0..output_frames {
        let src_idx_float = i as f64 * ratio;
        let index0 = src_idx_float.floor() as usize;
        let index1 = (index0 + 1).min(input_frames - 1);
        let frac = (src_idx_float - index0 as f64) as f32;

        for c in 0..channels {
            let s0 = input[index0 * channels + c];
            let s1 = input[index1 * channels + c];
            // Linear interpolation
            let interpolated = s0 + (s1 - s0) * frac;
            output.push(interpolated);
        }
    }
    
    log::info!("[Resampler] Resampled IR from {}Hz to {}Hz. ({} -> {} frames)", 
        from_rate, to_rate, input_frames, output_frames);

    output
}

/// A stereo FFT convolver for reverb processing.
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

    /// Loads a WAV file as an Impulse Response (Blocking version, mainly for testing).
    /// Returns a new instance with the loaded IR.
    fn from_file(path: &Path, sample_rate: u32, block_size: usize) -> Result<Self> {
        log::info!("[Convolver] Loading IR from {:?}", path);

        // --- Load IR file ---
        let file = File::open(path)
            .map_err(|e| anyhow!("[Convolver] Failed to open IR {:?}: {}", path, e))?;
        let mut reader = BufReader::new(file);

        let (fmt, _chunks, data_start, data_size) = 
            parse_wav_metadata(&mut reader, path)
            .map_err(|e| anyhow!("[Convolver] Failed to parse IR metadata for {:?}: {}", path, e))?;

        let decoder = WavSampleReader::new(reader, fmt, data_start, data_size)
            .map_err(|e| anyhow!("[Convolver] Failed to create IR reader for {:?}: {}", path, e))?;
        
        let mut ir_samples_interleaved: Vec<f32> = decoder.collect();
        if ir_samples_interleaved.is_empty() {
            return Err(anyhow!("[Convolver] IR file {:?} contains no samples.", path));
        }

        if fmt.sample_rate != sample_rate {
            log::warn!("[Convolver] IR Rate Mismatch (File: {}, Engine: {}). Resampling...", fmt.sample_rate, sample_rate);
            ir_samples_interleaved = resample_interleaved(
                &ir_samples_interleaved, 
                fmt.num_channels as usize, 
                fmt.sample_rate, 
                sample_rate
            );
        }

        // --- De-interleave ---
        let mut ir_l: Vec<f32> = Vec::new();
        let mut ir_r: Vec<f32> = Vec::new();
        let ir_channels = fmt.num_channels as usize;

        if ir_channels == 1 {
            // Mono IR: copy to both L and R
            ir_l = ir_samples_interleaved;
            ir_r = ir_l.clone();
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
        }

        // --- Peak Normalization with Headroom ---
        // Find the loudest peak in the file.
        let max_l = ir_l.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        let max_r = ir_r.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        let global_peak = max_l.max(max_r);

        if global_peak > 0.0 {
            // Normalize so Peak == 1.0 (This preserves the natural character)
            // Apply a safety attenuation to prevent summation clipping.
            // This mimics "turning down the master volume" of the reverb engine internally.
            let target_peak = 0.015; 
            let scale = target_peak / global_peak;
            
            log::debug!("[Convolver] Normalizing IR. Input Peak: {:.4}, Scale Factor: {:.4} (Target: {})", global_peak, scale, target_peak);

            for x in ir_l.iter_mut() { *x *= scale; }
            for x in ir_r.iter_mut() { *x *= scale; }
        } else {
            log::warn!("[Convolver] IR appears to be silent.");
        }

        // --- Set IR in convolvers ---
        // We must re-create the convolvers, as `init` is the only way
        // to set the IR, and it can only be called once.
        let mut convolver_l = FFTConvolver::<f32>::default();
        let mut convolver_r = FFTConvolver::<f32>::default();

        let _ = convolver_l.init(block_size, &ir_l);
        let _ = convolver_r.init(block_size, &ir_r);
        
        log::info!("[Convolver] Successfully prepared IR.");
        
        Ok(Self {
            convolver_l,
            convolver_r,
            is_loaded: true,
            block_size
        })
    }
    fn process(&mut self, dry_l: &[f32], dry_r: &[f32], wet_l: &mut [f32], wet_r: &mut [f32]) {
        if !self.is_loaded {
            // If no IR is loaded, fill output with silence
            wet_l.fill(0.0);
            wet_r.fill(0.0);
            return;
        }
        
        // Ensure input chunk matches the initialized block size
        if dry_l.len() != self.block_size || dry_r.len() != self.block_size {
            // If we don't check this, convolver_l.process() would panic
            log::error!("[Convolver] Block size mismatch! Configured for {}, but got buffer of size {}", self.block_size, dry_l.len());
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


/// If voice limit is exceeded, this finds the oldest *release* samples 
/// and forces them to fade out quickly.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn enforce_voice_limit(voices: &mut HashMap<u64, Voice>, sample_rate: u32, polyphony: usize) {
    // Only count voices that are NOT already fading out.
    // If a voice is fading, we have already "dealt with it" and shouldn't 
    // punish the remaining voices while waiting for the dead one to exit.
    let active_musical_voices = voices.values()
        .filter(|v| !v.is_fading_out)
        .count();

    if active_musical_voices <= polyphony {
        return;
    }

    let voices_to_steal = active_musical_voices - polyphony;

    // Identify candidates: 
    //    - Must not be an attack sample (we only steal release tails)
    //    - Must not already be fading out (don't double-steal)
    //    - OPTIONAL: Give them a grace period. Don't steal a release voice 
    //      that is less than 50ms old, or it sounds like a glitch.
    let min_age = Duration::from_millis(50);
    
    let mut candidates: Vec<(u64, Instant)> = voices.iter()
        .filter(|(_, v)| {
            !v.is_attack_sample 
            && !v.is_fading_out 
            && v.note_on_time.elapsed() > min_age
        })
        .map(|(id, v)| (*id, v.note_on_time))
        .collect();

    // Sort by oldest time first (ascending Instant)
    candidates.sort_by_key(|(_, time)| *time);

    // Steal the oldest ones
    for (voice_id, _) in candidates.iter().take(voices_to_steal) {
        if let Some(voice) = voices.get_mut(voice_id) {
            log::warn!("[AudioThread] Voice Limit Exceeded ({}/{}). Stealing Release Voice ID {}", active_musical_voices, polyphony, voice_id);
            
            // Force into fade-out mode
            voice.is_fading_out = true;
            voice.is_fading_in = false; // Cancel any fade-in if it was happening
            
            // This overrides the default CROSSFADE_TIME speed with the faster stealing speed
            let steal_fade_frames = (sample_rate as f32 * VOICE_STEALING_FADE_TIME) as usize;
            voice.fade_increment = if steal_fade_frames > 0 { 
                1.0 / steal_fade_frames as f32 
            } else { 
                1.0 
            };
        }
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
    spawner_tx: &mpsc::Sender<SpawnJob>,
) {
    let press_duration = stopped_note.start_time.elapsed().as_millis() as i64;
    let note = stopped_note.note;

    if let Some(rank) = organ.ranks.get(&stopped_note.rank_id) {
        if let Some(pipe) = rank.pipes.get(&note) {
            let release_sample = pipe.releases.iter()
                .find(|r| r.max_key_press_time_ms == -1 || press_duration <= r.max_key_press_time_ms)
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
                    rank.windchest_group_id.clone() // Pass the group ID so tremulant affects release tail
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
            
            // If release creation failed (or no sample), force attack to fade out immediately.
            // Otherwise, it gets stuck in the loop forever.
            if !release_created {
                if let Some(voice) = voices.get_mut(&stopped_note.voice_id) {
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
    mut system_gain: f32,
    mut polyphony: usize,
    tui_tx: mpsc::Sender<TuiMessage>,
    shared_midi_recorder: Arc<Mutex<Option<MidiRecorder>>>,
) where
    P: Producer<Item = f32> + Send + 'static,
{

    // Channel for receiving loaded Reverb convolvers from background thread
    let (ir_loader_tx, ir_loader_rx) = mpsc::channel::<Result<StereoConvolver>>();

    // Spawner Thread Setup
    let (spawner_tx, spawner_rx) = mpsc::channel::<SpawnJob>();

    // Spawn the background thread that handles the "heavy" thread::spawn calls
    thread::spawn(move || {
        log::info!("[SpawnerThread] Started.");
        for job in spawner_rx {
            // Detached thread, runs until IO is done.
            thread::spawn(move || {
                run_loader_job(job);
            });
        }
        log::info!("[SpawnerThread] Shutting down.");
    });

    thread::spawn(move || {
        // Create a map from stop_name -> stop_index for fast lookup
        let stop_name_to_index_map: HashMap<String, usize> = organ.stops.iter().enumerate()
            .map(|(i, stop)| (stop.name.clone(), i))
            .collect();

        let mut active_notes: HashMap<u8, Vec<ActiveNote>> = HashMap::new();
        let mut voices: HashMap<u64, Voice> = HashMap::with_capacity(128);
        let mut voice_counter: u64 = 0;
        
        // This buffer holds the "dry" mix from all voices
        let mut mix_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT];
        
        // Scratch buffers for Reverb (Planar)
        let mut reverb_dry_l: Vec<f32> = vec![0.0; buffer_size_frames];
        let mut reverb_dry_r: Vec<f32> = vec![0.0; buffer_size_frames];
        
        let mut wet_buffer_l: Vec<f32> = vec![0.0; buffer_size_frames];
        let mut wet_buffer_r: Vec<f32> = vec![0.0; buffer_size_frames];
        
        // Initialize the StereoConvolver with the correct block size
        let mut convolver = StereoConvolver::new(buffer_size_frames);
        let mut wet_dry_ratio: f32 = 0.0; // Start 100% dry

        let mut voices_to_remove: Vec<u64> = Vec::with_capacity(32);

        // Calculate the maximum time allowed per buffer (CPU budget)
        let buffer_duration_secs = buffer_size_frames as f32 / sample_rate as f32;

        let mut last_ui_update = Instant::now();
        let ui_update_interval = Duration::from_millis(250);
        let mut last_reported_voice_count: usize = usize::MAX;
        
        let mut max_load_accumulator = 0.0f32;

        let mut pending_note_queue: VecDeque<AppMessage> = VecDeque::with_capacity(64);

        // --- Tremulant State ---
        let mut active_tremulants_ids: HashMap<String, bool> = HashMap::new(); // State tracking
        let mut tremulant_lfos: HashMap<String, TremulantLfo> = HashMap::new(); // Phase tracking
        let mut prev_windchest_mods: HashMap<String, f32> = HashMap::new();
        let mut scratch_read_buffer: Vec<f32> = vec![0.0; buffer_size_frames * CHANNEL_COUNT * 2];

        let mut audio_recorder: Option<AudioRecorder> = None;

        loop {
            let start_time = Instant::now();

            // Drain incoming messages from UI to internal queue
            // We differentiate "Immediate" vs "Deferrable" events
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    AppMessage::NoteOn(..) => pending_note_queue.push_back(msg),
                    // Set active status for tremulants
                    AppMessage::SetTremulantActive(id, active) => {
                        active_tremulants_ids.insert(id, active);
                    },
                    AppMessage::StartAudioRecording => {
                        match AudioRecorder::start(organ.name.clone(), sample_rate) {
                            Ok(rec) => { 
                                audio_recorder = Some(rec); 
                                let _ = tui_tx.send(TuiMessage::MidiLog("Audio Recording Started".into()));
                            },
                            Err(e) => { 
                                let _ = tui_tx.send(TuiMessage::Error(format!("Rec Error: {}", e))); 
                            }
                        }
                    },
                    AppMessage::StopAudioRecording => {
                         if let Some(rec) = audio_recorder.take() {
                             rec.stop();
                             let _ = tui_tx.send(TuiMessage::MidiLog("Audio Recording Stopped/Saved".into()));
                         }
                    },
                    AppMessage::StartMidiRecording => {
                        let mut guard = shared_midi_recorder.lock().unwrap();
                        if guard.is_none() {
                            *guard = Some(MidiRecorder::new(organ.name.clone()));
                            let _ = tui_tx.send(TuiMessage::MidiLog("MIDI Recording Started (Virtual Chs)".into()));
                        }
                    },
                    AppMessage::StopMidiRecording => {
                        let mut guard = shared_midi_recorder.lock().unwrap();
                        if let Some(recorder) = guard.take() {
                            match recorder.save() {
                                Ok(path) => { let _ = tui_tx.send(TuiMessage::MidiLog(format!("Saved: {}", path))); },
                                Err(e) => { let _ = tui_tx.send(TuiMessage::Error(format!("MIDI Save Error: {}", e))); }
                            }
                        }
                    },
                    // Handle control messages immediately to feel responsive
                    _ => process_message(msg, 
                        &mut wet_dry_ratio, 
                        &mut system_gain, 
                        &mut polyphony, 
                        &ir_loader_tx, 
                        sample_rate, 
                        buffer_size_frames, 
                        &mut active_notes, 
                        &organ, &mut voices, 
                        &mut voice_counter, 
                        &stop_name_to_index_map, 
                        &spawner_tx,
                        &mut pending_note_queue,
                    ),
                }
            }

            // Process pending NoteOns with Throttling
            let mut new_voice_count = 0;
            while new_voice_count < MAX_NEW_VOICES_PER_BLOCK {
                if let Some(msg) = pending_note_queue.pop_front() {
                    // This function only handles NoteOn now
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

            // Check IR Loader
            if let Ok(Ok(conv)) = ir_loader_rx.try_recv() { convolver = conv; if wet_dry_ratio == 0.0 { wet_dry_ratio = 0.3; } }

            mix_buffer.fill(0.0);
            enforce_voice_limit(&mut voices, sample_rate, polyphony);

            // --- Update Tremulant LFOs ---
            // This calculates the modulation factor for each active tremulant
            // and then maps those factors to Windchest Groups

            let dt = buffer_size_frames as f32 / sample_rate as f32;
            let mut current_windchest_mods: HashMap<String, f32> = HashMap::new();

            for (trem_id, trem_def) in &organ.tremulants {
                let is_active = *active_tremulants_ids.get(trem_id).unwrap_or(&false);
                let target_level = if is_active { 1.0 } else { 0.0 };
                let lfo = tremulant_lfos.entry(trem_id.clone()).or_insert(TremulantLfo { phase: 0.0, current_level: 0.0 });

                if lfo.current_level != target_level {
                    let rate = if is_active { if trem_def.start_rate > 0.0 { trem_def.start_rate } else { 1000.0 } } else { if trem_def.stop_rate > 0.0 { trem_def.stop_rate } else { 1000.0 } };
                    let change = rate * dt;
                    if lfo.current_level < target_level { lfo.current_level = (lfo.current_level + change).min(target_level); } else { lfo.current_level = (lfo.current_level - change).max(target_level); }
                }

                if lfo.current_level <= 0.0 && !is_active { continue; }

                let freq = if trem_def.period > 0.0 { 1000.0 / trem_def.period } else { 0.0 };
                let phase_inc = (freq * buffer_size_frames as f32) / sample_rate as f32;
                lfo.phase = (lfo.phase + phase_inc) % 1.0;

                let sine_val = (lfo.phase * std::f32::consts::TAU).sin();
                let inertia = lfo.current_level;

                // Subtractive Volume Modulation
                let am_swing = trem_def.amp_mod_depth * 0.01 * TREMULANT_AM_BOOST;
                let lfo_01 = (sine_val + 1.0) * 0.5; // 0.0 to 1.0
                let active_am = 1.0 - (am_swing * (1.0 - lfo_01));
                let final_am = 1.0 + (active_am - 1.0) * inertia;

                for wc_group in organ.windchest_groups.values() {
                    if wc_group.tremulant_ids.contains(trem_id) {
                         let existing = *current_windchest_mods.get(&wc_group.id_str).unwrap_or(&1.0);
                         let new_mod = existing * final_am;
                         current_windchest_mods.insert(wc_group.id_str.clone(), new_mod);
                         
                         let unpadded = wc_group.id_str.trim_start_matches('0');
                         let key_unpadded = if unpadded.is_empty() { "0" } else { unpadded };
                         if key_unpadded != wc_group.id_str { current_windchest_mods.insert(key_unpadded.to_string(), new_mod); }
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

            // --- Voice Processing Loop ---
            for (voice_id, voice) in voices.iter_mut() {
                if voice.is_fading_out && voice.fade_level <= 0.0001 { voices_to_remove.push(*voice_id); continue; }
                
                // Calculate Targets
                let (trem_start_am, trem_end_am) = if let Some(wc_id) = &voice.windchest_group_id {
                    let start = *prev_windchest_mods.get(wc_id).unwrap_or(&1.0);
                    let end = *current_windchest_mods.get(wc_id).unwrap_or(&1.0);
                    (start, end)
                } else { (1.0, 1.0) };

                let pitch_start = 1.0 + (trem_start_am - 1.0) * 0.1; 
                let pitch_end = 1.0 + (trem_end_am - 1.0) * 0.1;
                let avg_pitch = (pitch_start + pitch_end) * 0.5;
                
                // Buffer Management (Lazy Compaction)
                let needed_frames_float = buffer_size_frames as f32 * avg_pitch;
                let needed_frames = needed_frames_float.ceil() as usize + 2; 
                let needed_samples = needed_frames * CHANNEL_COUNT;

                // If the buffer is getting too full/fragmented, compact it now.
                // We keep valid data from buffer_start_idx onwards.
                if voice.buffer_start_idx + needed_samples > voice.input_buffer.capacity() {
                    // Move valid data to the start of the buffer
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
                    let _ = voice.consumer.pop_slice(&mut scratch_read_buffer[..read_samples]);
                    voice.input_buffer.extend_from_slice(&scratch_read_buffer[..read_samples]);
                }

                // Check actual available data (relative to our virtual start index)
                let total_valid_samples = voice.input_buffer.len() - voice.buffer_start_idx;
                if total_valid_samples < needed_samples {
                     if voice.is_finished.load(Ordering::Relaxed) { voices_to_remove.push(*voice_id); }
                     continue; 
                }

                // Setup Envelope
                let env_start = voice.fade_level;
                let mut env_end = env_start;
                if voice.is_fading_in { env_end = (env_start + voice.fade_increment * buffer_size_frames as f32).min(1.0); if env_end >= 1.0 { voice.is_fading_in = false; } }
                else if voice.is_fading_out { env_end = (env_start - voice.fade_increment * buffer_size_frames as f32).max(0.0); }
                voice.fade_level = env_end;

                let gain_delta = (trem_end_am * env_end - trem_start_am * env_start) / buffer_size_frames as f32;
                let mut current_gain_scalar = trem_start_am * env_start * voice.gain;
                
                let mix_chunks = mix_buffer.chunks_exact_mut(CHANNEL_COUNT);

                // Processing (Fast vs Slow Path)
                let is_fast_path = (avg_pitch - 1.0).abs() < 0.00001;

                // Get pointer to the *start* of valid data
                // unsafe { voice.input_buffer.get_unchecked(voice.buffer_start_idx) }
                let base_ptr = unsafe { voice.input_buffer.as_ptr().add(voice.buffer_start_idx) };

                if is_fast_path {
                    // --- FAST PATH ---
                    // Snap cursor to discard drift
                    let start_offset = voice.cursor_pos.round() as usize * CHANNEL_COUNT;
                    
                    // Simple linear loop, compiler can vectorise this easily
                    for (mix, i) in mix_chunks.zip(0..buffer_size_frames) {
                        let sample_idx = start_offset + (i * CHANNEL_COUNT);
                        unsafe {
                            let l = *base_ptr.add(sample_idx);
                            let r = *base_ptr.add(sample_idx + 1);
                            mix[0] += l * current_gain_scalar;
                            mix[1] += r * current_gain_scalar;
                        }
                        current_gain_scalar += gain_delta;
                    }
                    
                    // Advance cursor
                    voice.cursor_pos = (voice.cursor_pos.round() as usize + buffer_size_frames) as f32;

                } else {
                    // --- SLOW PATH ---
                    let pitch_delta = (pitch_end - pitch_start) / buffer_size_frames as f32;
                    let mut current_pitch_rate = pitch_start;

                    for mix in mix_chunks {
                        let idx = voice.cursor_pos.floor() as usize;
                        let frac = voice.cursor_pos - idx as f32;
                        let idx_stereo = idx * CHANNEL_COUNT;

                        unsafe {
                            // Reads are relative to base_ptr (buffer_start_idx)
                            let s0_l = *base_ptr.add(idx_stereo);
                            let s0_r = *base_ptr.add(idx_stereo + 1);
                            let s1_l = *base_ptr.add(idx_stereo + 2);
                            let s1_r = *base_ptr.add(idx_stereo + 3);

                            let out_l = s0_l + (s1_l - s0_l) * frac;
                            let out_r = s0_r + (s1_r - s0_r) * frac;

                            mix[0] += out_l * current_gain_scalar;
                            mix[1] += out_r * current_gain_scalar;
                        }

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
                    // Just removing calls Drop, which cancels the detached thread
                    voices.remove(vid);
                }
                voices_to_remove.clear();
            }

            // Reverb & Gain
             let apply_reverb = wet_dry_ratio > 0.0 && convolver.is_loaded;
            if apply_reverb {
                for i in 0..buffer_size_frames { reverb_dry_l[i] = mix_buffer[i*2]; reverb_dry_r[i] = mix_buffer[i*2+1]; }
                convolver.process(&reverb_dry_l, &reverb_dry_r, &mut wet_buffer_l, &mut wet_buffer_r);
                let dl = (1.0 - wet_dry_ratio) * system_gain;
                let wl = wet_dry_ratio * system_gain;
                 for i in 0..buffer_size_frames {
                    mix_buffer[i*2] = (mix_buffer[i*2] * dl) + (wet_buffer_l[i] * wl);
                    mix_buffer[i*2+1] = (mix_buffer[i*2+1] * dl) + (wet_buffer_r[i] * wl);
                }
            } else {
                 for s in mix_buffer.iter_mut() { *s *= system_gain; }
            }

            if let Some(rec) = &mut audio_recorder {
                rec.push(&mix_buffer);
            }

            // Load reporting
            let duration = start_time.elapsed();
             let load = duration.as_secs_f32() / buffer_duration_secs;
             if load > max_load_accumulator { max_load_accumulator = load; }
             
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

            let mut offset = 0;
            let needed = mix_buffer.len();
            while offset < needed {
                let pushed = producer.push_slice(&mix_buffer[offset..needed]);
                offset += pushed;
                if offset < needed { thread::sleep(Duration::from_millis(1)); }
            }
        }
    });
}

fn process_note_on(
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

// Process everything EXCEPT NoteOn
fn process_message(
    msg: AppMessage,
    wet_dry_ratio: &mut f32,
    system_gain: &mut f32,
    polyphony: &mut usize,
    ir_loader_tx: &mpsc::Sender<Result<StereoConvolver>>,
    sample_rate: u32,
    buffer_size_frames: usize,
    active_notes: &mut HashMap<u8, Vec<ActiveNote>>,
    organ: &Arc<Organ>,
    voices: &mut HashMap<u64, Voice>,
    voice_counter: &mut u64,
    stop_map: &HashMap<String, usize>,
    spawner_tx: &mpsc::Sender<SpawnJob>,
    pending_queue: &mut VecDeque<AppMessage>
) {
    match msg {
        AppMessage::NoteOff(n, s) => {
            // Check if this note is still waiting in the pending queue
            // If we find it there, we delete it. It effectively "never happened" (staccato).
            // No need to play a release sample because the attack never started.
            let mut removed_from_queue = false;
            if !pending_queue.is_empty() {
                // retain loops through the queue. returning 'false' removes the item.
                pending_queue.retain(|pending_msg| {
                    if let AppMessage::NoteOn(pending_note, _, pending_stop) = pending_msg {
                        if *pending_note == n && *pending_stop == s {
                            removed_from_queue = true;
                            return false; // Remove this NoteOn!
                    }
                    }
                    true // Keep other messages
                });
            }

            // Standard NoteOff logic (only if we didn't just kill it in the queue)
            if let Some(idx) = stop_map.get(&s) {
                if let Some(list) = active_notes.get_mut(&n) {
                    // Find the active note corresponding to this stop
                    if let Some(pos) = list.iter().position(|an| an.stop_index == *idx) {
                        let stopped = list.remove(pos);
                        if list.is_empty() { active_notes.remove(&n); }
                        trigger_note_release(stopped, organ, voices, sample_rate, voice_counter, spawner_tx);
                    }
                }
            }
        },
        AppMessage::AllNotesOff => { 
                pending_queue.clear();
                let notes: Vec<u8> = active_notes.keys().cloned().collect();
                for note in notes { handle_note_off(note, organ, voices, active_notes, sample_rate, voice_counter, spawner_tx); }
        },
        AppMessage::SetReverbWetDry(r) => *wet_dry_ratio = r.clamp(0.0, 1.0),
        AppMessage::SetReverbIr(p) => { let tx = ir_loader_tx.clone(); thread::spawn(move || { let _ = tx.send(StereoConvolver::from_file(&p, sample_rate, buffer_size_frames)); }); },
        AppMessage::SetGain(g) => *system_gain = g,
        AppMessage::SetPolyphony(p) => *polyphony = p,
        AppMessage::Quit => { std::process::exit(0); } // Quick exit
        _ => {}
    }
}

/// Helper function to handle Note Off logic
fn handle_note_off(
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
                spawner_tx
            );
        }
    }
}

fn run_loader_job(mut job: SpawnJob) {
    // Check if the note was stopped before we even started loading
    if job.is_cancelled.load(Ordering::Relaxed) {
        // Just mark finished so resources are cleaned up, but don't do IO
        job.is_finished.store(true, Ordering::SeqCst);
        return; 
    }

    let path_str_clone = job.path.file_name().unwrap_or_default().to_string_lossy().to_string();
    
    // We can't access "self" variables from Voice here, so they are passed in SpawnJob
    let panic_result = std::panic::catch_unwind(move || {
        let result: Result<()> = (|| {
            // Check cache
            let maybe_cached_data = job.organ.sample_cache.as_ref().and_then(|c| c.get(&job.path).cloned());
            let maybe_cached_meta = job.organ.metadata_cache.as_ref().and_then(|c| c.get(&job.path).cloned());

            let loop_info;
            let input_channels;
            let mut source: Option<Box<dyn Iterator<Item = f32>>> = None;
            let mut source_is_finished;
            let use_memory_reader;
            let mut samples_in_memory: Vec<f32> = Vec::new();
            
            let mut interleaved_buffer = vec![0.0f32; 1024 * CHANNEL_COUNT];
            let frames_to_skip = job.frames_to_skip;

            if let (Some(cached_samples), Some(cached_metadata)) = (maybe_cached_data, maybe_cached_meta) {
                samples_in_memory = (*cached_samples).clone();
                loop_info = if job.is_attack_sample { cached_metadata.loop_info } else { None };
                input_channels = cached_metadata.channel_count as usize;
                use_memory_reader = true;
                source_is_finished = false;
            } else {
                let file = File::open(&job.path)?;
                let mut reader = BufReader::new(file);
                let (fmt, other_chunks, data_start, data_size) = parse_wav_metadata(&mut reader, &job.path)?;
                
                if fmt.sample_rate != job.sample_rate { return Err(anyhow!("Rate mismatch")); }
                
                let mut loop_info_from_file = None;
                for chunk in other_chunks {
                    if &chunk.id == b"smpl" { loop_info_from_file = parse_smpl_chunk(&chunk.data); break; }
                }
                loop_info = if job.is_attack_sample { loop_info_from_file } else { None };
                input_channels = fmt.num_channels as usize;

                let decoder = WavSampleReader::new(reader, fmt, data_start, data_size)?;
                
                if job.is_attack_sample && loop_info.is_some() {
                    samples_in_memory = decoder.collect();
                    use_memory_reader = true;
                    source_is_finished = false;
                } else {
                    let mut iterator = Box::new(decoder);
                    
                    // If we skip to EOF, mark source as finished immediately.
                    let mut skip_successful = true;
                    if frames_to_skip > 0 {
                        let samples_to_skip = frames_to_skip * input_channels;
                        for _ in 0..samples_to_skip { 
                            if iterator.next().is_none() { 
                                skip_successful = false; 
                                break; 
                            } 
                        }
                    }
                    
                    if !skip_successful {
                        source_is_finished = true;
                    } else {
                        source_is_finished = false;
                    }
                    
                    source = Some(iterator);
                    use_memory_reader = false;
                }
            }

            let is_mono = input_channels == 1;
            let mut current_frame_index: usize = frames_to_skip; 
            let mut loop_start_frame: usize = 0;
            let mut loop_end_frame: usize = 0;
            let mut is_looping_sample = job.is_attack_sample && loop_info.is_some();

            if use_memory_reader && is_looping_sample {
                let (start, end) = loop_info.unwrap();
                loop_start_frame = start as usize;
                let total_frames = samples_in_memory.len() / input_channels;
                loop_end_frame = if end == 0 { total_frames } else { end as usize };
                if loop_start_frame >= loop_end_frame || loop_end_frame > total_frames {
                    is_looping_sample = false;
                    current_frame_index = 0;
                }
            }

            'loader_loop: loop {
                if job.is_cancelled.load(Ordering::Relaxed) { break 'loader_loop; }

                let frames_to_read = 1024;
                let mut frames_read = 0;

                if use_memory_reader {
                    for i in 0..frames_to_read {
                        if is_looping_sample {
                            if current_frame_index >= loop_end_frame { current_frame_index = loop_start_frame; }
                        } else {
                            if current_frame_index >= (samples_in_memory.len() / input_channels) { source_is_finished = true; break; }
                        }
                        let sample_l_idx = current_frame_index * input_channels;
                        let sample_l = samples_in_memory.get(sample_l_idx).cloned().unwrap_or(0.0);
                        let sample_r = if is_mono { sample_l } else { samples_in_memory.get(sample_l_idx + 1).cloned().unwrap_or(0.0) };
                        interleaved_buffer[i * CHANNEL_COUNT] = sample_l;
                        interleaved_buffer[i * CHANNEL_COUNT + 1] = sample_r;
                        current_frame_index += 1;
                        frames_read += 1;
                    }
                } else {
                    if !source_is_finished {
                        if let Some(ref mut s_iter) = source {
                            for i in 0..frames_to_read {
                                if let Some(sample_l) = s_iter.next() {
                                    let sample_r = if is_mono { sample_l } else { s_iter.next().unwrap_or(0.0) };
                                    interleaved_buffer[i * CHANNEL_COUNT] = sample_l;
                                    interleaved_buffer[i * CHANNEL_COUNT + 1] = sample_r;
                                    frames_read += 1;
                                } else {
                                    source_is_finished = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                
                // Push to ringbuffer
                if frames_read > 0 {
                    let samples_to_push = frames_read * CHANNEL_COUNT;
                    let mut offset = 0;
                    while offset < samples_to_push {
                        if job.is_cancelled.load(Ordering::Relaxed) { break 'loader_loop; }
                        let pushed = job.producer.push_slice(&interleaved_buffer[offset..samples_to_push]);
                        offset += pushed;
                        // Throttle based on ringbuffer fullness
                        if offset < samples_to_push { thread::sleep(Duration::from_millis(1)); }
                    }
                }

                if source_is_finished && !is_looping_sample { break 'loader_loop; }
                if is_looping_sample && frames_read == 0 { thread::sleep(Duration::from_millis(1)); }
            }
            Ok(())
        })();
        if let Err(e) = result { log::error!("Loader error: {}", e); }
    });

    if let Err(e) = panic_result {
        log::error!("[LoaderThread] PANICKED for file {:?}: {:?}", path_str_clone, e);
    }

    job.is_finished.store(true, Ordering::SeqCst);
}

/// Sets up the cpal audio stream and spawns the processing thread.
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

    let target_sample_rate = SampleRate(sample_rate);

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
    spawn_audio_processing_thread(rx, producer, organ, sample_rate, buffer_size_frames, gain, polyphony, tui_tx.clone(), shared_midi_recorder);

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

            // Manually interleave the stereo data into the 6-channel output.
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

            // Notify the user interface about the underrun
            let _ = tui_tx.send(TuiMessage::AudioUnderrun);

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
