use anyhow::{anyhow, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write, Cursor};
use std::path::{Path, PathBuf};
use rubato::{Resampler, SincFixedIn, SincInterpolationType, SincInterpolationParameters, WindowFunction};

use crate::wav::{parse_smpl_chunk, WavFmt, OtherChunk};

const I16_MAX_F: f32 = 32768.0;  // 2^15
const I24_MAX_F: f32 = 8388608.0; // 2^23
const I32_MAX_F: f32 = 2147483648.0; // 2^31

#[derive(Debug)]
pub struct SampleMetadata {
    pub loop_info: Option<(u32, u32)>,
    pub channel_count: u16,
}

/// Helper to read a 24-bit sample from a reader
fn read_i24<R: Read>(reader: &mut R) -> std::io::Result<i32> {
    let b1 = reader.read_u8()? as i32;
    let b2 = reader.read_u8()? as i32;
    let b3 = reader.read_u8()? as i32;
    // Combine, then sign-extend from 24th bit
    let sample = (b1 | (b2 << 8) | (b3 << 16)) << 8 >> 8;
    Ok(sample)
}

/// Helper to read all audio data from a reader into f32 waves
fn read_f32_waves<R: Read>(
    mut reader: R, 
    format: WavFmt,
    data_size: u32
) -> Result<Vec<Vec<f32>>> {
    let bytes_per_sample = (format.bits_per_sample / 8) as u32;
    let num_frames = data_size / (bytes_per_sample * format.num_channels as u32);
    let num_channels = format.num_channels as usize;
    let mut output_waves = vec![Vec::with_capacity(num_frames as usize); num_channels];
    
    for _ in 0..num_frames {
        for ch in 0..num_channels {
            let sample_f32 = match (format.audio_format, format.bits_per_sample) {
                (1, 16) => (reader.read_i16::<LittleEndian>()? as f32) / I16_MAX_F,
                (1, 24) => (read_i24(&mut reader)? as f32) / I24_MAX_F,
                (1, 32) => (reader.read_i32::<LittleEndian>()? as f32) / I32_MAX_F,
                (3, 32) => reader.read_f32::<LittleEndian>()?,
                _ => return Err(anyhow!("Unsupported read format: {}/{}", format.audio_format, format.bits_per_sample)),
            };
            output_waves[ch].push(sample_f32);
        }
    }
    Ok(output_waves)
}

/// Helper to convert f32 waves into an interleaved byte buffer
fn write_f32_waves_to_bytes(
    waves: &[Vec<f32>],
    target_bits: u16,
    target_is_float: bool
) -> Result<Vec<u8>> {
    if waves.is_empty() || waves[0].is_empty() {
        return Ok(Vec::new());
    }
    
    let num_channels = waves.len();
    let num_frames = waves[0].len();
    let bytes_per_sample = (target_bits / 8) as usize;
    let mut output_bytes = Vec::with_capacity(num_frames * num_channels * bytes_per_sample);

    for i in 0..num_frames {
        for ch in 0..num_channels {
            let sample_f32 = waves[ch][i];
            
            match (target_is_float, target_bits) {
                (true, 32) => {
                    output_bytes.write_f32::<LittleEndian>(sample_f32)?;
                },
                (false, 16) => {
                    let sample_i16 = (sample_f32.clamp(-1.0, 1.0) * (I16_MAX_F - 1.0)) as i16;
                    output_bytes.write_i16::<LittleEndian>(sample_i16)?;
                },
                (false, 24) => {
                      let sample_i32 = (sample_f32.clamp(-1.0, 1.0) * (I24_MAX_F - 1.0)) as i32;
                      output_bytes.write_i24::<LittleEndian>(sample_i32)?;
                },
                (false, 32) => {
                    let sample_i32 = (sample_f32.clamp(-1.0, 1.0) * (I32_MAX_F - 1.0)) as i32;
                    output_bytes.write_i32::<LittleEndian>(sample_i32)?;
                },
                _ => return Err(anyhow!("Invalid target format combination")),
            }
        }
    }
    Ok(output_bytes)
}

