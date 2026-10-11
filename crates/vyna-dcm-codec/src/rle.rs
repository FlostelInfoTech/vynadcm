//! RLE Lossless codec (NEMA PS3.5 §G.3), implemented from scratch.
//!
//! Each frame starts with a 64-byte header: a 32-bit little-endian segment
//! count `N` (1–15) followed by 15 segment offsets. Segment `s` holds byte
//! `s` of every sample (byte-plane order) PackBits-compressed. Decoding
//! decompresses each segment and interleaves the planes back into sample
//! order, which is already the native (little-endian) byte order.

use crate::{Error, Result};

/// Size of the RLE frame header in bytes.
pub const HEADER_LEN: usize = 64;
/// Maximum number of RLE segments.
pub const MAX_SEGMENTS: usize = 15;

/// Decode one RLE frame.
///
/// `expected_len` is the known decompressed size
/// (`rows × columns × samples × bytes_per_sample`); the decoder rejects
/// frames that do not match it exactly.
pub fn decode_frame(compressed: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    if compressed.len() < HEADER_LEN {
        return Err(Error::Rle(format!(
            "frame too short: {} < {HEADER_LEN} header bytes",
            compressed.len()
        )));
    }
    let count = u32::from_le_bytes(compressed[0..4].try_into().unwrap()) as usize;
    if count == 0 || count > MAX_SEGMENTS {
        return Err(Error::Rle(format!("bad segment count {count}")));
    }
    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..count {
        let o = 4 + i * 4;
        offsets.push(u32::from_le_bytes(compressed[o..o + 4].try_into().unwrap()) as usize);
    }
    offsets.push(compressed.len());
    if offsets[0] != HEADER_LEN {
        return Err(Error::Rle(format!(
            "first segment offset {} != {HEADER_LEN}",
            offsets[0]
        )));
    }
    for w in offsets.windows(2) {
        if w[1] < w[0] || w[1] > compressed.len() {
            return Err(Error::Rle("segment offsets out of order or range".into()));
        }
    }

    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(count);
    for k in 0..count {
        planes.push(packbits_decode(&compressed[offsets[k]..offsets[k + 1]])?);
    }
    let plane_len = planes[0].len();
    if planes.iter().any(|p| p.len() != plane_len) {
        return Err(Error::Rle("segments decoded to unequal lengths".into()));
    }
    if plane_len * count != expected_len {
        return Err(Error::Rle(format!(
            "decoded {} bytes, expected {expected_len}",
            plane_len * count
        )));
    }

    let mut out = vec![0u8; expected_len];
    for (i, plane) in planes.iter().enumerate() {
        for (j, b) in plane.iter().enumerate() {
            out[j * count + i] = *b;
        }
    }
    Ok(out)
}

/// Encode one frame of raw native-order samples.
///
/// `bytes_per_sample` (1–15) selects the plane count: 1 for 8-bit pixels,
/// 2 for 16-bit pixels, 3 for 8-bit RGB, etc.
pub fn encode_frame(raw: &[u8], bytes_per_sample: usize) -> Result<Vec<u8>> {
    if bytes_per_sample == 0 || bytes_per_sample > MAX_SEGMENTS {
        return Err(Error::Rle(format!(
            "bad bytes-per-sample {bytes_per_sample}"
        )));
    }
    if !raw.len().is_multiple_of(bytes_per_sample) {
        return Err(Error::Rle(
            "raw length not a multiple of bytes-per-sample".into(),
        ));
    }
    let samples = raw.len() / bytes_per_sample;
    let mut encoded: Vec<Vec<u8>> = Vec::with_capacity(bytes_per_sample);
    for s in 0..bytes_per_sample {
        let mut plane = Vec::with_capacity(samples);
        for i in 0..samples {
            plane.push(raw[i * bytes_per_sample + s]);
        }
        encoded.push(packbits_encode(&plane));
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(bytes_per_sample as u32).to_le_bytes());
    let mut offset = HEADER_LEN as u32;
    for k in 0..MAX_SEGMENTS {
        if k < encoded.len() {
            out.extend_from_slice(&offset.to_le_bytes());
            offset += encoded[k].len() as u32;
        } else {
            out.extend_from_slice(&0u32.to_le_bytes());
        }
    }
    debug_assert_eq!(out.len(), HEADER_LEN);
    for seg in &encoded {
        out.extend_from_slice(seg);
    }
    Ok(out)
}

