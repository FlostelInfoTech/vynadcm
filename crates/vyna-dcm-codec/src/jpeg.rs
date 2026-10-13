//! JPEG Baseline 8-bit pixel codec (Transfer Syntax `1.2.840.10008.1.2.4.50`).
//!
//! Decode uses `jpeg-decoder`, encode uses `jpeg-encoder` (both pure Rust).
//! Only 8-bit monochrome and interleaved RGB frames are supported, matching
//! what the baseline process can carry.

use std::io::Cursor;

use jpeg_decoder::{Decoder, PixelFormat};
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use vyna_dcm_core::{FragmentSequence, TransferSyntax};

use crate::{PixelCodec, PixelCodecError};

/// Pixel codec for JPEG Baseline 8-bit (`.50`).
///
/// `quality` is the encoder quality 1–100 (clamped); the DICOM-conventional
/// default is 90.
#[derive(Clone, Copy, Debug)]
pub struct JpegCodec {
    /// Encoder quality, 1 (smallest) to 100 (best). Default 90.
    pub quality: u8,
}

impl Default for JpegCodec {
    fn default() -> Self {
        Self { quality: 90 }
    }
}

impl PixelCodec for JpegCodec {
    fn syntaxes(&self) -> &[TransferSyntax] {
        &[TransferSyntax::JpegBaseline8Bit]
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
        if ts != TransferSyntax::JpegBaseline8Bit {
            return Err(PixelCodecError::Unsupported(ts));
        }
        if bits_allocated != 8 {
            return Err(PixelCodecError::Geometry(format!(
                "JPEG baseline needs 8 bits allocated, dataset says {bits_allocated}"
            )));
        }
        let data = fragments.joined();
        let mut decoder = Decoder::new(Cursor::new(&data));
        // Cap the decode buffer at the dataset geometry (checked: u16
        // triples can overflow 32-bit usize) and pre-validate the SOF
        // header before any pixel allocation, so a tiny frame claiming
        // huge dimensions fails fast instead of triggering a multi-GB
        // alloc.
        let expected_len = (rows as usize)
            .checked_mul(columns as usize)
            .and_then(|n| n.checked_mul(samples_per_pixel as usize))
            .ok_or_else(|| {
                PixelCodecError::Geometry(format!(
                    "jpeg geometry {rows}x{columns}x{samples_per_pixel} overflows address space"
                ))
            })?;
        decoder.set_max_decoding_buffer_size(expected_len);
        decoder
            .read_info()
            .map_err(|e| PixelCodecError::Codec(format!("jpeg header parse failed: {e}")))?;
        let info = decoder
            .info()
            .ok_or_else(|| PixelCodecError::Codec("jpeg decoder produced no image info".into()))?;
        if info.width != columns || info.height != rows {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg frame is {}x{}, dataset says {columns}x{rows}",
                info.width, info.height
            )));
        }
        let expected_samples = match info.pixel_format {
            PixelFormat::L8 => 1,
            PixelFormat::RGB24 => 3,
            PixelFormat::L16 => {
                return Err(PixelCodecError::Codec(
                    "jpeg frame uses lossless/high-precision coding; use the .70 codec".into(),
                ));
            }
            PixelFormat::CMYK32 => {
                return Err(PixelCodecError::Codec(
                    "CMYK jpeg frames are not supported".into(),
                ));
            }
        };
        if samples_per_pixel != expected_samples {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg frame has {expected_samples} component(s), dataset says {samples_per_pixel}"
            )));
        }
        // Header matches the dataset geometry; decode within the cap set above.
        let pixels = decoder
            .decode()
            .map_err(|e| PixelCodecError::Codec(format!("jpeg decode failed: {e}")))?;
        if pixels.len() != expected_len {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg decoded {} bytes, expected {expected_len}",
                pixels.len()
            )));
        }
        Ok(pixels)
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
        if ts != TransferSyntax::JpegBaseline8Bit {
            return Err(PixelCodecError::Unsupported(ts));
        }
        if bits_allocated != 8 {
            return Err(PixelCodecError::Geometry(format!(
                "JPEG baseline needs 8 bits allocated, dataset says {bits_allocated}"
            )));
        }
        let color = match samples_per_pixel {
            1 => ColorType::Luma,
            3 => ColorType::Rgb,
            n => {
                return Err(PixelCodecError::Geometry(format!(
                    "JPEG baseline needs 1 or 3 samples per pixel, got {n}"
                )));
            }
        };
        let expected_len = rows as usize * columns as usize * samples_per_pixel as usize;
        if raw.len() != expected_len {
            return Err(PixelCodecError::Geometry(format!(
                "raw frame is {} bytes, expected {expected_len}",
                raw.len()
            )));
        }
        let mut out = Vec::new();
        let mut encoder = Encoder::new(&mut out, self.quality.clamp(1, 100));
        if samples_per_pixel == 3 {
            // DICOM mandates 4:2:2 chroma for Baseline color (the crate
            // defaults to 4:4:4 at q90+ and 4:2:0 below); average matches
            // libjpeg's downsampling.
            encoder.set_sampling_factor(SamplingFactor::R_4_2_2);
            encoder.set_chroma_subsampling_method(jpeg_encoder::ChromaSubsamplingMethod::Average);
        }
        encoder
            .encode(raw, columns, rows, color)
            .map_err(|e| PixelCodecError::Codec(format!("jpeg encode failed: {e}")))?;
        Ok(FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![out],
        })
    }
}