/// Helper function to scale loop points within a 'smpl' chunk's binary data.
/// This modifies the `smpl_data` buffer in place.
fn scale_smpl_chunk_loops(
    smpl_data: &mut Vec<u8>,
    ratio: f64,
    new_sample_rate: u32
) -> Result<()> {
    // smpl chunk data is complex. We need to parse it carefully.
    // We'll use a cursor to read and write in place.
    if smpl_data.len() < 36 {
        // Not enough data for header (up to cSampleLoops)
        return Err(anyhow!("'smpl' chunk too small to parse (< 36 bytes)"));
    }

    let mut cursor = Cursor::new(smpl_data);

    // --- Read and Write Header Fields ---
    // We just seek past the first 8 bytes (Manufacturer, Product)
    cursor.seek(SeekFrom::Start(8))?;
    
    // dwSamplePeriod (offset 8)
    // This is nanoseconds per sample: (1_000_000_000 / sample_rate)
    let new_sample_period = (1_000_000_000.0 / new_sample_rate as f64).round() as u32;
    cursor.write_u32::<LittleEndian>(new_sample_period)?;

    // Seek past MIDIUnityNote, MIDIPitchFraction, SMPTEFormat, SMPTEOffset (4*4 = 16 bytes)
    // Current pos is 12, so seek 16 more.
    cursor.seek(SeekFrom::Current(16))?; // Now at byte 28

    // cSampleLoops (offset 28)
    let num_loops = cursor.read_u32::<LittleEndian>()?;
    
    // cbSamplerData (offset 32)
    let _sampler_data_size = cursor.read_u32::<LittleEndian>()?;
    
    // Cursor is now at byte 36, the start of the loop array.
    
    if num_loops == 0 {
        return Ok(()); // No loops to modify
    }

    // Check if we have enough data for all loops
    let loop_array_size = num_loops as u64 * 24; // Each loop is 6 * u32 = 24 bytes
    let header_size = 36_u64;
    let expected_min_size = header_size + loop_array_size;

    if (cursor.get_ref().len() as u64) < expected_min_size {
        return Err(anyhow!(
            "Invalid 'smpl' chunk: header reports {} loops, but data is too small ({} < {})", 
            num_loops, cursor.get_ref().len(), expected_min_size
        ));
    }

    // --- Iterate and Modify Loops ---
    for i in 0..num_loops {
        let loop_start_pos = cursor.position();
        log::debug!("Scaling loop {}", i);

        // Seek past dwCuePointID (u32) and dwType (u32)
        cursor.seek(SeekFrom::Current(8))?; // Now at loop_start_pos + 8

        // dwStart (offset 8 in loop struct)
        let start_frame = cursor.read_u32::<LittleEndian>()?;
        let new_start_frame = (start_frame as f64 * ratio).round() as u32;

        // dwEnd (offset 12 in loop struct)
        let end_frame = cursor.read_u32::<LittleEndian>()?;
        let new_end_frame = (end_frame as f64 * ratio).round() as u32;

        log::debug!(
            "  Loop {}: Start {} -> {}, End {} -> {}",
            i, start_frame, new_start_frame, end_frame, new_end_frame
        );

        // We need to write the new values back.
        // Go back to the start of dwStart
        cursor.seek(SeekFrom::Start(loop_start_pos + 8))?;
        cursor.write_u32::<LittleEndian>(new_start_frame)?;
        cursor.write_u32::<LittleEndian>(new_end_frame)?;

        // Seek to the end of this loop struct (past dwFraction, dwPlayCount)
        // We are at loop_start_pos + 16, need to go to loop_start_pos + 24
        cursor.seek(SeekFrom::Start(loop_start_pos + 24))?;
    }

    Ok(())
}

