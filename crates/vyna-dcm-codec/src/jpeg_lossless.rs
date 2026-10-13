//! JPEG Lossless pixel codec (Transfer Syntax `1.2.840.10008.1.2.4.70`).
//!
//! Backed by the pure-Rust `jpegli` crate (ITU-T T.81 Annex H, DICOM Process
//! 14 Selection Value 1): single-component grayscale, precision 2–16 bits.
//! Samples are coded as opaque bit patterns, so unsigned and two's-complement
//! signed data both round-trip exactly.

use jpegli::EncodeOptions;
use vyna_dcm_core::{FragmentSequence, TransferSyntax};

use crate::{PixelCodec, PixelCodecError, samples_from_bytes, samples_to_bytes};

/// Pixel codec for JPEG Lossless, Non-Hierarchical (`.70`).
#[derive(Clone, Copy, Debug, Default)]
pub struct JpegLosslessCodec;

impl PixelCodec for JpegLosslessCodec {
    fn syntaxes(&self) -> &[TransferSyntax] {
        &[TransferSyntax::JpegLossless]
    }

    fn decode_frame(
        &self,
        ts: TransferSyntax,
        fragments: &FragmentSequence,
        rows: u16,
        columns: u16,
        samples_per_pixel: u16,
        bits_allocated: u16,
    ) -> std::result::Result<Vec<u8>, PixelCodecError> {
        if ts != TransferSyntax::JpegLossless {
            return Err(PixelCodecError::Unsupported(ts));
        }
        check_mono_16bit(samples_per_pixel, bits_allocated)?;
        let data = fragments.joined();
        let (samples, w, h) = jpegli::decode(&data, columns.into(), rows.into())
            .map_err(|e| PixelCodecError::Codec(format!("jpeg-lossless decode failed: {e}")))?;
        if w != columns as u32 || h != rows as u32 {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg-lossless frame is {w}x{h}, dataset says {columns}x{rows}"
            )));
        }
        samples_to_bytes(&samples, bits_allocated)
    }

    fn encode_frame(
        &self,
        ts: TransferSyntax,
        raw: &[u8],
        rows: u16,
        columns: u16,
        samples_per_pixel: u16,
        bits_allocated: u16,
    ) -> std::result::Result<FragmentSequence, PixelCodecError> {
        if ts != TransferSyntax::JpegLossless {
            return Err(PixelCodecError::Unsupported(ts));
        }
        check_mono_16bit(samples_per_pixel, bits_allocated)?;
        check_raw_len(raw, rows, columns, bits_allocated)?;
        let samples = samples_from_bytes(raw, bits_allocated)?;
        let mut opts = EncodeOptions::default();
        opts.precision = bits_allocated.max(8) as u8;
        let mut out = Vec::new();
        jpegli::encode_with_options(&samples, columns.into(), rows.into(), &opts, &mut out)
            .map_err(|e| PixelCodecError::Codec(format!("jpeg-lossless encode failed: {e}")))?;
        Ok(FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![out],
        })
    }
}

/// Shared geometry check for the single-component lossless families.
pub(crate) fn check_mono_16bit(
    samples_per_pixel: u16,
    bits_allocated: u16,
) -> std::result::Result<(), PixelCodecError> {
    if samples_per_pixel != 1 {
        return Err(PixelCodecError::Geometry(format!(
            "this codec handles monochrome frames only, got {samples_per_pixel} samples per pixel"
        )));
    }
    if !(2..=16).contains(&bits_allocated) {
        return Err(PixelCodecError::Geometry(format!(
            "bits allocated {bits_allocated} outside the 2..=16 range"
        )));
    }
    Ok(())
}

/// Raw length must match rows × columns at the given bit depth.
pub(crate) fn check_raw_len(
    raw: &[u8],
    rows: u16,
    columns: u16,
    bits_allocated: u16,
) -> std::result::Result<(), PixelCodecError> {
    // Checked: the u16 product can overflow 32-bit usize (and would
    // panic in debug builds).
    let expected = (rows as usize)
        .checked_mul(columns as usize)
        .and_then(|n| n.checked_mul(bits_allocated.div_ceil(8) as usize))
        .ok_or_else(|| {
            PixelCodecError::Geometry(format!(
                "geometry {rows}x{columns}x{bits_allocated} overflows address space"
            ))
        })?;
    if raw.len() != expected {
        return Err(PixelCodecError::Geometry(format!(
            "raw frame is {} bytes, expected {expected}",
            raw.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern_16(w: u16, h: u16, max: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(w as usize * h as usize * 2);
        for y in 0..h {
            for x in 0..w {
                let v =
                    (x as u32 * 257 + y as u32 * 2003 + ((x ^ y) as u32 * 17)) % (max as u32 + 1);
                out.extend_from_slice(&(v as u16).to_le_bytes());
            }
        }
        out
    }

    fn roundtrip(raw: &[u8], rows: u16, cols: u16, bits: u16) {
        let codec = JpegLosslessCodec;
        let frags = codec
            .encode_frame(TransferSyntax::JpegLossless, raw, rows, cols, 1, bits)
            .unwrap();
        let back = codec
            .decode_frame(TransferSyntax::JpegLossless, &frags, rows, cols, 1, bits)
            .unwrap();
        assert_eq!(back, raw, "lossless roundtrip must be exact");
    }

    #[test]
    fn lossless_roundtrips_exactly() {
        // 8-bit incl. odd sizes.
        let raw: Vec<u8> = (0..33 * 17u32).map(|i| (i * 7 + i / 33) as u8).collect();
        roundtrip(&raw, 17, 33, 8);
        // 12-bit stored in 16, 16-bit full range, signed patterns.
        roundtrip(&pattern_16(32, 16, 4095), 16, 32, 16);
        roundtrip(&pattern_16(16, 16, 65535), 16, 16, 16);
        // Two's-complement signed extremes round-trip as bit patterns.
        let signed: Vec<u8> = [0x00, 0x80, 0xFF, 0x7F, 0x00, 0x00, 0xFF, 0xFF]
            .into_iter()
            .cycle()
            .take(16 * 16 * 2)
            .collect();
        roundtrip(&signed, 16, 16, 16);
    }

    #[test]
    fn truncated_input_errors() {
        let codec = JpegLosslessCodec;
        let raw = pattern_16(16, 16, 4095);
        let frags = codec
            .encode_frame(TransferSyntax::JpegLossless, &raw, 16, 16, 1, 16)
            .unwrap();
        let bytes = frags.joined();
        let cut = FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![bytes[..bytes.len() / 2].to_vec()],
        };
        let err = codec
            .decode_frame(TransferSyntax::JpegLossless, &cut, 16, 16, 1, 16)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Codec(_)), "{err:?}");
    }

    #[test]
    fn rejects_bad_geometry() {
        let codec = JpegLosslessCodec;
        let raw = vec![0u8; 64];
        let err = codec
            .encode_frame(TransferSyntax::JpegLossless, &raw, 8, 8, 3, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
        let err = codec
            .encode_frame(TransferSyntax::JpegLossless, &raw, 8, 8, 1, 32)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
        let err = codec
            .encode_frame(TransferSyntax::JpegBaseline8Bit, &raw, 8, 8, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Unsupported(_)), "{err:?}");
    }
}
