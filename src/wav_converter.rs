use anyhow::{anyhow, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write, Cursor};
use std::path::{Path, PathBuf};
use rubato::{Resampler, SincFixedIn, SincInterpolationType, SincInterpolationParameters, WindowFunction};

const TARGET_SAMPLE_RATE: u32 = 48000;
const I16_MAX_F: f32 = 32768.0;  // 2^15
const I24_MAX_F: f32 = 8388608.0; // 2^23
const I32_MAX_F: f32 = 2147483648.0; // 2^31

/// A simple struct to hold the format info we care about.
#[derive(Debug, Clone, Copy)]
struct WavFormat {
    audio_format: u16,
    channel_count: u16,
    sampling_rate: u32,
    bits_per_sample: u16,
}

/// A struct to hold metadata chunks (like 'smpl') that we want to preserve.
#[derive(Debug, Clone)]
struct OtherChunk {
    id: [u8; 4],
    data: Vec<u8>,
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
    format: WavFormat, 
    data_size: u32
) -> Result<Vec<Vec<f32>>> {
    let bytes_per_sample = (format.bits_per_sample / 8) as u32;
    let num_frames = data_size / (bytes_per_sample * format.channel_count as u32);
    let num_channels = format.channel_count as usize;
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

/// Checks a .wav file. If `convert_to_16_bit` is true, `pitch_tuning_cents` is not 0,
/// or sample rate is not 48kHz, this creates a new processed .wav file.
pub fn process_sample_file(
    relative_path: &Path,
    base_dir: &Path,
    pitch_tuning_cents: f32,
    convert_to_16_bit: bool,
) -> Result<PathBuf> {
    
    let full_path = base_dir.join(relative_path);
    if !full_path.exists() {
        return Err(anyhow!("Sample file not found: {:?}", full_path));
    }

    // Manually parse the file
    let file = File::open(&full_path)?;
    let mut reader = BufReader::new(file);

    // Check RIFF header
    let mut riff_header = [0; 4];
    reader.read_exact(&mut riff_header)?;
    if &riff_header != b"RIFF" { return Err(anyhow!("Not a RIFF file: {:?}", full_path)); }
    let _file_size = reader.read_u32::<LittleEndian>()?;
    let mut wave_header = [0; 4];
    reader.read_exact(&mut wave_header)?;
    if &wave_header != b"WAVE" { return Err(anyhow!("Not a WAVE file: {:?}", full_path)); }

    // --- Loop through all chunks ---
    let mut format_chunk: Option<WavFormat> = None;
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
                format_chunk = Some(WavFormat {
                    audio_format: cursor.read_u16::<LittleEndian>()?,
                    channel_count: cursor.read_u16::<LittleEndian>()?,
                    sampling_rate: cursor.read_u32::<LittleEndian>()?,
                    bits_per_sample: {
                        cursor.seek(SeekFrom::Start(14))?;
                        cursor.read_u16::<LittleEndian>()?
                    },
                });
            }
            b"data" => {
                // We found the data chunk. Record its position and size.
                // We will skip reading the data for now.
                data_chunk_info = Some((chunk_data_start_pos, chunk_size));
            }
            _ => {
                // Unknown or metadata chunk (like `smpl`), read and store it
                let mut chunk_data = vec![0; chunk_size as usize];
                reader.read_exact(&mut chunk_data)?;
                other_chunks.push(OtherChunk { id: chunk_id, data: chunk_data });
            }
        }
        if reader.seek(SeekFrom::Start(next_chunk_aligned_pos)).is_err() {
            break; // Reached end of file
        }
    }

    // Validate and check if processing is needed
    let format = format_chunk.ok_or_else(|| anyhow!("File has no 'fmt ' chunk: {:?}", full_path))?;
    let (data_offset, data_size) = data_chunk_info.ok_or_else(|| anyhow!("File has no 'data' chunk: {:?}", full_path))?;

    let target_bits = if convert_to_16_bit { 16 } else { format.bits_per_sample };
    let target_sample_rate = TARGET_SAMPLE_RATE;
    let target_is_float = format.audio_format == 3 && !convert_to_16_bit;

    let needs_resample = format.sampling_rate != target_sample_rate || pitch_tuning_cents != 0.0;
    let needs_bit_change = target_bits != format.bits_per_sample || (format.audio_format == 3 && !target_is_float);
    
    // Early exit
    if !needs_resample && !needs_bit_change {
        return Ok(relative_path.to_path_buf());
    }
    
    // Generate new file name
    let original_stem = relative_path.file_stem().unwrap_or_default().to_string_lossy();
    let original_ext = relative_path.extension().unwrap_or_default().to_string_lossy();

    let mut suffixes = Vec::new();
    if target_bits != format.bits_per_sample {
        suffixes.push(format!("{}b", target_bits));
    }
    if pitch_tuning_cents != 0.0 {
        suffixes.push(format!("p{:+.1}", pitch_tuning_cents));
    }
    if format.sampling_rate != target_sample_rate {
        suffixes.push(format!("{}k", target_sample_rate / 1000));
    }

    let new_file_name = format!("{}.{}.{}", original_stem, suffixes.join("."), original_ext);
    let new_relative_path = relative_path.with_file_name(new_file_name);
    let new_full_path = base_dir.join(&new_relative_path);

    // Skip if processed version already exists
    if new_full_path.exists() {
        return Ok(new_relative_path);
    }
    
    println!(
        "[WavConvert] Processing: {:?} (Target: {}kHz, {}bit, Pitch: {}c)",
        relative_path, target_sample_rate / 1000, target_bits, pitch_tuning_cents
    );

    // Read all audio data
    let mut reader = reader.into_inner(); // Get back the File
    reader.seek(SeekFrom::Start(data_offset))?;
    let data_reader = BufReader::new(reader);
    let input_waves = read_f32_waves(data_reader, format, data_size)?;

    // Resample if needed
    let output_waves = if needs_resample {
        let pitch_factor = 2.0f64.powf(pitch_tuning_cents as f64 / 1200.0);
        let effective_input_rate = format.sampling_rate as f64 / pitch_factor;
        let resample_ratio = target_sample_rate as f64 / effective_input_rate;

        // Use high-quality Sinc resampler for offline processing
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
        input_waves // Pass through
    };

    // Convert to target format bytes
    let final_data_chunk = write_f32_waves_to_bytes(&output_waves, target_bits, target_is_float)?;

    // Write New File
    let out_file = File::create(&new_full_path)?;
    let mut writer = BufWriter::new(out_file);

    let new_data_size = final_data_chunk.len() as u32;
    let new_bits_per_sample: u16 = target_bits;
    let new_audio_format = if target_is_float { 3 } else { 1 };
    let new_block_align = format.channel_count * (new_bits_per_sample / 8);
    let new_byte_rate = target_sample_rate * new_block_align as u32; // Use target rate
    
    let mut other_chunks_total_size: u32 = 0;
    for chunk in &other_chunks {
        other_chunks_total_size += 8; // (id + size)
        let data_len = chunk.data.len() as u32;
        other_chunks_total_size += data_len + (data_len % 2); // data + padding
    }

    let new_riff_file_size =
        4 + (8 + 16) + other_chunks_total_size + (8 + new_data_size);

    writer.write_all(b"RIFF")?;
    writer.write_u32::<LittleEndian>(new_riff_file_size)?;
    writer.write_all(b"WAVE")?;

    writer.write_all(b"fmt ")?;
    writer.write_u32::<LittleEndian>(16)?; // chunk size (minimal PCM)
    writer.write_u16::<LittleEndian>(new_audio_format)?;
    writer.write_u16::<LittleEndian>(format.channel_count)?;
    writer.write_u32::<LittleEndian>(target_sample_rate)?; // <-- Write new rate
    writer.write_u32::<LittleEndian>(new_byte_rate)?;
    writer.write_u16::<LittleEndian>(new_block_align)?;
    writer.write_u16::<LittleEndian>(new_bits_per_sample)?;
    
    for chunk in &other_chunks {
        writer.write_all(&chunk.id)?;
        writer.write_u32::<LittleEndian>(chunk.data.len() as u32)?;
        writer.write_all(&chunk.data)?;
        if chunk.data.len() % 2 != 0 {
            writer.write_u8(0)?; // padding byte
        }
    }

    writer.write_all(b"data")?;
    writer.write_u32::<LittleEndian>(new_data_size)?;
    writer.write_all(&final_data_chunk)?;

    writer.flush()?;

    Ok(new_relative_path)
}