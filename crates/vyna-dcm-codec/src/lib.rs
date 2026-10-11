//! `vyna-dcm-codec`: encoding, files, pixels and JSON.
//!
//! - [`decode_dataset`] / [`encode_dataset`] — dataset codec for every
//!   supported transfer syntax
//! - [`read_part10`] / [`write_part10`] — Part-10 file format
//! - [`rle`] — from-scratch RLE Lossless pixel codec
//! - [`dataset_to_json`] / [`dataset_from_json`] — DICOM JSON Model (PS3.18 F)
//! - [`dump_dataset`] — human-readable text dump
//! - [`PixelCodec`] — plug-in trait for JPEG/JPEG 2000 families
//!
//! # Example
//!
//! ```
//! use vyna_dcm_codec::{decode_dataset, encode_dataset};
//! use vyna_dcm_core::{tags, DataSet, TransferSyntax, Vr};
//!
//! let mut ds = DataSet::new();
//! ds.put_text(tags::MODALITY, Vr::Cs, "ECG");
//! let bytes = encode_dataset(&ds, TransferSyntax::ExplicitVrLittleEndian).unwrap();
//! assert_eq!(decode_dataset(&bytes, TransferSyntax::ExplicitVrLittleEndian).unwrap(), ds);
//! ```

#![warn(missing_docs)]

mod decode;
mod dump;
mod encode;
mod error;
mod json;
mod part10;
pub mod rle;

pub(crate) use decode::decode_up_to;
pub use decode::{UNDEFINED_LENGTH, decode_dataset};
pub use dump::dump_dataset;
pub use encode::{DEFAULT_FRAGMENT_LEN, encode_dataset, fragment_payload};
pub use error::{Error, Result};
pub use json::{dataset_from_json, dataset_to_json};
pub use part10::{FileMeta, META_VERSION, PREFIX_LEN, read_part10, write_part10};
pub use vyna_dcm_core::TransferSyntax;

use vyna_dcm_core::{DataSet, FragmentSequence};

/// Plug-in pixel codec for the JPEG / JPEG 2000 families.
///
/// RLE, uncompressed and deflated syntaxes are built in; compressed still
/// images need an implementation of this trait (e.g. backed by `image`,
/// `jpeg-decoder` or `openjpeg` in downstream crates like VynaLink).
pub trait PixelCodec: Send + Sync {
    /// Transfer syntaxes this codec handles.
    fn syntaxes(&self) -> &[TransferSyntax];

    /// Decode one frame's fragments to raw native-order samples.
    fn decode_frame(
        &self,
        ts: TransferSyntax,
        fragments: &FragmentSequence,
        rows: u16,
        columns: u16,
        samples_per_pixel: u16,
        bits_allocated: u16,
    ) -> std::result::Result<Vec<u8>, PixelCodecError>;

    /// Encode raw native-order samples into one frame's fragments.
    fn encode_frame(
        &self,
        ts: TransferSyntax,
        raw: &[u8],
        rows: u16,
        columns: u16,
        samples_per_pixel: u16,
        bits_allocated: u16,
    ) -> std::result::Result<FragmentSequence, PixelCodecError>;
}

/// Error type for [`PixelCodec`] implementations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PixelCodecError {
    /// Requested transfer syntax is not supported by this codec.
    #[error("unsupported transfer syntax {0:?}")]
    Unsupported(TransferSyntax),
    /// Frame geometry does not match the payload.
    #[error("frame geometry error: {0}")]
    Geometry(String),
    /// Underlying codec failure.
    #[error("codec failure: {0}")]
    Codec(String),
}

/// Decode native (uncompressed/RLE) pixel data of a dataset to raw samples.
///
/// `ds` must carry Rows/Columns/SamplesPerPixel/BitsAllocated plus Pixel
/// Data as bytes (native syntaxes) or fragments (RLE). JPEG-family syntaxes
/// need a [`PixelCodec`]; use [`decode_pixels_with`] for those.
pub fn decode_pixels(ds: &DataSet, ts: TransferSyntax) -> Result<Vec<u8>> {
    decode_pixels_with(ds, ts, None)
}