/// Mean absolute per-byte difference between two equal-length buffers.
#[cfg(test)]
pub(crate) fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    if a.is_empty() {
        return 0.0;
    }
    let total: u64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y) as u64)
        .sum();
    total as f64 / a.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pattern exercising flat areas, gradients and impulses.
    fn pattern(w: u16, h: u16, samples: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(w as usize * h as usize * samples as usize);
        for y in 0..h {
            for x in 0..w {
                for s in 0..samples {
                    let v =
                        (x as u32 * 7 + y as u32 * 13 + s as u32 * 41 + ((x ^ y) as u32 * 3)) % 256;
                    // An impulse every 32 pixels stresses high frequencies.
                    let v = if (x + y) % 32 == 0 { 255 - v } else { v };
                    out.push(v as u8);
                }
            }
        }
        out
    }

    #[test]
    fn baseline_mono_roundtrip_is_close() {
        let codec = JpegCodec::default();
        let (rows, cols) = (17, 33); // odd sizes: partial edge blocks
        let raw = pattern(cols, rows, 1);
        let frags = codec
            .encode_frame(TransferSyntax::JpegBaseline8Bit, &raw, rows, cols, 1, 8)
            .unwrap();
        assert_eq!(frags.fragments.len(), 1);
        let back = codec
            .decode_frame(TransferSyntax::JpegBaseline8Bit, &frags, rows, cols, 1, 8)
            .unwrap();
        assert_eq!(back.len(), raw.len());
        let err = mean_abs_diff(&raw, &back);
        assert!(err < 8.0, "mean abs err {err} too high at q90");
    }

    #[test]
    fn baseline_rgb_roundtrip_is_close() {
        let codec = JpegCodec::default();
        let (rows, cols) = (16, 16);
        let raw = pattern(cols, rows, 3);
        let frags = codec
            .encode_frame(TransferSyntax::JpegBaseline8Bit, &raw, rows, cols, 3, 8)
            .unwrap();
        let back = codec
            .decode_frame(TransferSyntax::JpegBaseline8Bit, &frags, rows, cols, 3, 8)
            .unwrap();
        let err = mean_abs_diff(&raw, &back);
        // 4:2:2 subsampling costs chroma detail on this adversarial
        // pattern (per-pixel chroma swings); real images do better.
        assert!(err < 14.0, "mean abs err {err} too high at q90");
        assert_sof0_sampling(&frags.fragments.concat(), &[0x21, 0x11, 0x11]);
    }

    #[test]
    fn baseline_rgb_odd_sizes_roundtrip() {
        let codec = JpegCodec::default();
        let (rows, cols) = (17, 33);
        let raw = pattern(cols, rows, 3);
        let frags = codec
            .encode_frame(TransferSyntax::JpegBaseline8Bit, &raw, rows, cols, 3, 8)
            .unwrap();
        let back = codec
            .decode_frame(TransferSyntax::JpegBaseline8Bit, &frags, rows, cols, 3, 8)
            .unwrap();
        assert_eq!(back.len(), raw.len());
        let err = mean_abs_diff(&raw, &back);
        assert!(err < 16.0, "mean abs err {err} too high at q90");
        assert_sof0_sampling(&frags.fragments.concat(), &[0x21, 0x11, 0x11]);
    }

    /// Assert the SOF0 component sampling factors of a JPEG bitstream
    /// (e.g. `21 11 11` for 4:2:2 color (luma H=2, V=1)).
    fn assert_sof0_sampling(jpeg: &[u8], want: &[u8]) {
        let mut pos = jpeg
            .windows(2)
            .position(|w| w == [0xFF, 0xC0])
            .expect("no SOF0 marker")
            + 4; // marker + length
        pos += 1 + 2 + 2; // precision + height + width
        let ncomp = jpeg[pos] as usize;
        pos += 1;
        assert_eq!(ncomp, want.len(), "component count");
        let mut got = Vec::with_capacity(ncomp);
        for _ in 0..ncomp {
            got.push(jpeg[pos + 1]); // H/V sampling nibbles
            pos += 3;
        }
        assert_eq!(got, want);
    }

    #[test]
    fn rejects_wrong_geometry_and_syntax() {
        let codec = JpegCodec::default();
        let raw = pattern(8, 8, 1);
        let frags = codec
            .encode_frame(TransferSyntax::JpegBaseline8Bit, &raw, 8, 8, 1, 8)
            .unwrap();
        // Wrong declared size.
        let err = codec
            .decode_frame(TransferSyntax::JpegBaseline8Bit, &frags, 8, 9, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
        // Wrong component count.
        let err = codec
            .decode_frame(TransferSyntax::JpegBaseline8Bit, &frags, 8, 8, 3, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
        // 12-bit extended is a different transfer syntax.
        let err = codec
            .decode_frame(TransferSyntax::JpegExtended12Bit, &frags, 8, 8, 1, 8)
            .unwrap_err();
        assert!(
            matches!(
                err,
                PixelCodecError::Unsupported(TransferSyntax::JpegExtended12Bit)
            ),
            "{err:?}"
        );
        // Non-8-bit dataset.
        let err = codec
            .encode_frame(TransferSyntax::JpegBaseline8Bit, &raw, 8, 8, 1, 16)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
        // Garbage is a codec error, not a panic.
        let garbage = FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![vec![0, 1, 2, 3, 4]],
        };
        let err = codec
            .decode_frame(TransferSyntax::JpegBaseline8Bit, &garbage, 8, 8, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Codec(_)), "{err:?}");
    }

    #[test]
    fn huge_sof_dims_rejected_before_decode_alloc() {
        // A 15-byte frame claiming 60000x60000 mono (~3.6 GB decoded)
        // must fail fast on the SOF header, not attempt the allocation.
        let sof = [
            0xFF, 0xD8, // SOI
            0xFF, 0xC0, 0x00, 0x0B, 0x08, // SOF0, len 11, 8-bit
            0xEA, 0x60, 0xEA, 0x60, // 60000 x 60000
            0x01, // one component
            0x01, 0x11, 0x00, // id 1, H1V1, table 0
        ];
        let frags = FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![sof.to_vec()],
        };
        let codec = JpegCodec::default();
        let err = codec
            .decode_frame(TransferSyntax::JpegBaseline8Bit, &frags, 8, 8, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
    }
}
