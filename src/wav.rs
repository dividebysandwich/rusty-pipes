use anyhow::{anyhow, Result};
use std::io::{Seek, SeekFrom, Read, Cursor};
use byteorder::{ReadBytesExt as OtherReadBytesExt, LittleEndian};

const I16_MAX_F: f32 = 32768.0;  // 2^15
const I24_MAX_F: f32 = 8388608.0; // 2^23
const I32_MAX_F: f32 = 2147483648.0; // 2^31

/// Holds format information from the 'fmt ' chunk.
#[derive(Debug, Clone, Copy)]
pub struct WavFmt {
    pub audio_format: u16,   // 1 = PCM, 3 = IEEE Float
    pub num_channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
}

impl WavFmt {
    /// Parses the 'fmt ' chunk data.
    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 16 {
            return Err(anyhow!("'fmt ' chunk is too small: {} bytes", data.len()));
        }
        let mut cursor = Cursor::new(data);
        let audio_format = cursor.read_u16::<LittleEndian>()?;
        let num_channels = cursor.read_u16::<LittleEndian>()?;
        let sample_rate = cursor.read_u32::<LittleEndian>()?;
        let _byte_rate = cursor.read_u32::<LittleEndian>()?; // Not needed
        let _block_align = cursor.read_u16::<LittleEndian>()?; // Not needed
        let bits_per_sample = cursor.read_u16::<LittleEndian>()?;

        if audio_format != 1 && audio_format != 3 {
            return Err(anyhow!("Unsupported audio format: {} (only 1=PCM or 3=Float)", audio_format));
        }
        
        log::trace!("[WavFmt] Parsed: {}ch, {}Hz, {}b, format={}", num_channels, sample_rate, bits_per_sample, audio_format);

        Ok(Self {
            audio_format,
            num_channels,
            sample_rate,
            bits_per_sample,
        })
    }
}

/// Parses all necessary metadata from a WAV file in one pass.
/// Returns (format, loop_info, data_chunk_start_pos, data_chunk_size)
pub fn parse_wav_metadata<R: Read + Seek>(
    reader: &mut R
) -> Result<(WavFmt, Option<(u32, u32)>, u64, u32)> {
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

    let mut fmt: Option<WavFmt> = None;
    let mut loop_info: Option<(u32, u32)> = None;
    let mut data_info: Option<(u64, u32)> = None; // (start_pos, size_in_bytes)

    'chunk_loop: while let Ok(chunk_id) = reader.read_u32::<LittleEndian>().map(|id| id.to_le_bytes()) {
        let chunk_size = reader.read_u32::<LittleEndian>()?;
        let chunk_data_start_pos = reader.stream_position()?;
        let next_chunk_aligned_pos =
            chunk_data_start_pos + (chunk_size as u64 + (chunk_size % 2) as u64);

        match &chunk_id {
            b"fmt " => {
                let mut chunk_data = vec![0; chunk_size as usize];
                reader.read_exact(&mut chunk_data)?;
                fmt = Some(WavFmt::parse(&chunk_data)?);
            }
            b"smpl" => {
                let mut chunk_data = vec![0; chunk_size as usize];
                reader.read_exact(&mut chunk_data)?;
                loop_info = parse_smpl_chunk(&chunk_data);
            }
            b"data" => {
                // Found data, store its position and size.
                data_info = Some((chunk_data_start_pos, chunk_size));
                
                // We MUST break here. Any 'smpl' chunk *must* come before 'data'.
                // If it comes after, it's a non-standard file and we won't find it.
                // This matches the behavior of the original parser.
                break 'chunk_loop;
            }
            _ => {
                // Other chunk, skip it
            }
        }
        
        // Seek to the *start* of the next chunk
        if reader.seek(SeekFrom::Start(next_chunk_aligned_pos)).is_err() {
            // End of file, break loop
            break 'chunk_loop;
        }
    }

    let fmt = fmt.ok_or_else(|| anyhow!("'fmt ' chunk not found"))?;
    let (data_start, data_size) = data_info.ok_or_else(|| anyhow!("'data' chunk not found"))?;

    Ok((fmt, loop_info, data_start, data_size))
}


/// An iterator that reads samples from a WAV file's data chunk
/// and converts them to f32.
pub struct WavSampleReader<R: Read + Seek> {
    reader: R,
    fmt: WavFmt,
    data_chunk_size: u32,
    bytes_read: u32,
}

impl<R: Read + Seek> WavSampleReader<R> {
    /// Creates a new reader. Assumes the reader is positioned *after*
    /// metadata and seeks to the start of the data chunk.
    pub fn new(mut reader: R, fmt: WavFmt, data_start: u64, data_size: u32) -> Result<Self> {
        reader.seek(SeekFrom::Start(data_start))?;
        Ok(Self {
            reader,
            fmt,
            data_chunk_size: data_size,
            bytes_read: 0,
        })
    }

    #[allow(dead_code)]
    pub fn sample_rate(&self) -> u32 {
        self.fmt.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.fmt.num_channels
    }

    /// Reads a single 24-bit signed sample.
    fn read_i24(&mut self) -> std::io::Result<i32> {
        let b1 = self.reader.read_u8()? as i32;
        let b2 = self.reader.read_u8()? as i32;
        let b3 = self.reader.read_u8()? as i32;
        // Combine, then sign-extend from 24th bit
        let sample = (b1 | (b2 << 8) | (b3 << 16)) << 8 >> 8;
        Ok(sample)
    }
}

impl<R: Read + Seek> Iterator for WavSampleReader<R> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        // Check if we're at the end of the data chunk
        if self.bytes_read >= self.data_chunk_size {
            return None;
        }

        match self.fmt.bits_per_sample {
            16 => {
                let sample = self.reader.read_i16::<LittleEndian>().ok()?;
                self.bytes_read += 2;
                Some((sample as f32) / I16_MAX_F)
            }
            24 => {
                // Use our custom 24-bit reader
                let sample = self.read_i24().ok()?;
                self.bytes_read += 3;
                Some((sample as f32) / I24_MAX_F)
            }
            32 => {
                if self.fmt.audio_format == 1 { // 32-bit PCM
                    let sample = self.reader.read_i32::<LittleEndian>().ok()?;
                    self.bytes_read += 4;
                    Some((sample as f32) / I32_MAX_F)
                } else { // 32-bit Float
                    let sample = self.reader.read_f32::<LittleEndian>().ok()?;
                    self.bytes_read += 4;
                    Some(sample)
                }
            }
            _ => {
                log::warn!("Unsupported bits_per_sample: {}", self.fmt.bits_per_sample);
                None // Unsupported format
            }
        }
    }
}

/// Parses a 'smpl' chunk's data. Returns (loop_start, loop_end) in samples.
pub fn parse_smpl_chunk(data: &[u8]) -> Option<(u32, u32)> {
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
