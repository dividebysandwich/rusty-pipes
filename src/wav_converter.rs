use anyhow::{anyhow, Result, Context};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write, Cursor};
use std::path::{Path, PathBuf};
use rubato::{
    Resampler, SincInterpolationType, SincInterpolationParameters, 
    WindowFunction, Async, FixedAsync, Indexing
};
use audioadapter_buffers::direct::InterleavedSlice;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;
use symphonia::core::audio::SampleBuffer;

use crate::wav::{parse_smpl_chunk, WavFmt, OtherChunk, IsWavPackError};

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
                _ => return Err(anyhow!("Invalid target format combination: float={} bits={}", target_is_float, target_bits)),
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

/// Helper function to scale markers within a 'cue ' chunk's binary data.
fn scale_cue_chunk_markers(
    cue_data: &mut Vec<u8>,
    ratio: f64
) -> Result<()> {
    // A 'cue ' chunk structure:
    // dwCuePoints (4 bytes)
    // CuePoints array (24 bytes each)
    
    if cue_data.len() < 4 {
        return Err(anyhow!("'cue ' chunk too small (< 4 bytes)"));
    }

    let mut cursor = Cursor::new(cue_data);

    // Read number of cue points
    let num_points = cursor.read_u32::<LittleEndian>()?;
    
    // Validate size
    let expected_size = 4 + (num_points as u64 * 24);
    if (cursor.get_ref().len() as u64) < expected_size {
        return Err(anyhow!("'cue ' chunk too small for {} points", num_points));
    }

    for i in 0..num_points {
        let _point_start_pos = cursor.position();
        
        // Structure of a CuePoint (24 bytes):
        // 0: dwName (ID) - u32
        // 4: dwPosition (Play Order Position) - u32 <-- NEEDS SCALING
        // 8: fccChunk (Chunk ID, e.g. 'data') - [u8; 4]
        // 12: dwChunkStart (Chunk Start) - u32
        // 16: dwBlockStart (Block Start) - u32
        // 20: dwSampleOffset (Sample Offset) - u32 <-- NEEDS SCALING

        // Skip dwName (4 bytes)
        cursor.seek(SeekFrom::Current(4))?; 

        // Read & Scale dwPosition
        let pos = cursor.read_u32::<LittleEndian>()?;
        let new_pos = (pos as f64 * ratio).round() as u32;
        
        // Write back dwPosition
        cursor.seek(SeekFrom::Current(-4))?;
        cursor.write_u32::<LittleEndian>(new_pos)?;

        // Skip fccChunk (4), dwChunkStart (4), dwBlockStart (4) -> 12 bytes total
        cursor.seek(SeekFrom::Current(12))?;

        // Read & Scale dwSampleOffset
        // In simple PCM WAVs, this is usually identical to dwPosition.
        let offset = cursor.read_u32::<LittleEndian>()?;
        let new_offset = (offset as f64 * ratio).round() as u32;

        // Write back dwSampleOffset
        cursor.seek(SeekFrom::Current(-4))?;
        cursor.write_u32::<LittleEndian>(new_offset)?;

        // Loop is done, cursor is naturally at the start of the next point
        log::debug!("[WavConvert] Scaled Cue Point {}: {} -> {}", i, pos, new_pos);
    }

    Ok(())
}

// --- WAVPACK SUPPORT ---

/// Fast probe to get metadata without decoding the whole file.
fn peek_wavpack_info(path: &Path) -> Result<(u32, u16, u16, bool)> {
    let src = File::open(path).with_context(|| format!("Failed to open WavPack file for peeking: {:?}", path))?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    
    let mut hint = Hint::new();
    if let Some(ext) = path.extension() {
        hint.with_extension(&ext.to_string_lossy());
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &Default::default(), &Default::default())
        .with_context(|| format!("Failed to probe WavPack format: {:?}", path))?;
        
    let format = probed.format;
    let track = format.default_track().ok_or_else(|| anyhow!("No default track in WavPack file"))?;
    let params = track.codec_params.clone();
    
    let sample_rate = params.sample_rate.unwrap_or(48000);
    let channels = params.channels.map_or(2, |c| c.count() as u16);
    let bits_per_sample = params.bits_per_sample.unwrap_or(24) as u16; 
    
    // WavPack decodes to float in our pipeline, but source might be integer. 
    // We treat it as potential float for conversion logic purposes.
    let is_float = true; 

    Ok((sample_rate, channels, bits_per_sample, is_float))
}

