use anyhow::{anyhow, Result, Error};
use std::io::{Seek, SeekFrom, Read, Cursor};
use std::path::Path;
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
/// A struct to hold metadata chunks (like 'smpl') that we want to preserve.
#[derive(Debug, Clone)]
pub struct OtherChunk {
    pub id: [u8; 4],
    pub data: Vec<u8>,
}

// Custom error type to signal WavPack detection
#[derive(Debug)]
pub struct IsWavPackError;
impl std::fmt::Display for IsWavPackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "File appears to be WavPack")
    }
}
impl std::error::Error for IsWavPackError {}

/// Parses all necessary metadata from a WAV file in one pass.
pub fn parse_wav_metadata<R: Read + Seek>(
    reader: &mut R,
    full_path_for_logs: &Path,
) -> Result<(WavFmt, Vec<OtherChunk>, u64, u32)> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    
    // --- Check for WavPack Signature ---
    if &header == b"wvpk" {
        return Err(Error::new(IsWavPackError));
    }

    // --- Standard RIFF Check ---
    if &header != b"RIFF" { 
        return Err(anyhow!("Not a RIFF file (found {:?}): {:?}", header, full_path_for_logs)); 
    }

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
            chunk_data_start_pos + (chunk_size as u64 + ((chunk_size as u64) % 2));

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

    #[allow(dead_code)]
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
                None
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

/// Parses a 'cue ' chunk's data and returns a list of sample offsets.
pub fn parse_cue_chunk(data: &[u8]) -> Vec<u32> {
    let mut positions = Vec::new();
    
    if data.len() < 4 { return positions; }
    let mut cursor = Cursor::new(data);

    // Read num_points
    let num_points = match cursor.read_u32::<LittleEndian>() {
        Ok(n) => n,
        Err(_) => return positions,
    };

    // Each CuePoint is 24 bytes.
    // Offset 4 (dwPosition) and 20 (dwSampleOffset) are relevant. 
    // Usually dwSampleOffset is the most reliable for PCM.
    
    for _ in 0..num_points {
        // Safe check for remaining data
        let current_pos = cursor.position();
        if (data.len() as u64 - current_pos) < 24 { break; }

        // Skip Name (4)
        if cursor.seek(SeekFrom::Current(4)).is_err() { break; }
        
        // Read dwPosition (4) - roughly the sample index
        let _dw_position = match cursor.read_u32::<LittleEndian>() {
            Ok(n) => n,
            Err(_) => break,
        };
        
        // Skip fccChunk(4), dwChunkStart(4), dwBlockStart(4) -> 12 bytes
        if cursor.seek(SeekFrom::Current(12)).is_err() { break; }
        
        // Read dwSampleOffset (4)
        let sample_offset = match cursor.read_u32::<LittleEndian>() {
            Ok(n) => n,
            Err(_) => break,
        };

        positions.push(sample_offset);
    }
    
    positions.sort();
    positions
}