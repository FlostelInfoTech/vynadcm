//! RLE Lossless codec (NEMA PS3.5 §G.3), implemented from scratch.
//!
//! Each frame starts with a 64-byte header: a 32-bit little-endian segment
//! count `N` (1–15) followed by 15 segment offsets. Segment `s` holds byte
//! `s` of every sample (byte-plane order) PackBits-compressed, padded to
//! even length with one trailing zero byte. Decoding runs each segment to
//! the known plane length and tolerates that one pad byte past the end.

use crate::{Error, Result};

/// Size of the RLE frame header in bytes.
pub const HEADER_LEN: usize = 64;
/// Maximum number of RLE segments.
pub const MAX_SEGMENTS: usize = 15;

/// Decode one RLE frame.
///
/// Segments run component-major, MSB-first within each component
/// (PS3.5 §G.3): 16-bit mono is `[high, low]`, 16-bit RGB is
/// `[R-high, R-low, G-high, G-low, B-high, B-low]`. `expected_len` is
/// the known decompressed size; the decoder rejects frames that do
/// not match it exactly.
pub fn decode_frame(
    compressed: &[u8],
    samples: usize,
    bytes_per_sample: usize,
    expected_len: usize,
) -> Result<Vec<u8>> {
    if compressed.len() < HEADER_LEN {
        return Err(Error::Rle(format!(
            "frame too short: {} < {HEADER_LEN} header bytes",
            compressed.len()
        )));
    }
    let count = u32::from_le_bytes(compressed[0..4].try_into().unwrap()) as usize;
    if samples == 0 || bytes_per_sample == 0 {
        return Err(Error::Rle(
            "samples and bytes-per-sample must be nonzero".into(),
        ));
    }
    if count == 0 || count > MAX_SEGMENTS || count != samples * bytes_per_sample {
        return Err(Error::Rle(format!(
            "bad segment count {count} for {samples}×{bytes_per_sample} pixels"
        )));
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

    if !expected_len.is_multiple_of(count) {
        return Err(Error::Rle(format!(
            "expected length {expected_len} not a multiple of {count} segments"
        )));
    }
    let plane_len = expected_len / count;
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(count);
    for k in 0..count {
        planes.push(packbits_decode(
            &compressed[offsets[k]..offsets[k + 1]],
            plane_len,
        )?);
    }

    let mut out = vec![0u8; expected_len];
    for (i, plane) in planes.iter().enumerate() {
        // Wire plane i is byte (MSB-first) i%bps of sample i/bps;
        // output is little-endian within each sample.
        let at = (i / bytes_per_sample) * bytes_per_sample
            + (bytes_per_sample - 1 - i % bytes_per_sample);
        for (j, b) in plane.iter().enumerate() {
            out[j * count + at] = *b;
        }
    }
    Ok(out)
}

/// Whether a basic offset table is usable for RLE framing: one entry
/// per frame, starting at 0, strictly increasing, in range, and each
/// pointing at a plausible RLE frame header. Some writers (old DCMTK)
/// include fragment item headers in the offsets; those tables fail the
/// header check and callers fall back to fragment-per-frame.
pub(crate) fn bot_usable(joined: &[u8], bot: &[u32], frames: usize) -> bool {
    if bot.len() != frames || frames == 0 || bot[0] != 0 {
        return false;
    }
    for w in bot.windows(2) {
        if w[1] <= w[0] {
            return false;
        }
    }
    for o in bot {
        let o = *o as usize;
        if o + HEADER_LEN > joined.len() {
            return false;
        }
        let count = u32::from_le_bytes(joined[o..o + 4].try_into().unwrap()) as usize;
        if count == 0 || count > MAX_SEGMENTS {
            return false;
        }
        let first = u32::from_le_bytes(joined[o + 4..o + 8].try_into().unwrap()) as usize;
        if first != HEADER_LEN {
            return false;
        }
    }
    true
}

/// Encode one frame of raw native-order samples.
///
/// Segments run component-major, MSB-first within each component
/// (PS3.5 §G.3), each padded to even length.
pub fn encode_frame(raw: &[u8], samples: usize, bytes_per_sample: usize) -> Result<Vec<u8>> {
    if samples == 0 || bytes_per_sample == 0 {
        return Err(Error::Rle(
            "samples and bytes-per-sample must be nonzero".into(),
        ));
    }
    let count = samples * bytes_per_sample;
    if count > MAX_SEGMENTS {
        return Err(Error::Rle(format!("bad segment count {count}")));
    }
    if !raw.len().is_multiple_of(count) {
        return Err(Error::Rle(
            "raw length not a multiple of the pixel size".into(),
        ));
    }
    let pixels = raw.len() / count;
    let mut encoded: Vec<Vec<u8>> = Vec::with_capacity(count);
    for k in 0..count {
        // Wire segment k is MSB-first byte k%bps of sample k/bps.
        let at = (k / bytes_per_sample) * bytes_per_sample
            + (bytes_per_sample - 1 - k % bytes_per_sample);
        let mut plane = Vec::with_capacity(pixels);
        for i in 0..pixels {
            plane.push(raw[i * count + at]);
        }
        let mut seg = packbits_encode(&plane);
        // Segments are even-length on the wire.
        if seg.len() % 2 == 1 {
            seg.push(0);
        }
        encoded.push(seg);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(count as u32).to_le_bytes());
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

/// PackBits decompression (TIFF/DICOM variant) to exactly `want` bytes.
///
/// Runs stop at the known plane length: short streams are truncated,
/// overlong runs are corrupt, and at most one trailing byte (the
/// even-length segment pad writers emit) is tolerated past the end.
fn packbits_decode(input: &[u8], want: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(want);
    let mut i = 0;
    while out.len() < want {
        if i >= input.len() {
            return Err(Error::Rle("truncated segment".into()));
        }
        let n = input[i] as i8;
        i += 1;
        if n >= 0 {
            let count = n as usize + 1;
            if i + count > input.len() {
                return Err(Error::Rle("truncated literal run".into()));
            }
            if out.len() + count > want {
                return Err(Error::Rle("literal run overflows the plane".into()));
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
            if out.len() + count > want {
                return Err(Error::Rle("replicate run overflows the plane".into()));
            }
            out.extend(std::iter::repeat_n(input[i], count));
            i += 1;
        }
    }
    if input.len() > i + 1 {
        return Err(Error::Rle("trailing bytes past the plane end".into()));
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
            packbits_decode(&[0x02, 0xAA, 0xBB, 0xCC], 3).unwrap(),
            vec![0xAA, 0xBB, 0xCC]
        );
        // Replicate: 0xFE (-2) -> repeat 3x.
        assert_eq!(
            packbits_decode(&[0xFE, 0x7F], 3).unwrap(),
            vec![0x7F, 0x7F, 0x7F]
        );
        // No-op 0x80 skipped.
        assert_eq!(packbits_decode(&[0x80, 0x00, 0x11], 1).unwrap(), vec![0x11]);
        // Truncated runs rejected.
        assert!(packbits_decode(&[0x05, 0x01], 6).is_err());
        assert!(packbits_decode(&[0xFF], 2).is_err());
        // One trailing pad byte tolerated, two rejected.
        assert_eq!(packbits_decode(&[0x00, 0x11, 0x00], 1).unwrap(), vec![0x11]);
        assert!(packbits_decode(&[0x00, 0x11, 0x00, 0x00], 1).is_err());
        // Runs may not overflow the plane.
        assert!(packbits_decode(&[0x02, 0xAA, 0xBB, 0xCC], 2).is_err());
        assert!(packbits_decode(&[0xFE, 0x7F], 2).is_err());
    }

    #[test]
    fn packbits_round_trip() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 % 251) as u8).collect();
        let enc = packbits_encode(&data);
        assert_eq!(packbits_decode(&enc, data.len()).unwrap(), data);
        // Highly redundant input compresses well.
        let flat = vec![0xABu8; 1000];
        assert!(packbits_encode(&flat).len() < 100);
        assert_eq!(
            packbits_decode(&packbits_encode(&flat), flat.len()).unwrap(),
            flat
        );
    }

    #[test]
    fn frame_round_trip_16bit() {
        // 4 samples of 16-bit LE: 0x1234, 0x0000, 0xFFFF, 0x00FF.
        let raw = vec![0x34, 0x12, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x00];
        let enc = encode_frame(&raw, 1, 2).unwrap();
        assert_eq!(u32::from_le_bytes(enc[0..4].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(enc[4..8].try_into().unwrap()), 64);
        assert_eq!(decode_frame(&enc, 1, 2, raw.len()).unwrap(), raw);
    }

    #[test]
    fn frame_round_trip_rgb_planar() {
        let raw: Vec<u8> = (0..96u8).collect();
        let enc = encode_frame(&raw, 3, 1).unwrap();
        assert_eq!(decode_frame(&enc, 3, 1, raw.len()).unwrap(), raw);
    }

    #[test]
    fn frame_rejects_bad_headers() {
        assert!(decode_frame(&[0u8; 10], 1, 2, 10).is_err());
        let mut bad = vec![0u8; 64];
        bad[0] = 16; // count > 15
        assert!(decode_frame(&bad, 1, 2, 0).is_err());
        let mut bad2 = vec![0u8; 64];
        bad2[0] = 1;
        bad2[4] = 1; // first offset != 64
        assert!(decode_frame(&bad2, 1, 2, 0).is_err());
    }

    #[test]
    fn bot_usable_rejects_header_counted_tables() {
        let f0 = encode_frame(&[1u8; 16], 1, 2).unwrap();
        let f1 = encode_frame(&[2u8; 16], 1, 2).unwrap();
        let mut joined = f0.clone();
        joined.extend_from_slice(&f1);
        // Correct table: fragment starts.
        assert!(bot_usable(&joined, &[0, f0.len() as u32], 2));
        // Old-DCMTK style: item headers counted (+8 per frame).
        assert!(!bot_usable(&joined, &[0, f0.len() as u32 + 8], 2));
        // Structural faults.
        assert!(!bot_usable(&joined, &[8, f0.len() as u32], 2));
        assert!(!bot_usable(&joined, &[0], 2));
        assert!(!bot_usable(&joined, &[0, 0], 2));
    }
}