/// Uses Symphonia to read audio data. This supports WavPack and others.
/// Returns (Interleaved Samples, Sample Rate, Channel Count, BitsPerSample)
fn read_wavpack_file(path: &Path) -> Result<(Vec<Vec<f32>>, u32, u16, u16)> {
    let src = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    
    let mut hint = Hint::new();
    if let Some(ext) = path.extension() {
        hint.with_extension(&ext.to_string_lossy());
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &Default::default(), &Default::default())?;
    let mut format = probed.format;
    
    let track = format.default_track().ok_or_else(|| anyhow!("No default track in WavPack file"))?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    
    let sample_rate = params.sample_rate.unwrap_or(48000);
    let channels = params.channels.map_or(2, |c| c.count() as u16);
    // WavPack bits_per_sample might be None, default to 24 for safety if unknown
    let bits_per_sample = params.bits_per_sample.unwrap_or(24) as u16; 

    let mut decoder = symphonia::default::get_codecs().make(&params, &Default::default())?;
    
    let mut output_waves = vec![Vec::new(); channels as usize];

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(e)) => {
                // Check if it's an unexpected EOF vs a normal one
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    if !output_waves.is_empty() && !output_waves[0].is_empty() {
                        log::warn!("Partial read of {:?}: Unexpected EOF, but recovered {} frames. Continuing.", path, output_waves[0].len());
                        break;
                    }
                    return Err(anyhow!("Unexpected End of Stream while decoding {:?}", path))   
                }
                break; // Normal EOF
            }, 
            Err(e) => return Err(anyhow!("Symphonia decode error in {:?}: {}", path, e)),
        };

        if packet.track_id() != track_id { continue; }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let duration = decoded.capacity() as u64;
                let mut sample_buf = SampleBuffer::<f32>::new(duration, spec);
                sample_buf.copy_interleaved_ref(decoded);

                let samples = sample_buf.samples();
                // De-interleave
                for (i, sample) in samples.iter().enumerate() {
                    let ch = i % (channels as usize);
                    output_waves[ch].push(*sample);
                }
            },
            Err(e) => return Err(anyhow!("Decode packet error: {}", e)),
        }
    }

    Ok((output_waves, sample_rate, channels, bits_per_sample))
}

/// Loads a sample and verifies it matches the target sample rate.
pub fn load_sample_as_f32(path: &Path, target_sample_rate: u32) -> Result<(Vec<f32>, SampleMetadata)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    // Try parsing as standard WAV
    let parse_result = crate::wav::parse_wav_metadata(&mut reader, path);
    
    match parse_result {
        Ok((format, other_chunks, data_offset, data_size)) => {
            // --- WAV PATH ---
            if format.sample_rate != target_sample_rate {
                return Err(anyhow!("Sample rate mismatch in cache: {} != {}", format.sample_rate, target_sample_rate));
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
            
            // Interleave
            let num_channels = waves.len();
            let num_frames = waves[0].len();
            let mut interleaved = vec![0.0f32; num_frames * num_channels];
            for i in 0..num_frames {
                for ch in 0..num_channels {
                    interleaved[i * num_channels + ch] = waves[ch][i];
                }
            }
            Ok((interleaved, metadata))
        },
        Err(e) => {
            // If it's a WavPack file, load using Symphonia
            if e.is::<IsWavPackError>() {
                log::debug!("Detected WavPack file: {:?}", path);
                let (waves, rate, channels, _) = read_wavpack_file(path)?;
                
                if rate != target_sample_rate {
                     return Err(anyhow!("Sample rate mismatch in cache (WV): {} != {}", rate, target_sample_rate));
                }

                // TODO: extracting loop points from WavPack is complex via Symphonia.
                // For now, we assume 0 loops or rely on ODF override.
                let metadata = SampleMetadata {
                    loop_info: None, 
                    channel_count: channels,
                };
                
                if waves.is_empty() || waves[0].is_empty() {
                    return Ok((Vec::new(), metadata));
                }

                // Interleave
                let num_frames = waves[0].len();
                let mut interleaved = vec![0.0f32; num_frames * channels as usize];
                for i in 0..num_frames {
                    for ch in 0..channels as usize {
                        interleaved[i * channels as usize + ch] = waves[ch][i];
                    }
                }
                Ok((interleaved, metadata))

            } else {
                // Real error
                Err(e)
            }
        }
    }
}

