use anyhow::Result;
use decibel::{AmplitudeRatio, DecibelRatio};
use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::organ::Organ;

// Common Audio Constants
pub const CHANNEL_COUNT: usize = 2;
pub const VOICE_BUFFER_FRAMES: usize = 14400;
pub const CROSSFADE_TIME: f32 = 0.10;
pub const VOICE_STEALING_FADE_TIME: f32 = 1.00;
pub const MAX_NEW_VOICES_PER_BLOCK: usize = 28;
pub const TREMULANT_AM_BOOST: f32 = 1.0;

pub struct TremulantLfo {
    pub phase: f32,
    pub current_level: f32,
}

pub struct SpawnJob {
    pub path: PathBuf,
    pub organ: Arc<Organ>,
    pub sample_rate: u32,
    pub is_attack_sample: bool,
    pub frames_to_skip: usize,
    pub producer: HeapProd<f32>,
    pub is_finished: Arc<AtomicBool>,
    pub is_cancelled: Arc<AtomicBool>,
}

/// Represents one playing sample, either attack or release.
pub struct Voice {
    pub gain: f32,
    pub consumer: HeapCons<f32>,
    pub is_finished: Arc<AtomicBool>,
    pub is_cancelled: Arc<AtomicBool>,

    pub fade_level: f32,
    pub is_fading_out: bool,
    pub is_fading_in: bool,
    pub is_awaiting_release_sample: bool,
    pub release_voice_id: Option<u64>,

    pub note_on_time: Instant,
    pub is_attack_sample: bool,
    pub fade_increment: f32,

    pub windchest_group_id: Option<String>,

    pub input_buffer: Vec<f32>,
    pub buffer_start_idx: usize,
    pub cursor_pos: f32,
}

impl Voice {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn new(
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
        let fade_increment = if fade_frames > 0 {
            1.0 / fade_frames as f32
        } else {
            1.0
        };

        let amplitude_ratio: AmplitudeRatio<f64> = DecibelRatio(gain_db as f64).into();
        let gain = amplitude_ratio.amplitude_value() as f32;

        let ring_buf = HeapRb::<f32>::new(VOICE_BUFFER_FRAMES * CHANNEL_COUNT);
        let (mut producer, consumer) = ring_buf.split();

        let is_finished = Arc::new(AtomicBool::new(false));
        let is_cancelled = Arc::new(AtomicBool::new(false));

        let mut preloaded_frames_count = 0;
        if let Some(ref preloaded) = preloaded_bytes {
            let pushed = producer.push_slice(preloaded);
            preloaded_frames_count = pushed / CHANNEL_COUNT;
        }

        let job = SpawnJob {
            path: path.to_path_buf(),
            organ: Arc::clone(&organ),
            sample_rate,
            is_attack_sample,
            frames_to_skip: preloaded_frames_count,
            producer,
            is_finished: Arc::clone(&is_finished),
            is_cancelled: Arc::clone(&is_cancelled),
        };

        if let Err(e) = spawner_tx.send(job) {
            log::error!("Failed to queue voice spawn job: {}", e);
            is_finished.store(true, Ordering::Relaxed);
        }

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

impl Drop for Voice {
    fn drop(&mut self) {
        self.is_cancelled.store(true, Ordering::SeqCst);
    }
}