/// PackBits decompression (TIFF/DICOM variant).
fn packbits_decode(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let n = input[i] as i8;
        i += 1;
        if n >= 0 {
            let count = n as usize + 1;
            if i + count > input.len() {
                return Err(Error::Rle("truncated literal run".into()));
            }
            out.extend_from_slice(&input[i..i + count]);
            i += count;
        } else if n == i8::MIN {
            // No-op.
        } else {
            let count = (-(n as i32) + 1) as usize;
            if i >= input.len() {
                return Err(Error::Rle("truncated replicate run".into()));
            }
            out.extend(std::iter::repeat_n(input[i], count));
            i += 1;
        }
    }
    Ok(out)
}

/// PackBits compression: greedy replicate runs of ≥3, else literals.
fn packbits_encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        // Measure identical-byte run ahead (max 128).
        let mut run = 1;
        while run < 128 && i + run < input.len() && input[i + run] == input[i] {
            run += 1;
        }
        if run >= 3 {
            out.push((1 - run as i16) as u8); // count byte -(run-1)
            out.push(input[i]);
            i += run;
            continue;
        }
        // Literal run: stop before a 3+ replicate run or at 128 bytes.
        let start = i;
        while i < input.len() && i - start < 128 {
            let mut ahead = 1;
            while ahead < 3 && i + ahead < input.len() && input[i + ahead] == input[i] {
                ahead += 1;
            }
            if ahead >= 3 {
                break;
            }
            i += 1;
        }
        out.push((i - start - 1) as u8);
        out.extend_from_slice(&input[start..i]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_known_vectors() {
        // Literal: 0x02 -> copy 3 bytes.
        assert_eq!(
            packbits_decode(&[0x02, 0xAA, 0xBB, 0xCC]).unwrap(),
            vec![0xAA, 0xBB, 0xCC]
        );
        // Replicate: 0xFE (-2) -> repeat 3x.
        assert_eq!(
            packbits_decode(&[0xFE, 0x7F]).unwrap(),
            vec![0x7F, 0x7F, 0x7F]
        );
        // No-op 0x80 skipped.
        assert_eq!(packbits_decode(&[0x80, 0x00, 0x11]).unwrap(), vec![0x11]);
        // Truncated runs rejected.
        assert!(packbits_decode(&[0x05, 0x01]).is_err());
        assert!(packbits_decode(&[0xFF]).is_err());
    }

    #[test]
    fn packbits_round_trip() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 % 251) as u8).collect();
        let enc = packbits_encode(&data);
        assert_eq!(packbits_decode(&enc).unwrap(), data);
        // Highly redundant input compresses well.
        let flat = vec![0xABu8; 1000];
        assert!(packbits_encode(&flat).len() < 100);
        assert_eq!(packbits_decode(&packbits_encode(&flat)).unwrap(), flat);
    }

    #[test]
    fn frame_round_trip_16bit() {
        // 4 samples of 16-bit LE: 0x1234, 0x0000, 0xFFFF, 0x00FF.
        let raw = vec![0x34, 0x12, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x00];
        let enc = encode_frame(&raw, 2).unwrap();
        assert_eq!(u32::from_le_bytes(enc[0..4].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(enc[4..8].try_into().unwrap()), 64);
        assert_eq!(decode_frame(&enc, raw.len()).unwrap(), raw);
    }

    #[test]
    fn frame_round_trip_rgb_planar() {
        let raw: Vec<u8> = (0..96u8).collect();
        let enc = encode_frame(&raw, 3).unwrap();
        assert_eq!(decode_frame(&enc, raw.len()).unwrap(), raw);
    }

    #[test]
    fn frame_rejects_bad_headers() {
        assert!(decode_frame(&[0u8; 10], 10).is_err());
        let mut bad = vec![0u8; 64];
        bad[0] = 16; // count > 15
        assert!(decode_frame(&bad, 0).is_err());
        let mut bad2 = vec![0u8; 64];
        bad2[0] = 1;
        bad2[4] = 1; // first offset != 64
        assert!(decode_frame(&bad2, 0).is_err());
    }
}
