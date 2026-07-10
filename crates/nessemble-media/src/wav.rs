//! `.incwav` WAV → DPCM conversion, ported from the reference `wav.c` +
//! `pseudo/incwav.c`. A mono PCM WAV is delta-modulated into the NES DPCM bit
//! stream: each output byte packs eight 1-bit up/down decisions.

/// Why a WAV could not be converted (each maps to a reference error message).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavError {
    /// Header shorter than 12 bytes → "Could not read".
    ShortRead,
    /// Missing `RIFF`/`WAVE` magic → "is not a WAV".
    NotWav,
    /// More than one channel → "WAV is not mono".
    NotMono,
}

struct Fmt {
    channels: u16,
    bits_sample: u16,
}

/// Convert mono PCM WAV `bytes` into DPCM using `amplitude` (clamped to
/// `2..=40`). Returns the DPCM byte stream, or a [`WavError`].
pub fn wav_to_dpcm(bytes: &[u8], amplitude: i32) -> Result<Vec<u8>, WavError> {
    if bytes.len() < 12 {
        return Err(WavError::ShortRead);
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }

    // Walk the chunk list for `fmt ` and `data`.
    let mut pos = 12;
    let mut fmt: Option<Fmt> = None;
    let mut data: Option<(usize, usize)> = None; // (start, len)
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        pos += 8;
        if id == b"fmt " {
            if size < 16 || pos + 16 > bytes.len() {
                return Ok(Vec::new());
            }
            fmt = Some(Fmt {
                channels: u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]),
                bits_sample: u16::from_le_bytes([bytes[pos + 14], bytes[pos + 15]]),
            });
            pos += size;
        } else if id == b"data" {
            if fmt.is_none() || size == 0 {
                return Ok(Vec::new());
            }
            let end = (pos + size).min(bytes.len());
            data = Some((pos, end - pos));
            break;
        } else {
            pos += size;
        }
    }

    let (fmt, (data_start, data_len)) = match (fmt, data) {
        (Some(f), Some(d)) => (f, d),
        _ => return Ok(Vec::new()),
    };

    if fmt.channels != 1 {
        return Err(WavError::NotMono);
    }

    let amplitude = amplitude.clamp(2, 40);
    Ok(encode_dpcm(
        &bytes[data_start..data_start + data_len],
        fmt.bits_sample,
        amplitude,
    ))
}

/// The core delta-modulation loop (reference `pseudo_incwav`). `y` tracks the
/// reconstructed level; each bit records whether the target sample is at or
/// above it.
fn encode_dpcm(data: &[u8], bits_sample: u16, amplitude: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut reader = SampleReader::new(data, bits_sample);
    let mut y: i32 = 0;
    let mut x: i32 = 0;
    let mut subsample: i32 = 99;
    const OVERSAMPLE: i32 = 100;

    while reader.chunk_left > 0 {
        let mut code: u32 = 0;
        for i in 0..8 {
            while subsample < 100 {
                let sample = reader.next_sample();
                x = (sample as i32 * amplitude + 16384) >> 15;
                subsample += OVERSAMPLE;
            }
            subsample -= 100;

            if x >= y {
                y += 1;
                if y > 31 {
                    y = 31;
                }
                code |= 1 << i;
            } else {
                y -= 1;
                if y < -32 {
                    y = -32;
                }
            }
        }
        out.push(code as u8);
    }
    out
}

/// Sequential PCM sample reader mirroring the reference `wav_sample`.
struct SampleReader<'a> {
    data: &'a [u8],
    pos: usize,
    bits_sample: u16,
    chunk_left: i64,
}

impl<'a> SampleReader<'a> {
    fn new(data: &'a [u8], bits_sample: u16) -> Self {
        SampleReader {
            data,
            pos: 0,
            bits_sample,
            chunk_left: data.len() as i64,
        }
    }

    /// Read one sample (little-endian, `bits_sample` wide), returning the signed
    /// 16-bit value; 8-bit samples are recentred from unsigned to signed.
    fn next_sample(&mut self) -> i16 {
        if self.chunk_left == 0 {
            return 0;
        }
        let mut cur: u32 = 0;
        let mut i = 0;
        while i < self.bits_sample && self.chunk_left > 0 {
            let c = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            cur >>= 8;
            cur |= (c as u32 & 0xFF) << 8;
            self.chunk_left -= 1;
            i += 8;
        }
        let mut sample = cur as i32;
        if self.bits_sample <= 8 {
            sample -= 32768;
        }
        sample as i16
    }
}
