//! JPEG-LS pixel codec (Transfer Syntaxes `.80` lossless, `.81` near-lossless).
//!
//! Backed by the pure-Rust `jpegls` crate (ISO/IEC 14495-1 / ITU-T T.87):
//! single-component grayscale, precision 2–16 bits. Like the `.70` codec,
//! samples are coded as opaque bit patterns, so signed data round-trips.

use jpegls::EncodeOptions;
use vyna_dcm_core::{FragmentSequence, TransferSyntax};

use crate::jpeg_lossless::{check_mono_16bit, check_raw_len};
use crate::{PixelCodec, PixelCodecError, samples_from_bytes, samples_to_bytes};

/// Pixel codec for JPEG-LS Lossless (`.80`) and Near-lossless (`.81`).
///
/// `near` is the maximum sample error used when *encoding* `.81` frames
/// (0 would be lossless; the decode side always honours the NEAR value stored
/// in the bitstream itself).
#[derive(Clone, Copy, Debug)]
pub struct JpegLsCodec {
    /// Near-lossless error bound for `.81` encodes. Default 2.
    pub near: u8,
}

impl Default for JpegLsCodec {
    fn default() -> Self {
        Self { near: 2 }
    }
}

impl PixelCodec for JpegLsCodec {
    fn syntaxes(&self) -> &[TransferSyntax] {
        &[
            TransferSyntax::JpegLsLossless,
            TransferSyntax::JpegLsNearLossless,
        ]
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
        if !self.syntaxes().contains(&ts) {
            return Err(PixelCodecError::Unsupported(ts));
        }
        check_mono_16bit(samples_per_pixel, bits_allocated)?;
        let data = fragments.joined();
        let (samples, w, h) = jpegls::decode(&data, columns.into(), rows.into())
            .map_err(|e| PixelCodecError::Codec(format!("jpeg-ls decode failed: {e}")))?;
        if w != columns as u32 || h != rows as u32 {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg-ls frame is {w}x{h}, dataset says {columns}x{rows}"
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
        if !self.syntaxes().contains(&ts) {
            return Err(PixelCodecError::Unsupported(ts));
        }
        check_mono_16bit(samples_per_pixel, bits_allocated)?;
        check_raw_len(raw, rows, columns, bits_allocated)?;
        let samples = samples_from_bytes(raw, bits_allocated)?;
        let mut opts = EncodeOptions::default();
        opts.near = if ts == TransferSyntax::JpegLsNearLossless {
            self.near
        } else {
            0
        };
        opts.precision = Some(bits_allocated.max(8) as u8);
        let mut out = Vec::new();
        jpegls::encode_with_options(&samples, columns.into(), rows.into(), &opts, &mut out)
            .map_err(|e| PixelCodecError::Codec(format!("jpeg-ls encode failed: {e}")))?;
        Ok(FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![out],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern_16(w: u16, h: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(w as usize * h as usize * 2);
        for y in 0..h {
            for x in 0..w {
                // Smooth gradient plus flat plateaus: ideal JPEG-LS fodder.
                let v = if (x / 8 + y / 8) % 2 == 0 {
                    1000 + x * 3
                } else {
                    3000 + y * 5
                };
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn lossless_roundtrip_is_exact() {
        let codec = JpegLsCodec::default();
        let raw = pattern_16(33, 17);
        let frags = codec
            .encode_frame(TransferSyntax::JpegLsLossless, &raw, 17, 33, 1, 16)
            .unwrap();
        let back = codec
            .decode_frame(TransferSyntax::JpegLsLossless, &frags, 17, 33, 1, 16)
            .unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn near_lossless_honours_error_bound() {
        let codec = JpegLsCodec { near: 3 };
        let raw = pattern_16(32, 32);
        let frags = codec
            .encode_frame(TransferSyntax::JpegLsNearLossless, &raw, 32, 32, 1, 16)
            .unwrap();
        let back = codec
            .decode_frame(TransferSyntax::JpegLsNearLossless, &frags, 32, 32, 1, 16)
            .unwrap();
        assert_eq!(back.len(), raw.len());
        // NEAR is a hard per-sample guarantee, not an average.
        for (a, b) in raw
            .as_chunks::<2>()
            .0
            .iter()
            .zip(back.as_chunks::<2>().0.iter())
        {
            let (a, b) = (u16::from_le_bytes(*a), u16::from_le_bytes(*b));
            assert!(a.abs_diff(b) <= 3, "{a} vs {b}");
        }
        // And it must actually compress.
        assert!(frags.joined().len() < raw.len());
    }

    #[test]
    fn rejects_other_syntaxes() {
        let codec = JpegLsCodec::default();
        let raw = vec![0u8; 64];
        let err = codec
            .encode_frame(TransferSyntax::JpegBaseline8Bit, &raw, 8, 8, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Unsupported(_)), "{err:?}");
    }
}
