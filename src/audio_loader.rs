use anyhow::{anyhow, Result};
use ringbuf::traits::Producer;
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::voice::{SpawnJob, CHANNEL_COUNT};
use crate::wav::{parse_smpl_chunk, parse_wav_metadata, WavSampleReader};

/// Worker function that loads samples from disk or cache and fills the ring buffer.
pub fn run_loader_job(mut job: SpawnJob) {
    // Check cancellation before doing heavy lifting
    if job.is_cancelled.load(Ordering::Relaxed) {
        job.is_finished.store(true, Ordering::SeqCst);
        return;
    }

    let path_str_clone = job
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Wrap in catch_unwind to prevent a loader panic from crashing the whole engine
    let panic_result = std::panic::catch_unwind(move || {
        let result: Result<()> = (|| {
            // Check Caches
            let maybe_cached_data = job
                .organ
                .sample_cache
                .as_ref()
                .and_then(|c| c.get(&job.path).cloned());
            let maybe_cached_meta = job
                .organ
                .metadata_cache
                .as_ref()
                .and_then(|c| c.get(&job.path).cloned());

            let loop_info;
            let input_channels;
            let mut source: Option<Box<dyn Iterator<Item = f32>>> = None;
            let mut source_is_finished;
            let use_memory_reader;
            let mut samples_in_memory: Vec<f32> = Vec::new();

            let mut interleaved_buffer = vec![0.0f32; 1024 * CHANNEL_COUNT];
            let frames_to_skip = job.frames_to_skip;

            if let (Some(cached_samples), Some(cached_metadata)) =
                (maybe_cached_data, maybe_cached_meta)
            {
                // Fast Path: Memory Cache
                samples_in_memory = (*cached_samples).clone();
                loop_info = if job.is_attack_sample {
                    cached_metadata.loop_info
                } else {
                    None
                };
                input_channels = cached_metadata.channel_count as usize;
                use_memory_reader = true;
                source_is_finished = false;
            } else {
                // Slow Path: Disk I/O
                let file = File::open(&job.path)?;
                let mut reader = BufReader::new(file);
                let (fmt, other_chunks, data_start, data_size) =
                    parse_wav_metadata(&mut reader, &job.path)?;

                if fmt.sample_rate != job.sample_rate {
                    return Err(anyhow!("Rate mismatch"));
                }

                let mut loop_info_from_file = None;
                for chunk in other_chunks {
                    if &chunk.id == b"smpl" {
                        loop_info_from_file = parse_smpl_chunk(&chunk.data);
                        break;
                    }
                }
                loop_info = if job.is_attack_sample {
                    loop_info_from_file
                } else {
                    None
                };
                input_channels = fmt.num_channels as usize;

                let decoder = WavSampleReader::new(reader, fmt, data_start, data_size)?;

                if job.is_attack_sample && loop_info.is_some() {
                    // Small looping samples must be fully loaded into memory
                    samples_in_memory = decoder.collect();
                    use_memory_reader = true;
                    source_is_finished = false;
                } else {
                    // Long one-shot samples are streamed
                    let mut iterator = Box::new(decoder);

                    // Skip frames (e.g. if we had preloaded bytes)
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

                    source_is_finished = !skip_successful;
                    source = Some(iterator);
                    use_memory_reader = false;
                }
            }

            let is_mono = input_channels == 1;
            let mut current_frame_index: usize = frames_to_skip;
            let mut loop_start_frame: usize = 0;
            let mut loop_end_frame: usize = 0;
            let mut is_looping_sample = job.is_attack_sample && loop_info.is_some();

            // Validate loop points against loaded data size
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

            // Streaming Loop
            'loader_loop: loop {
                if job.is_cancelled.load(Ordering::Relaxed) {
                    break 'loader_loop;
                }

                let frames_to_read = 1024;
                let mut frames_read = 0;

                if use_memory_reader {
                    for i in 0..frames_to_read {
                        if is_looping_sample {
                            if current_frame_index >= loop_end_frame {
                                current_frame_index = loop_start_frame;
                            }
                        } else {
                            if current_frame_index >= (samples_in_memory.len() / input_channels) {
                                source_is_finished = true;
                                break;
                            }
                        }

                        let sample_l_idx = current_frame_index * input_channels;
                        // Manual safety checks removed for brevity, but indices are bounded above
                        let sample_l = samples_in_memory.get(sample_l_idx).cloned().unwrap_or(0.0);
                        let sample_r = if is_mono {
                            sample_l
                        } else {
                            samples_in_memory
                                .get(sample_l_idx + 1)
                                .cloned()
                                .unwrap_or(0.0)
                        };

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
                                    let sample_r = if is_mono {
                                        sample_l
                                    } else {
                                        s_iter.next().unwrap_or(0.0)
                                    };
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

                // Push data to the ring buffer
                if frames_read > 0 {
                    let samples_to_push = frames_read * CHANNEL_COUNT;
                    let mut offset = 0;
                    while offset < samples_to_push {
                        if job.is_cancelled.load(Ordering::Relaxed) {
                            break 'loader_loop;
                        }

                        let pushed = job
                            .producer
                            .push_slice(&interleaved_buffer[offset..samples_to_push]);
                        offset += pushed;

                        // Backpressure: If ring buffer is full, sleep briefly
                        if offset < samples_to_push {
                            thread::sleep(Duration::from_millis(1));
                        }
                    }
                }

                if source_is_finished && !is_looping_sample {
                    break 'loader_loop;
                }

                // If looping but we read 0 frames (maybe waiting for something?), prevent cpu spin
                if is_looping_sample && frames_read == 0 {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            log::error!("Loader error: {}", e);
        }
    });

    if let Err(e) = panic_result {
        log::error!(
            "[LoaderThread] PANICKED for file {:?}: {:?}",
            path_str_clone,
            e
        );
    }

    job.is_finished.store(true, Ordering::SeqCst);
}
