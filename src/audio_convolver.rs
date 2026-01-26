use anyhow::{anyhow, Result};
use fft_convolver::FFTConvolver;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use crate::wav::{parse_wav_metadata, WavSampleReader};

/// Simple Linear Interpolation Resampler.
pub fn resample_interleaved(
    input: &[f32],
    channels: usize,
    from_rate: u32,
    to_rate: u32,
) -> Vec<f32> {
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
            let interpolated = s0 + (s1 - s0) * frac;
            output.push(interpolated);
        }
    }

    log::info!(
        "[Resampler] Resampled IR from {}Hz to {}Hz. ({} -> {} frames)",
        from_rate,
        to_rate,
        input_frames,
        output_frames
    );

    output
}

/// A stereo FFT convolver for reverb processing.
pub struct StereoConvolver {
    convolver_l: FFTConvolver<f32>,
    convolver_r: FFTConvolver<f32>,
    pub is_loaded: bool,
    block_size: usize,
}

impl StereoConvolver {
    pub fn new(block_size: usize) -> Self {
        Self {
            convolver_l: FFTConvolver::<f32>::default(),
            convolver_r: FFTConvolver::<f32>::default(),
            is_loaded: false,
            block_size,
        }
    }

    pub fn from_file(path: &Path, sample_rate: u32, block_size: usize) -> Result<Self> {
        log::info!("[Convolver] Loading IR from {:?}", path);

        let file = File::open(path)
            .map_err(|e| anyhow!("[Convolver] Failed to open IR {:?}: {}", path, e))?;
        let mut reader = BufReader::new(file);

        let (fmt, _chunks, data_start, data_size) =
            parse_wav_metadata(&mut reader, path).map_err(|e| {
                anyhow!(
                    "[Convolver] Failed to parse IR metadata for {:?}: {}",
                    path,
                    e
                )
            })?;

        let decoder = WavSampleReader::new(reader, fmt, data_start, data_size).map_err(|e| {
            anyhow!(
                "[Convolver] Failed to create IR reader for {:?}: {}",
                path,
                e
            )
        })?;

        let mut ir_samples_interleaved: Vec<f32> = decoder.collect();
        if ir_samples_interleaved.is_empty() {
            return Err(anyhow!(
                "[Convolver] IR file {:?} contains no samples.",
                path
            ));
        }

        if fmt.sample_rate != sample_rate {
            log::warn!(
                "[Convolver] IR Rate Mismatch (File: {}, Engine: {}). Resampling...",
                fmt.sample_rate,
                sample_rate
            );
            ir_samples_interleaved = resample_interleaved(
                &ir_samples_interleaved,
                fmt.num_channels as usize,
                fmt.sample_rate,
                sample_rate,
            );
        }

        let mut ir_l: Vec<f32> = Vec::new();
        let mut ir_r: Vec<f32> = Vec::new();
        let ir_channels = fmt.num_channels as usize;

        if ir_channels == 1 {
            ir_l = ir_samples_interleaved;
            ir_r = ir_l.clone();
        } else {
            let num_frames = ir_samples_interleaved.len() / ir_channels;
            ir_l.reserve(num_frames);
            ir_r.reserve(num_frames);
            for i in 0..num_frames {
                ir_l.push(ir_samples_interleaved[i * ir_channels]);
                ir_r.push(ir_samples_interleaved[i * ir_channels + 1]);
            }
        }

        // Peak Normalization
        let max_l = ir_l.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        let max_r = ir_r.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        let global_peak = max_l.max(max_r);

        if global_peak > 0.0 {
            let target_peak = 0.015;
            let scale = target_peak / global_peak;

            log::debug!(
                "[Convolver] Normalizing IR. Input Peak: {:.4}, Scale Factor: {:.4}",
                global_peak,
                scale
            );

            for x in ir_l.iter_mut() {
                *x *= scale;
            }
            for x in ir_r.iter_mut() {
                *x *= scale;
            }
        } else {
            log::warn!("[Convolver] IR appears to be silent.");
        }

        let mut convolver_l = FFTConvolver::<f32>::default();
        let mut convolver_r = FFTConvolver::<f32>::default();

        let _ = convolver_l.init(block_size, &ir_l);
        let _ = convolver_r.init(block_size, &ir_r);

        log::info!("[Convolver] Successfully prepared IR.");

        Ok(Self {
            convolver_l,
            convolver_r,
            is_loaded: true,
            block_size,
        })
    }

    pub fn process(&mut self, dry_l: &[f32], dry_r: &[f32], wet_l: &mut [f32], wet_r: &mut [f32]) {
        if !self.is_loaded {
            wet_l.fill(0.0);
            wet_r.fill(0.0);
            return;
        }

        if dry_l.len() != self.block_size || dry_r.len() != self.block_size {
            log::error!("[Convolver] Block size mismatch!");
            wet_l.fill(0.0);
            wet_r.fill(0.0);
            return;
        }

        let _ = self.convolver_l.process(dry_l, wet_l);
        let _ = self.convolver_r.process(dry_r, wet_r);
    }
}