/// Decode pixel data, using `codec` for JPEG-family transfer syntaxes.
pub fn decode_pixels_with(
    ds: &DataSet,
    ts: TransferSyntax,
    codec: Option<&dyn PixelCodec>,
) -> Result<Vec<u8>> {
    use vyna_dcm_core::{Value, tags};

    let req_u16 = |tag| {
        ds.uint16(tag).ok_or_else(|| Error::BadValue {
            tag,
            vr: vyna_dcm_core::Vr::Us,
            message: "missing pixel descriptor".into(),
        })
    };
    let rows = req_u16(tags::ROWS)?;
    let columns = req_u16(tags::COLUMNS)?;
    let samples = ds.uint16(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    let bits = req_u16(tags::BITS_ALLOCATED)?;
    let bytes_per_sample = bits.div_ceil(8) as usize;
    let frames: usize = ds
        .string(tags::NUMBER_OF_FRAMES)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let frame_len = rows as usize * columns as usize * samples as usize * bytes_per_sample;
    let expected = frame_len * frames;

    let el = ds.get(tags::PIXEL_DATA).ok_or_else(|| Error::BadValue {
        tag: tags::PIXEL_DATA,
        vr: vyna_dcm_core::Vr::Ow,
        message: "missing Pixel Data".into(),
    })?;

    match (&el.value, ts) {
        (Value::Bytes(b), ts) if ts.is_uncompressed() => {
            if b.len() != expected {
                return Err(Error::BadValue {
                    tag: tags::PIXEL_DATA,
                    vr: el.vr,
                    message: format!("pixel data {} bytes, expected {expected}", b.len()),
                });
            }
            Ok(b.clone())
        }
        (Value::Fragments(f), TransferSyntax::RleLossless) => {
            // RLE: one or more segments per frame; without frame offsets,
            // treat each fragment run as one frame when BOT is present,
            // else decode the whole stream as a single frame.
            if frames == 1 || f.basic_offset_table.is_empty() {
                // Concatenate fragments: single-frame fast path. Multi-frame
                // without BOT is ambiguous; decode frame-by-frame only when
                // each fragment holds exactly one frame.
                if frames == 1 {
                    let joined = f.joined();
                    return rle::decode_frame(&joined, frame_len);
                }
                if f.fragments.len() == frames {
                    let mut out = Vec::with_capacity(expected);
                    for frag in &f.fragments {
                        out.extend(rle::decode_frame(frag, frame_len)?);
                    }
                    return Ok(out);
                }
                return Err(Error::Rle(
                    "multi-frame RLE without basic offset table and ambiguous framing".into(),
                ));
            }
            // BOT present: slice the joined stream per frame.
            let joined = f.joined();
            let mut offsets = f.basic_offset_table.clone();
            offsets.push(joined.len() as u32);
            let mut out = Vec::with_capacity(expected);
            for w in offsets.windows(2) {
                let (a, b) = (w[0] as usize, w[1] as usize);
                if b < a || b > joined.len() {
                    return Err(Error::Rle("basic offset table out of range".into()));
                }
                out.extend(rle::decode_frame(&joined[a..b], frame_len)?);
            }
            Ok(out)
        }
        (Value::Fragments(f), ts) => {
            let codec =
                codec.ok_or_else(|| Error::UnsupportedTransferSyntax(ts.uid().to_string()))?;
            if !codec.syntaxes().contains(&ts) {
                return Err(Error::UnsupportedTransferSyntax(ts.uid().to_string()));
            }
            // JPEG family: one fragment per frame (one frame per fragment
            // is the norm; multi-fragment frames are concatenated when BOT
            // is absent and there is a single frame).
            if frames == 1 {
                let single = if f.fragments.len() == 1 {
                    f.clone()
                } else {
                    FragmentSequence {
                        basic_offset_table: vec![],
                        fragments: vec![f.joined()],
                    }
                };
                return codec
                    .decode_frame(ts, &single, rows, columns, samples, bits)
                    .map_err(|e| Error::BadValue {
                        tag: tags::PIXEL_DATA,
                        vr: el.vr,
                        message: e.to_string(),
                    });
            }
            Err(Error::BadValue {
                tag: tags::PIXEL_DATA,
                vr: el.vr,
                message: "multi-frame JPEG needs per-frame fragments/BOT; not yet supported".into(),
            })
        }
        (v, _) => Err(Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr: el.vr,
            message: format!("pixel value shape {v:?} does not match {ts:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::{DataElement, tags};

    const TS_JPEG: TransferSyntax = TransferSyntax::JpegBaseline8Bit;

    fn jpeg_dataset() -> DataSet {
        DataSet::new()
            .with(DataElement::u16_single(tags::ROWS, 2))
            .with(DataElement::u16_single(tags::COLUMNS, 2))
            .with(DataElement::u16_single(tags::SAMPLES_PER_PIXEL, 1))
            .with(DataElement::u16_single(tags::BITS_ALLOCATED, 8))
            .with(DataElement::new(
                tags::PIXEL_DATA,
                vyna_dcm_core::Vr::Ob,
                vyna_dcm_core::Value::Fragments(FragmentSequence {
                    basic_offset_table: vec![],
                    fragments: vec![vec![0xFF, 0xD8, 0xFF, 0xD9]],
                }),
            ))
    }

    struct StubCodec {
        syntaxes: Vec<TransferSyntax>,
        frame: Vec<u8>,
    }

    impl PixelCodec for StubCodec {
        fn syntaxes(&self) -> &[TransferSyntax] {
            &self.syntaxes
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
            assert_eq!(ts, TS_JPEG);
            assert_eq!(
                (rows, columns, samples_per_pixel, bits_allocated),
                (2, 2, 1, 8)
            );
            assert_eq!(fragments.fragments.len(), 1);
            Ok(self.frame.clone())
        }

        fn encode_frame(
            &self,
            _ts: TransferSyntax,
            _raw: &[u8],
            _rows: u16,
            _columns: u16,
            _samples_per_pixel: u16,
            _bits_allocated: u16,
        ) -> std::result::Result<FragmentSequence, PixelCodecError> {
            Err(PixelCodecError::Codec("stub cannot encode".into()))
        }
    }

    #[test]
    fn jpeg_without_codec_errors() {
        let err = decode_pixels(&jpeg_dataset(), TS_JPEG).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedTransferSyntax(ref u) if u == TS_JPEG.uid()),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn jpeg_codec_declining_syntax_errors() {
        let codec = StubCodec {
            syntaxes: vec![],
            frame: vec![],
        };
        let err = decode_pixels_with(&jpeg_dataset(), TS_JPEG, Some(&codec)).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedTransferSyntax(ref u) if u == TS_JPEG.uid()),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn jpeg_stub_codec_dispatches_frame() {
        let codec = StubCodec {
            syntaxes: vec![TS_JPEG],
            frame: vec![1, 2, 3, 4],
        };
        let raw = decode_pixels_with(&jpeg_dataset(), TS_JPEG, Some(&codec)).unwrap();
        assert_eq!(raw, vec![1, 2, 3, 4]);
    }
}