/// Checks and processes audio file. Supports WavPack input, always outputs WAV to cache.
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

    // --- Format Detection Phase ---
    // We need format details to decide if we skip processing.
    let mut input_waves: Vec<Vec<f32>> = Vec::new();
    let sample_rate;
    let bits_per_sample;
    let channels;
    let is_float;
    let mut other_chunks: Vec<OtherChunk> = Vec::new();

    let file = File::open(&full_source_path)
        .with_context(|| format!("Failed to open source file: {:?}", full_source_path))?;
    let mut reader = BufReader::new(file);

    match crate::wav::parse_wav_metadata(&mut reader, &full_source_path) {
        Ok((fmt, chunks, _data_offset, _data_size)) => {
            // It is a WAV
            sample_rate = fmt.sample_rate;
            bits_per_sample = fmt.bits_per_sample;
            channels = fmt.num_channels;
            is_float = fmt.audio_format == 3;
            other_chunks = chunks;
            input_waves = Vec::new(); 
        },
        Err(e) if e.is::<IsWavPackError>() => {
            let (rate, ch, bits, float) = peek_wavpack_info(&full_source_path)?;
            sample_rate = rate;
            channels = ch;
            bits_per_sample = bits;
            is_float = float;
        },
        Err(e) => return Err(e).with_context(|| format!("Failed to parse metadata for {:?}", full_source_path)),
    }

    let mut target_bits = if convert_to_16_bit { 16 } else { bits_per_sample };
    let target_is_float = is_float && !convert_to_16_bit; 
    if target_is_float { target_bits = 32; }

    let needs_resample = sample_rate != target_sample_rate || pitch_tuning_cents != 0.0;
    let needs_bit_change = target_bits != bits_per_sample || (is_float && !target_is_float);
    let is_source_wavpack = other_chunks.is_empty() && input_waves.is_empty(); 

    if !needs_resample && !needs_bit_change && !is_source_wavpack {
        return Ok(full_source_path);
    }
    
    // --- Generate Cache Filename ---
    let original_stem = relative_path.file_stem().unwrap_or_default().to_string_lossy();
    // Always use .wav extension for cache
    let new_file_name = format!("{}.{}hz.p{:+0.1}.{}b.wav", original_stem, target_sample_rate, pitch_tuning_cents, target_bits);
    
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
    log::info!("[WavConvert] Processing -> {:?}", cache_full_path.file_name().unwrap_or_default());

    // Now we actually read the data
    if is_source_wavpack {
        // Full decode of WavPack
        let (waves, _, _, _) = read_wavpack_file(&full_source_path)?;
        input_waves = waves;
    } else {
        let file = File::open(&full_source_path)?;
        let mut reader = BufReader::new(file);
        let (_, _, data_offset, data_size) = crate::wav::parse_wav_metadata(&mut reader, &full_source_path)?;
        let fmt = WavFmt { audio_format: if is_float { 3 } else { 1 }, num_channels: channels, sample_rate, bits_per_sample };
        reader.seek(SeekFrom::Start(data_offset))?;
        input_waves = read_f32_waves(reader, fmt, data_size)
            .with_context(|| format!("Failed to read PCM data from {:?}", full_source_path))?;
    }

    let resample_ratio = if needs_resample {
        let pitch_factor = 2.0f64.powf(-pitch_tuning_cents as f64 / 1200.0);
        let effective_input_rate = sample_rate as f64 / pitch_factor;
        target_sample_rate as f64 / effective_input_rate
    } else {
        1.0
    };

    // Process/Resample
    let output_waves = if needs_resample {
        let num_channels = input_waves.len();
        let num_input_frames = input_waves[0].len();

        // Interleave input data (rubato works best with interleaved buffers)
        let mut input_interleaved = Vec::with_capacity(num_input_frames * num_channels);
        for i in 0..num_input_frames {
            for ch in 0..num_channels {
                input_interleaved.push(input_waves[ch][i]);
            }
        }

        // Configure Async Resampler
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 160,
            window: WindowFunction::BlackmanHarris,
        };

        let chunk_size = 1024;
        let mut resampler = Async::<f32>::new_sinc(
            resample_ratio,
            1.1, // Max ratio
            &params,
            chunk_size,
            num_channels,
            FixedAsync::Input,
        ).unwrap();

        // Prepare Output Buffer
        let expected_output_frames = (num_input_frames as f64 * resample_ratio).ceil() as usize + chunk_size * 2;
        let mut output_interleaved = vec![0.0f32; expected_output_frames * num_channels];

        // Create Adapters
        let input_adapter = InterleavedSlice::new(&input_interleaved, num_channels, num_input_frames)
            .map_err(|e| anyhow!("Failed to create input adapter: {:?}", e))?;
        let mut output_adapter = InterleavedSlice::new_mut(&mut output_interleaved, num_channels, expected_output_frames)
            .map_err(|e| anyhow!("Failed to create output adapter: {:?}", e))?;

        // Process Loop
        let mut indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            active_channels_mask: None,
            partial_len: None,
        };
        
        let mut input_frames_next = resampler.input_frames_next();
        let mut input_frames_left = num_input_frames;

        while input_frames_left >= input_frames_next {
            let (nbr_in, nbr_out) = resampler
                .process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing))
                .map_err(|e| anyhow!("Resampling error: {:?}", e))?;

            indexing.input_offset += nbr_in;
            indexing.output_offset += nbr_out;
            input_frames_left -= nbr_in;
            input_frames_next = resampler.input_frames_next();
        }

        // Process remaining partial chunk
        if input_frames_left > 0 {
            indexing.partial_len = Some(input_frames_left);
            let (_nbr_in, nbr_out) = resampler
                .process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing))
                .map_err(|e| anyhow!("Resampling partial error: {:?}", e))?;
            indexing.output_offset += nbr_out;
        }

        let total_output_frames = indexing.output_offset;

        // De-interleave back to Planar (Vec<Vec<f32>>) for writing
        let mut result_waves = vec![Vec::with_capacity(total_output_frames); num_channels];
        for i in 0..total_output_frames {
            for ch in 0..num_channels {
                result_waves[ch].push(output_interleaved[i * num_channels + ch]);
            }
        }
        
        result_waves
    } else {
        input_waves 
    };

    // If we have 'smpl' chunks from a WAV source, scale them.
    // If source was WavPack, we currently lose loops unless we implement APE tag parsing.
    if needs_resample && !other_chunks.is_empty() {
        for chunk in other_chunks.iter_mut() {
            if &chunk.id == b"smpl" {
                let _ = scale_smpl_chunk_loops(&mut chunk.data, resample_ratio, target_sample_rate);
            } else if &chunk.id == b"cue " {
                if let Err(e) = scale_cue_chunk_markers(&mut chunk.data, resample_ratio) {
                     log::warn!("Failed to scale cue markers: {}", e);
                }
            }
        }
    }

    // Write to WAV
    let final_data_chunk = write_f32_waves_to_bytes(&output_waves, target_bits, target_is_float)?;
    let out_file = File::create(&cache_full_path)
        .with_context(|| format!("Failed to create cache file {:?}", cache_full_path))?;
    let mut writer = BufWriter::new(out_file);

    let new_data_size = final_data_chunk.len() as u32;
    let new_bits_per_sample: u16 = target_bits;
    let new_audio_format = if target_is_float { 3 } else { 1 };
    let new_block_align = channels * (new_bits_per_sample / 8);
    let new_byte_rate = target_sample_rate * new_block_align as u32; 
    
    let mut other_chunks_total_size: u32 = 0;
    for chunk in &other_chunks {
        other_chunks_total_size += 8 + chunk.data.len() as u32 + (chunk.data.len() % 2) as u32; 
    }

    let new_riff_file_size = 4 + (8 + 16) + other_chunks_total_size + (8 + new_data_size);

    writer.write_all(b"RIFF")?;
    writer.write_u32::<LittleEndian>(new_riff_file_size)?;
    writer.write_all(b"WAVE")?;

    writer.write_all(b"fmt ")?;
    writer.write_u32::<LittleEndian>(16)?; 
    writer.write_u16::<LittleEndian>(new_audio_format)?;
    writer.write_u16::<LittleEndian>(channels)?;
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