/// Parses WAV file metadata chunks.
pub fn parse_wav_metadata<R: Read + Seek>(
    reader: &mut R,
    full_path_for_logs: &Path,
) -> Result<(WavFmt, Vec<OtherChunk>, u64, u32)> {
    let mut riff_header = [0; 4];
    reader.read_exact(&mut riff_header)?;
    if &riff_header != b"RIFF" { return Err(anyhow!("Not a RIFF file: {:?}", full_path_for_logs)); }
    let _file_size = reader.read_u32::<LittleEndian>()?;
    let mut wave_header = [0; 4];
    reader.read_exact(&mut wave_header)?;
    if &wave_header != b"WAVE" { return Err(anyhow!("Not a WAVE file: {:?}", full_path_for_logs)); }

    let mut format_chunk: Option<WavFmt> = None;
    let mut data_chunk_info: Option<(u64, u32)> = None; // (offset, size)
    let mut other_chunks: Vec<OtherChunk> = Vec::new();

    while let Ok(chunk_id) = reader.read_u32::<LittleEndian>().map(|id| id.to_le_bytes()) {
        let chunk_size = reader.read_u32::<LittleEndian>()?;
        let chunk_data_start_pos = reader.stream_position()?;
        let next_chunk_aligned_pos =
            chunk_data_start_pos + (chunk_size as u64 + (chunk_size % 2) as u64);

        match &chunk_id {
            b"fmt " => {
                let mut fmt_data = vec![0; chunk_size as usize];
                reader.read_exact(&mut fmt_data)?;
                let mut cursor = Cursor::new(fmt_data);
                format_chunk = Some(WavFmt {
                    audio_format: cursor.read_u16::<LittleEndian>()?,
                    num_channels: cursor.read_u16::<LittleEndian>()?,
                    sample_rate: cursor.read_u32::<LittleEndian>()?,
                    bits_per_sample: {
                        cursor.seek(SeekFrom::Start(14))?;
                        cursor.read_u16::<LittleEndian>()?
                    },
                });
            }
            b"data" => {
                data_chunk_info = Some((chunk_data_start_pos, chunk_size));
            }
            _ => {
                let mut chunk_data = vec![0; chunk_size as usize];
                reader.read_exact(&mut chunk_data)?;
                other_chunks.push(OtherChunk { id: chunk_id, data: chunk_data });
            }
        }
        if reader.seek(SeekFrom::Start(next_chunk_aligned_pos)).is_err() {
            break; // Reached end of file
        }
    }
    
    let format = format_chunk.ok_or_else(|| anyhow!("File has no 'fmt ' chunk: {:?}", full_path_for_logs))?;
    let (data_offset, data_size) = data_chunk_info.ok_or_else(|| anyhow!("File has no 'data' chunk: {:?}", full_path_for_logs))?;

    Ok((format, other_chunks, data_offset, data_size))
}

/// Loads a sample and verifies it matches the target sample rate.
pub fn load_sample_as_f32(path: &Path, target_sample_rate: u32) -> Result<(Vec<f32>, SampleMetadata)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let (format, other_chunks, data_offset, data_size) = 
        parse_wav_metadata(&mut reader, path)?;

    // Sanity check
    if format.sample_rate != target_sample_rate {
        return Err(anyhow!(
            "Attempted to cache non-processed file: {:?} (File: {}Hz, Target: {}Hz)",
            path, format.sample_rate, target_sample_rate
        ));
    }

    let mut loop_info = None;
    for chunk in other_chunks {
        if &chunk.id == b"smpl" {
            loop_info = parse_smpl_chunk(&chunk.data);
            break;
        }
    }
    
    let metadata = SampleMetadata {
        loop_info,
        channel_count: format.num_channels,
    };

    reader.seek(SeekFrom::Start(data_offset))?;
    let waves = read_f32_waves(reader, format, data_size)?;

    if waves.is_empty() || waves[0].is_empty() {
        return Ok((Vec::new(), metadata));
    }
    
    let num_channels = waves.len();
    let num_frames = waves[0].len();
    let mut interleaved = vec![0.0f32; num_frames * num_channels];
    for i in 0..num_frames {
        for ch in 0..num_channels {
            interleaved[i * num_channels + ch] = waves[ch][i];
        }
    }
    
    Ok((interleaved, metadata))
}