pub fn load_sample_head(path: &Path, target_sample_rate: u32, max_frames: usize) -> Result<Vec<f32>> {
    let file = File::open(path).with_context(|| format!("Failed to open sample head: {:?}", path))?;
    let mut reader = BufReader::new(file);
    log::debug!("Preloading {} frames from {:?}", max_frames, path);

    // Try parsing as WAV
    match crate::wav::parse_wav_metadata(&mut reader, path) {
        Ok((fmt, _chunks, data_offset, data_size)) => {
            // --- WAV PATH ---
            if fmt.sample_rate != target_sample_rate {
                return Err(anyhow!("Sample rate mismatch in head load: {} != {}", fmt.sample_rate, target_sample_rate));
            }

            let bytes_per_frame = (fmt.bits_per_sample / 8) as u32 * fmt.num_channels as u32;
            let total_frames = data_size / bytes_per_frame;
            let frames_to_read = (max_frames as u32).min(total_frames);

            // Seek to data
            reader.seek(SeekFrom::Start(data_offset))?;

            // We always output Stereo (2 channels) for the RingBuffer
            let mut interleaved_stereo = Vec::with_capacity(frames_to_read as usize * 2);

            for _ in 0..frames_to_read {
                // Read all channels for this frame
                let mut frame_samples = Vec::with_capacity(fmt.num_channels as usize);
                
                for _ in 0..fmt.num_channels {
                    let sample_f32 = match (fmt.audio_format, fmt.bits_per_sample) {
                        (1, 16) => (reader.read_i16::<LittleEndian>()? as f32) / I16_MAX_F,
                        (1, 24) => (read_i24(&mut reader)? as f32) / I24_MAX_F,
                        (1, 32) => (reader.read_i32::<LittleEndian>()? as f32) / I32_MAX_F,
                        (3, 32) => reader.read_f32::<LittleEndian>()?,
                        _ => 0.0, // Unsupported fallback
                    };
                    frame_samples.push(sample_f32);
                }

                // Push to output based on channel count
                if fmt.num_channels == 1 {
                    // Mono -> Stereo
                    interleaved_stereo.push(frame_samples[0]); // L
                    interleaved_stereo.push(frame_samples[0]); // R
                } else {
                    // Stereo (or take first 2 of multi-channel)
                    interleaved_stereo.push(frame_samples[0]); // L
                    interleaved_stereo.push(frame_samples[1]); // R
                }
            }

            Ok(interleaved_stereo)
        },
        Err(e) if e.is::<IsWavPackError>() => {
            // --- WAVPACK PATH ---
            // Use Symphonia, but stop early
            let src = File::open(path)?;
            let mss = MediaSourceStream::new(Box::new(src), Default::default());
            let mut hint = Hint::new();
            if let Some(ext) = path.extension() { hint.with_extension(&ext.to_string_lossy()); }

            let probed = symphonia::default::get_probe()
                .format(&hint, mss, &Default::default(), &Default::default())?;
            let mut format = probed.format;
            let track = format.default_track().ok_or_else(|| anyhow!("No track"))?;
            let track_id = track.id;
            let params = track.codec_params.clone();

            let sample_rate = params.sample_rate.unwrap_or(48000);
            if sample_rate != target_sample_rate {
                 return Err(anyhow!("Sample rate mismatch in head load (WV): {} != {}", sample_rate, target_sample_rate));
            }

            let mut decoder = symphonia::default::get_codecs().make(&params, &Default::default())?;
            let mut interleaved_stereo = Vec::with_capacity(max_frames * 2);
            let source_channels = params.channels.map_or(2, |c| c.count());

            // Decode loop
            while interleaved_stereo.len() < max_frames * 2 {
                let packet = match format.next_packet() {
                    Ok(p) => p,
                    Err(_) => break, // EOF
                };
                if packet.track_id() != track_id { continue; }

                if let Ok(decoded) = decoder.decode(&packet) {
                    let spec = *decoded.spec();
                    let duration = decoded.capacity() as u64;
                    let mut sample_buf = SampleBuffer::<f32>::new(duration, spec);
                    sample_buf.copy_interleaved_ref(decoded);
                    let samples = sample_buf.samples();

                    // Process these samples into our output
                    // samples is interleaved source data
                    for frame in samples.chunks(source_channels) {
                        if interleaved_stereo.len() >= max_frames * 2 { break; }
                        
                        if source_channels == 1 {
                            interleaved_stereo.push(frame[0]); // L
                            interleaved_stereo.push(frame[0]); // R
                        } else {
                            interleaved_stereo.push(frame[0]); // L
                            interleaved_stereo.push(frame[1]); // R
                        }
                    }
                }
            }
            
            Ok(interleaved_stereo)
        },
        Err(e) => Err(e).with_context(|| format!("Failed to parse metadata for head load: {:?}", path)),
    }
}

/// Attempts to extract a release tail from the attack sample if a valid CUE marker exists.
/// Returns Ok(Some(PathBuf)) if a release sample was generated.
pub fn try_extract_release_sample(
    relative_path: &Path,
    base_dir: &Path,
    cache_dir: &Path,
    pitch_tuning_cents: f32,
    convert_to_16_bit: bool,
    target_sample_rate: u32,
) -> Result<Option<PathBuf>> {
    
    let full_source_path = base_dir.join(relative_path);
    if !full_source_path.exists() { return Ok(None); }

    // Parse Metadata to find Loops and Cues
    let file = File::open(&full_source_path)?;
    let mut reader = BufReader::new(file);
    
    // We only support this for standard WAV files (legacy sets are usually WAV)
    let (fmt, chunks, data_offset, data_size) = match crate::wav::parse_wav_metadata(&mut reader, &full_source_path) {
        Ok(res) => res,
        Err(_) => return Ok(None), // Skip if not a valid WAV or is WavPack
    };

    // Find Loop End
    let mut loop_end = 0;
    for chunk in &chunks {
        if &chunk.id == b"smpl" {
            if let Some((_, end)) = crate::wav::parse_smpl_chunk(&chunk.data) {
                loop_end = end;
            }
        }
    }

    // If no loop, we probably shouldn't try to split it (it might be a one-shot percussive)
    if loop_end == 0 { return Ok(None); }

    // Find a valid Split Point (Cue)
    let mut split_point = None;
    for chunk in &chunks {
        if &chunk.id == b"cue " {
            let cues = crate::wav::parse_cue_chunk(&chunk.data);
            // We look for the first marker that is >= loop_end
            // This handles cases where the marker is exactly at the end of the loop
            for pos in cues {
                if pos >= loop_end {
                    split_point = Some(pos);
                    break;
                }
            }
        }
    }

    let Some(split_frame) = split_point else { return Ok(None); };

    // Generate Cache Filename (Distinct from the attack)
    let original_stem = relative_path.file_stem().unwrap_or_default().to_string_lossy();
    // We add ".rel" to the filename
    let target_bits = if convert_to_16_bit { 16 } else { fmt.bits_per_sample };
    let new_file_name = format!("{}.rel.{}hz.p{:+0.1}.{}b.wav", original_stem, target_sample_rate, pitch_tuning_cents, target_bits);
    
    let parent_in_cache = if let Some(parent) = relative_path.parent() {
        cache_dir.join(parent)
    } else {
        cache_dir.to_path_buf()
    };
    let cache_full_path = parent_in_cache.join(new_file_name);

    if cache_full_path.exists() {
        return Ok(Some(cache_full_path));
    }

    log::info!("[WavConvert] Extracting legacy release -> {:?}", cache_full_path.file_name().unwrap_or_default());

    // Read and Slice Audio
    reader.seek(SeekFrom::Start(data_offset))?;
    let full_waves = read_f32_waves(reader, fmt, data_size)?;

    if full_waves.is_empty() || split_frame as usize >= full_waves[0].len() {
        return Ok(None);
    }

    // Slice from split_point to end
    let mut sliced_waves = Vec::with_capacity(full_waves.len());
    for ch_data in &full_waves {
        sliced_waves.push(ch_data[split_frame as usize..].to_vec());
    }

    // Resample
    let needs_resample = fmt.sample_rate != target_sample_rate || pitch_tuning_cents.abs() > 0.0;

    let resample_ratio = if needs_resample {
        let pitch_factor = 2.0f64.powf(-pitch_tuning_cents as f64 / 1200.0);
        let effective_input_rate = fmt.sample_rate as f64 / pitch_factor;
        target_sample_rate as f64 / effective_input_rate
    } else {
        1.0
    };

    // Release Resampling
    let output_waves = if needs_resample {
        let num_channels = sliced_waves.len();
        let num_input_frames = sliced_waves[0].len();

        // Interleave
        let mut input_interleaved = Vec::with_capacity(num_input_frames * num_channels);
        for i in 0..num_input_frames {
            for ch in 0..num_channels {
                input_interleaved.push(sliced_waves[ch][i]);
            }
        }

        // Setup Async Resampler
        let params = SincInterpolationParameters {
            sinc_len: 64, f_cutoff: 0.95, interpolation: SincInterpolationType::Linear,
            oversampling_factor: 160, window: WindowFunction::BlackmanHarris,
        };

        let chunk_size = 1024;
        let mut resampler = Async::<f32>::new_sinc(
            resample_ratio, 
            1.1, 
            &params, 
            chunk_size, 
            num_channels, 
            FixedAsync::Input
        ).unwrap();
        
        // Output Buffer
        let expected_output_frames = (num_input_frames as f64 * resample_ratio).ceil() as usize + chunk_size * 2;
        let mut output_interleaved = vec![0.0f32; expected_output_frames * num_channels];

        // Adapters
        let input_adapter = InterleavedSlice::new(&input_interleaved, num_channels, num_input_frames).unwrap();
        let mut output_adapter = InterleavedSlice::new_mut(&mut output_interleaved, num_channels, expected_output_frames).unwrap();

        // Process
        let mut indexing = Indexing { input_offset: 0, output_offset: 0, active_channels_mask: None, partial_len: None };
        let mut frames_left = num_input_frames;
        let mut frames_next = resampler.input_frames_next();

        while frames_left >= frames_next {
            let (nin, nout) = resampler.process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing)).unwrap();
            indexing.input_offset += nin;
            indexing.output_offset += nout;
            frames_left -= nin;
            frames_next = resampler.input_frames_next();
        }

        if frames_left > 0 {
            indexing.partial_len = Some(frames_left);
            let (_, nout) = resampler.process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing)).unwrap();
            indexing.output_offset += nout;
        }

        let total_out = indexing.output_offset;

        // De-interleave
        let mut result_waves = vec![Vec::with_capacity(total_out); num_channels];
        for i in 0..total_out {
            for ch in 0..num_channels {
                result_waves[ch].push(output_interleaved[i * num_channels + ch]);
            }
        }
        result_waves
    } else {
        sliced_waves
    };

    // Write to Cache
    fs::create_dir_all(&parent_in_cache)?;
    let target_is_float = fmt.audio_format == 3 && !convert_to_16_bit;
    let target_bits = if target_is_float { 32 } else { target_bits }; // Ensure 32 for float

    let final_data = write_f32_waves_to_bytes(&output_waves, target_bits, target_is_float)?;
    
    let out_file = File::create(&cache_full_path)?;
    let mut writer = BufWriter::new(out_file);
    
    // Write minimal WAV header (no extra chunks needed for release tail)
    let channels = fmt.num_channels;
    let block_align = channels * (target_bits / 8);
    let byte_rate = target_sample_rate * block_align as u32;
    let riff_size = 36 + final_data.len() as u32;

    writer.write_all(b"RIFF")?;
    writer.write_u32::<LittleEndian>(riff_size)?;
    writer.write_all(b"WAVEfmt ")?;
    writer.write_u32::<LittleEndian>(16)?;
    writer.write_u16::<LittleEndian>(if target_is_float { 3 } else { 1 })?;
    writer.write_u16::<LittleEndian>(channels)?;
    writer.write_u32::<LittleEndian>(target_sample_rate)?;
    writer.write_u32::<LittleEndian>(byte_rate)?;
    writer.write_u16::<LittleEndian>(block_align)?;
    writer.write_u16::<LittleEndian>(target_bits)?;
    writer.write_all(b"data")?;
    writer.write_u32::<LittleEndian>(final_data.len() as u32)?;
    writer.write_all(&final_data)?;
    writer.flush()?;

    Ok(Some(cache_full_path))
}