/// Checks a .wav file. If processing is needed (rate, bit depth, tuning), creates a new file in the `cache_dir`.
pub fn process_sample_file(
    relative_path: &Path,
    base_dir: &Path,
    cache_dir: &Path,
    pitch_tuning_cents: f32,
    convert_to_16_bit: bool,
    target_sample_rate: u32,
) -> Result<PathBuf> {
    
    let full_source_path = base_dir.join(relative_path);
    if !full_source_path.exists() {
        return Err(anyhow!("Sample file not found: {:?}", full_source_path));
    }

    // Parse source
    let file = File::open(&full_source_path)?;
    let mut reader = BufReader::new(file);
    let (format, mut other_chunks, data_offset, data_size) = 
        parse_wav_metadata(&mut reader, &full_source_path)?;

    let target_bits = if convert_to_16_bit { 16 } else { format.bits_per_sample };
    let target_is_float = format.audio_format == 3 && !convert_to_16_bit;

    let needs_resample = format.sample_rate != target_sample_rate || pitch_tuning_cents != 0.0;
    let needs_bit_change = target_bits != format.bits_per_sample || (format.audio_format == 3 && !target_is_float);
    
    // Return original if no changes needed
    if !needs_resample && !needs_bit_change {
        return Ok(full_source_path);
    }
    
    // --- Generate Cache Filename ---
    let original_stem = relative_path.file_stem().unwrap_or_default().to_string_lossy();
    let original_ext = relative_path.extension().unwrap_or_default().to_string_lossy();

    let mut suffixes = Vec::new();
    
    // Include dynamic sample rate (e.g. "48000hz")
    suffixes.push(format!("{}hz", target_sample_rate));

    if target_bits != format.bits_per_sample {
        suffixes.push(format!("{}b", target_bits));
    }
    
    suffixes.push(format!("p{:+0.1}", pitch_tuning_cents));

    let new_file_name = format!("{}.{}.{}", original_stem, suffixes.join("."), original_ext);
    
    // Mirror directory structure in cache
    let parent_in_cache = if let Some(parent) = relative_path.parent() {
        cache_dir.join(parent)
    } else {
        cache_dir.to_path_buf()
    };
    
    let cache_full_path = parent_in_cache.join(new_file_name);

    if cache_full_path.exists() {
        return Ok(cache_full_path);
    }
    
    fs::create_dir_all(&parent_in_cache)?;

    log::info!(
        "[WavConvert] Processing -> {:?} (Target: {}Hz, {}bit, Pitch: {}c)",
        cache_full_path.file_name().unwrap_or_default(), 
        target_sample_rate, target_bits, pitch_tuning_cents
    );

    // Read
    let mut reader = reader.into_inner();
    reader.seek(SeekFrom::Start(data_offset))?;
    let data_reader = BufReader::new(reader);
    let input_waves = read_f32_waves(data_reader, format, data_size)?;

    let resample_ratio = if needs_resample {
        let pitch_factor = 2.0f64.powf(-pitch_tuning_cents as f64 / 1200.0);
        let effective_input_rate = format.sample_rate as f64 / pitch_factor;
        target_sample_rate as f64 / effective_input_rate
    } else {
        1.0
    };

    // Process/Resample
    let output_waves = if needs_resample {
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 160,
            window: WindowFunction::BlackmanHarris,
        };
        let mut resampler = SincFixedIn::<f32>::new(
            resample_ratio,
            1.0,
            params,
            input_waves[0].len(),
            input_waves.len(),
        )?;
        
        resampler.process(&input_waves, None)?
    } else {
        input_waves 
    };

    if needs_resample {
        for chunk in other_chunks.iter_mut() {
            if &chunk.id == b"smpl" {
                if let Err(e) = scale_smpl_chunk_loops(&mut chunk.data, resample_ratio, target_sample_rate) {
                    log::warn!("Failed to scale loop points: {}", e);
                }
                break; 
            }
        }
    }

    // Write
    let final_data_chunk = write_f32_waves_to_bytes(&output_waves, target_bits, target_is_float)?;
    let out_file = File::create(&cache_full_path)?;
    let mut writer = BufWriter::new(out_file);

    let new_data_size = final_data_chunk.len() as u32;
    let new_bits_per_sample: u16 = target_bits;
    let new_audio_format = if target_is_float { 3 } else { 1 };
    let new_block_align = format.num_channels * (new_bits_per_sample / 8);
    let new_byte_rate = target_sample_rate * new_block_align as u32; 
    
    let mut other_chunks_total_size: u32 = 0;
    for chunk in &other_chunks {
        other_chunks_total_size += 8; 
        let data_len = chunk.data.len() as u32;
        other_chunks_total_size += data_len + (data_len % 2); 
    }

    let new_riff_file_size = 4 + (8 + 16) + other_chunks_total_size + (8 + new_data_size);

    writer.write_all(b"RIFF")?;
    writer.write_u32::<LittleEndian>(new_riff_file_size)?;
    writer.write_all(b"WAVE")?;

    writer.write_all(b"fmt ")?;
    writer.write_u32::<LittleEndian>(16)?; 
    writer.write_u16::<LittleEndian>(new_audio_format)?;
    writer.write_u16::<LittleEndian>(format.num_channels)?;
    writer.write_u32::<LittleEndian>(target_sample_rate)?;
    writer.write_u32::<LittleEndian>(new_byte_rate)?;
    writer.write_u16::<LittleEndian>(new_block_align)?;
    writer.write_u16::<LittleEndian>(new_bits_per_sample)?;
    
    for chunk in &other_chunks {
        writer.write_all(&chunk.id)?;
        writer.write_u32::<LittleEndian>(chunk.data.len() as u32)?;
        writer.write_all(&chunk.data)?;
        if chunk.data.len() % 2 != 0 { writer.write_u8(0)?; }
    }

    writer.write_all(b"data")?;
    writer.write_u32::<LittleEndian>(new_data_size)?;
    writer.write_all(&final_data_chunk)?;
    writer.flush()?;

    Ok(cache_full_path)
}