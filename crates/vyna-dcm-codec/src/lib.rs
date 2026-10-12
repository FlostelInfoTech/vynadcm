//! `vyna-dcm-codec`: encoding, files, pixels and JSON.
//!
//! - [`decode_dataset`] / [`encode_dataset`] — dataset codec for every
//!   supported transfer syntax
//! - [`read_part10`] / [`write_part10`] — Part-10 file format
//! - [`rle`] — from-scratch RLE Lossless pixel codec
//! - [`dataset_to_json`] / [`dataset_from_json`] — DICOM JSON Model (PS3.18 F)
//! - [`dump_dataset`] — human-readable text dump
//! - [`PixelCodec`] — plug-in trait for JPEG/JPEG 2000 families
//! - [`BundledCodec`] — built-in [`PixelCodec`] for all bundled JPEG syntaxes
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

pub mod charset;
mod decode;
pub mod dicomdir;
mod dump;
mod encode;
mod error;
pub mod jpeg;
pub mod jpeg_2000;
pub mod jpeg_lossless;
pub mod jpeg_ls;
mod json;
mod part10;
pub mod rle;
pub mod transcode;

pub use decode::{UNDEFINED_LENGTH, decode_dataset};
pub(crate) use decode::{decode_dataset_capture_items, decode_up_to};
pub use dicomdir::{
    DEFAULT_FILE_SET_ID, DicomDir, DirEntry, DirFile, DirInstance, DirPatient, DirSeries, DirStudy,
    SkippedRecord, build_dicomdir, read_dicomdir, record_type_for_sop_class,
};
pub use dump::dump_dataset;
pub(crate) use encode::encode_dataset_capture_items;
pub use encode::{DEFAULT_FRAGMENT_LEN, encode_dataset, fragment_payload};
pub use error::{Error, Result};
pub use jpeg::JpegCodec;
pub use jpeg_2000::Jpeg2000Codec;
pub use jpeg_lossless::JpegLosslessCodec;
pub use jpeg_ls::JpegLsCodec;
pub use json::{dataset_from_json, dataset_to_json};
pub(crate) use part10::meta_section_len;
pub use part10::{FileMeta, META_VERSION, PREFIX_LEN, read_part10, write_part10};
pub use transcode::{TranscodeOptions, transcode_dataset};
pub use vyna_dcm_core::TransferSyntax;

use vyna_dcm_core::{DataSet, FragmentSequence};

/// Native-order samples as u16 values (little-endian pairs past 8 bits).
pub(crate) fn samples_from_bytes(
    raw: &[u8],
    bits_allocated: u16,
) -> std::result::Result<Vec<u16>, PixelCodecError> {
    if bits_allocated <= 8 {
        Ok(raw.iter().map(|&b| b as u16).collect())
    } else if bits_allocated <= 16 {
        if !raw.len().is_multiple_of(2) {
            return Err(PixelCodecError::Geometry(format!(
                "odd raw length {} at {bits_allocated} bits allocated",
                raw.len()
            )));
        }
        Ok(raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect())
    } else {
        Err(PixelCodecError::Geometry(format!(
            "bits allocated {bits_allocated} outside the 1..=16 range"
        )))
    }
}

/// Stored frame length: `YBR_FULL_422` packs 2 bytes per pixel on
/// the wire; every other photometric stores the decoded length.
/// (Decoders return RGB for 422 JPEG data, so only native and RLE
/// paths — which preserve stored bytes — use this.)
pub(crate) fn stored_frame_len(ds: &DataSet, rows: u16, columns: u16, decoded_len: usize) -> usize {
    use vyna_dcm_core::tags;
    let is_422 = ds
        .string(tags::PHOTOMETRIC_INTERPRETATION)
        .is_some_and(|s| s.trim() == "YBR_FULL_422");
    if is_422 {
        rows as usize * columns as usize * 2
    } else {
        decoded_len
    }
}

/// u16 samples back to native-order bytes.
pub(crate) fn samples_to_bytes(
    samples: &[u16],
    bits_allocated: u16,
) -> std::result::Result<Vec<u8>, PixelCodecError> {
    if bits_allocated <= 8 {
        let mut out = Vec::with_capacity(samples.len());
        for &s in samples {
            if s > 0xFF {
                return Err(PixelCodecError::Geometry(format!(
                    "sample {s} does not fit in {bits_allocated} bits"
                )));
            }
            out.push(s as u8);
        }
        Ok(out)
    } else if bits_allocated <= 16 {
        let mut out = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        Ok(out)
    } else {
        Err(PixelCodecError::Geometry(format!(
            "bits allocated {bits_allocated} outside the 1..=16 range"
        )))
    }
}

/// Plug-in pixel codec for the JPEG / JPEG 2000 families.
///
/// RLE, uncompressed and deflated syntaxes are built in. The JPEG families
/// are served by the bundled backends (see [`BundledCodec`]); implement this
/// trait only to override them (e.g. with a hardware codec).
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

/// Built-in [`PixelCodec`] for every bundled JPEG transfer syntax.
///
/// Dispatches `.50` to [`JpegCodec`], `.70` to [`JpegLosslessCodec`], `.80`
/// / `.81` to [`JpegLsCodec`] and `.90` / `.91` to [`Jpeg2000Codec`]. The
/// sub-codecs are public fields so callers can tune quality knobs:
///
/// ```
/// use vyna_dcm_codec::{BundledCodec, PixelCodec};
///
/// let mut codec = BundledCodec::default();
/// codec.jpeg.quality = 95;
/// assert_eq!(codec.syntaxes().len(), 6);
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct BundledCodec {
    /// JPEG Baseline (`.50`) backend.
    pub jpeg: JpegCodec,
    /// JPEG Lossless (`.70`) backend.
    pub jpeg_lossless: JpegLosslessCodec,
    /// JPEG-LS (`.80` / `.81`) backend.
    pub jpeg_ls: JpegLsCodec,
    /// JPEG 2000 (`.90` / `.91`) backend.
    pub jpeg_2000: Jpeg2000Codec,
}

impl BundledCodec {
    /// Transfer syntaxes the bundled backends handle.
    pub const SYNTAXES: [TransferSyntax; 6] = [
        TransferSyntax::JpegBaseline8Bit,
        TransferSyntax::JpegLossless,
        TransferSyntax::JpegLsLossless,
        TransferSyntax::JpegLsNearLossless,
        TransferSyntax::Jpeg2000Lossless,
        TransferSyntax::Jpeg2000,
    ];

    fn backend(&self, ts: TransferSyntax) -> Option<&dyn PixelCodec> {
        match ts {
            TransferSyntax::JpegBaseline8Bit => Some(&self.jpeg),
            TransferSyntax::JpegLossless => Some(&self.jpeg_lossless),
            TransferSyntax::JpegLsLossless | TransferSyntax::JpegLsNearLossless => {
                Some(&self.jpeg_ls)
            }
            TransferSyntax::Jpeg2000Lossless | TransferSyntax::Jpeg2000 => Some(&self.jpeg_2000),
            _ => None,
        }
    }
}

impl PixelCodec for BundledCodec {
    fn syntaxes(&self) -> &[TransferSyntax] {
        &Self::SYNTAXES
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
        self.backend(ts)
            .ok_or(PixelCodecError::Unsupported(ts))?
            .decode_frame(
                ts,
                fragments,
                rows,
                columns,
                samples_per_pixel,
                bits_allocated,
            )
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
        self.backend(ts)
            .ok_or(PixelCodecError::Unsupported(ts))?
            .encode_frame(ts, raw, rows, columns, samples_per_pixel, bits_allocated)
    }
}

/// Decode pixel data of a dataset to raw samples.
///
/// `ds` must carry Rows/Columns/SamplesPerPixel/BitsAllocated plus Pixel
/// Data as bytes (native syntaxes) or fragments (encapsulated syntaxes).
/// Native, RLE, and deflated syntaxes are built in; JPEG-family syntaxes
/// use the bundled backends ([`BundledCodec::default()`]).
pub fn decode_pixels(ds: &DataSet, ts: TransferSyntax) -> Result<Vec<u8>> {
    decode_pixels_with(ds, ts, Some(&BundledCodec::default()))
}

/// Decode pixel data, using `codec` for JPEG-family transfer syntaxes.
///
/// Pass an explicit codec to override the bundled backends (e.g. with a
/// hardware implementation), or `None` to reject JPEG-family syntaxes with
/// [`Error::UnsupportedTransferSyntax`].
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
    let stored_len = stored_frame_len(ds, rows, columns, frame_len);
    let stored_expected = stored_len * frames;

    let el = ds.get(tags::PIXEL_DATA).ok_or_else(|| Error::BadValue {
        tag: tags::PIXEL_DATA,
        vr: vyna_dcm_core::Vr::Ow,
        message: "missing Pixel Data".into(),
    })?;

    match (&el.value, ts) {
        (Value::Bytes(b), ts) if ts.is_uncompressed() => {
            if b.len() != stored_expected {
                return Err(Error::BadValue {
                    tag: tags::PIXEL_DATA,
                    vr: el.vr,
                    message: format!("pixel data {} bytes, expected {stored_expected}", b.len()),
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
                    return rle::decode_frame(&joined, stored_len);
                }
                if f.fragments.len() == frames {
                    let mut out = Vec::with_capacity(stored_expected);
                    for frag in &f.fragments {
                        out.extend(rle::decode_frame(frag, stored_len)?);
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
            let mut out = Vec::with_capacity(stored_expected);
            for w in offsets.windows(2) {
                let (a, b) = (w[0] as usize, w[1] as usize);
                if b < a || b > joined.len() {
                    return Err(Error::Rle("basic offset table out of range".into()));
                }
                out.extend(rle::decode_frame(&joined[a..b], stored_len)?);
            }
            Ok(out)
        }
        (Value::Fragments(f), ts) => {
            let codec =
                codec.ok_or_else(|| Error::UnsupportedTransferSyntax(ts.uid().to_string()))?;
            if !codec.syntaxes().contains(&ts) {
                return Err(Error::UnsupportedTransferSyntax(ts.uid().to_string()));
            }
            // JPEG family framing: the basic offset table wins when present;
            // otherwise one fragment per frame, with a single frame allowed
            // to span several concatenated fragments.
            let blobs: Vec<Vec<u8>> = if frames == 1 {
                vec![f.joined()]
            } else if !f.basic_offset_table.is_empty() {
                let joined = f.joined();
                let mut offsets = f.basic_offset_table.clone();
                offsets.push(joined.len() as u32);
                if offsets.len() != frames + 1 {
                    return Err(Error::BadValue {
                        tag: tags::PIXEL_DATA,
                        vr: el.vr,
                        message: format!(
                            "basic offset table holds {} frames, dataset says {frames}",
                            offsets.len() - 1
                        ),
                    });
                }
                let mut blobs = Vec::with_capacity(frames);
                for w in offsets.windows(2) {
                    let (a, b) = (w[0] as usize, w[1] as usize);
                    if b < a || b > joined.len() {
                        return Err(Error::BadValue {
                            tag: tags::PIXEL_DATA,
                            vr: el.vr,
                            message: "basic offset table out of range".into(),
                        });
                    }
                    blobs.push(joined[a..b].to_vec());
                }
                blobs
            } else if f.fragments.len() == frames {
                f.fragments.clone()
            } else {
                return Err(Error::BadValue {
                    tag: tags::PIXEL_DATA,
                    vr: el.vr,
                    message: format!(
                        "multi-frame JPEG without basic offset table needs one fragment per frame ({} fragments, {frames} frames)",
                        f.fragments.len()
                    ),
                });
            };
            let mut out = Vec::with_capacity(expected);
            for blob in &blobs {
                let single = FragmentSequence {
                    basic_offset_table: vec![],
                    fragments: vec![blob.clone()],
                };
                let frame = codec
                    .decode_frame(ts, &single, rows, columns, samples, bits)
                    .map_err(|e| Error::BadValue {
                        tag: tags::PIXEL_DATA,
                        vr: el.vr,
                        message: e.to_string(),
                    })?;
                if frame.len() != frame_len {
                    return Err(Error::BadValue {
                        tag: tags::PIXEL_DATA,
                        vr: el.vr,
                        message: format!(
                            "codec returned {} bytes for a {frame_len}-byte frame",
                            frame.len()
                        ),
                    });
                }
                out.extend(frame);
            }
            Ok(out)
        }
        (v, _) => Err(Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr: el.vr,
            message: format!("pixel value shape {v:?} does not match {ts:?}"),
        }),
    }
}

/// Encode raw native-order samples into encapsulated fragments.
///
/// `ds` supplies the frame geometry (Rows/Columns/SamplesPerPixel/
/// BitsAllocated/NumberOfFrames); `raw` must hold exactly `frames` frames.
/// RLE is built in; JPEG-family syntaxes use `codec`, defaulting to the
/// bundled backends ([`BundledCodec::default()`]) when `None`. Native
/// syntaxes are rejected — raw samples already are the native encoding.
pub fn encode_pixels(
    ds: &DataSet,
    ts: TransferSyntax,
    raw: &[u8],
    codec: Option<&dyn PixelCodec>,
) -> Result<FragmentSequence> {
    use vyna_dcm_core::{Vr, tags};

    let req_u16 = |tag| {
        ds.uint16(tag).ok_or_else(|| Error::BadValue {
            tag,
            vr: Vr::Us,
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
    // RLE preserves stored bytes (possibly subsampled); JPEG encoders
    // take decoded RGB/mono.
    let rle_frame_len = stored_frame_len(ds, rows, columns, frame_len);
    let want_frame_len = if ts == TransferSyntax::RleLossless {
        rle_frame_len
    } else {
        frame_len
    };
    if raw.len() != want_frame_len * frames {
        return Err(Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr: Vr::Ow,
            message: format!(
                "raw is {} bytes, expected {}",
                raw.len(),
                want_frame_len * frames
            ),
        });
    }
    if ts == TransferSyntax::RleLossless {
        let mut fragments = Vec::with_capacity(frames);
        for n in 0..frames {
            let frame = &raw[n * rle_frame_len..(n + 1) * rle_frame_len];
            fragments.push(rle::encode_frame(frame, bytes_per_sample)?);
        }
        return Ok(FragmentSequence {
            basic_offset_table: vec![],
            fragments,
        });
    }
    if ts.is_uncompressed() || ts.is_deflated() {
        return Err(Error::UnsupportedTransferSyntax(ts.uid().to_string()));
    }
    let bundled = BundledCodec::default();
    let codec: &dyn PixelCodec = codec.unwrap_or(&bundled);
    if !codec.syntaxes().contains(&ts) {
        return Err(Error::UnsupportedTransferSyntax(ts.uid().to_string()));
    }
    let mut fragments = Vec::with_capacity(frames);
    for n in 0..frames {
        let frame = &raw[n * frame_len..(n + 1) * frame_len];
        let mut seq = codec
            .encode_frame(ts, frame, rows, columns, samples, bits)
            .map_err(|e| Error::BadValue {
                tag: tags::PIXEL_DATA,
                vr: Vr::Ob,
                message: e.to_string(),
            })?;
        fragments.append(&mut seq.fragments);
    }
    Ok(FragmentSequence {
        basic_offset_table: vec![],
        fragments,
    })
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
        let err = decode_pixels_with(&jpeg_dataset(), TS_JPEG, None).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedTransferSyntax(ref u) if u == TS_JPEG.uid()),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn decode_pixels_uses_bundled_by_default() {
        use vyna_dcm_core::{Value, Vr};
        let ts = TransferSyntax::JpegLsLossless;
        let raw: Vec<u8> = (0..16 * 16u16)
            .flat_map(|i| (2000 + i).to_le_bytes())
            .collect();
        let ds = geometry_ds(16, 16, 1);
        let frags = encode_pixels(&ds, ts, &raw, None).unwrap();
        let mut framed = ds.clone();
        framed.insert(DataElement::new(
            tags::PIXEL_DATA,
            Vr::Ob,
            Value::Fragments(frags),
        ));
        // No explicit codec: the bundled backend handles it.
        assert_eq!(decode_pixels(&framed, ts).unwrap(), raw);
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

    #[test]
    fn bundled_codec_covers_jpeg_family() {
        let codec = BundledCodec::default();
        assert_eq!(BundledCodec::SYNTAXES.len(), 6);
        for ts in BundledCodec::SYNTAXES {
            assert!(codec.syntaxes().contains(&ts));
        }
        // Everything else stays unsupported.
        for ts in [
            TransferSyntax::JpegExtended12Bit,
            TransferSyntax::Jpeg2000Part2,
            TransferSyntax::HtJpeg2000,
            TransferSyntax::RleLossless,
        ] {
            let frags = FragmentSequence {
                basic_offset_table: vec![],
                fragments: vec![vec![0]],
            };
            let err = codec.decode_frame(ts, &frags, 1, 1, 1, 8).unwrap_err();
            assert!(
                matches!(err, PixelCodecError::Unsupported(_)),
                "{ts:?}: {err:?}"
            );
        }
    }

    fn geometry_ds(rows: u16, cols: u16, frames: usize) -> DataSet {
        DataSet::new()
            .with(DataElement::u16_single(tags::ROWS, rows))
            .with(DataElement::u16_single(tags::COLUMNS, cols))
            .with(DataElement::u16_single(tags::SAMPLES_PER_PIXEL, 1))
            .with(DataElement::u16_single(tags::BITS_ALLOCATED, 16))
            .with(DataElement::text(
                tags::NUMBER_OF_FRAMES,
                vyna_dcm_core::Vr::Is,
                frames.to_string(),
            ))
    }

    #[test]
    fn bundled_multi_frame_roundtrip() {
        use vyna_dcm_core::{Value, Vr};
        let codec = BundledCodec::default();
        let ts = TransferSyntax::JpegLsLossless;
        // Three distinct frames.
        let mut raw = Vec::new();
        for f in 0..3u16 {
            for i in 0..16 * 16u16 {
                raw.extend_from_slice(&(1000 + f * 500 + i).to_le_bytes());
            }
        }
        let ds = geometry_ds(16, 16, 3);
        let frags = encode_pixels(&ds, ts, &raw, Some(&codec)).unwrap();
        assert_eq!(frags.fragments.len(), 3);

        // Decode path 1: one fragment per frame, no offset table.
        let mut framed = ds.clone();
        framed.insert(DataElement::new(
            tags::PIXEL_DATA,
            Vr::Ob,
            Value::Fragments(frags.clone()),
        ));
        let back = decode_pixels_with(&framed, ts, Some(&codec)).unwrap();
        assert_eq!(back, raw);

        // Decode path 2: basic offset table slicing.
        let mut offsets = Vec::with_capacity(3);
        let mut acc = 0u32;
        for frag in &frags.fragments {
            offsets.push(acc);
            acc += frag.len() as u32;
        }
        let bot = FragmentSequence {
            basic_offset_table: offsets,
            fragments: frags.fragments.clone(),
        };
        framed.insert(DataElement::new(
            tags::PIXEL_DATA,
            Vr::Ob,
            Value::Fragments(bot),
        ));
        let back = decode_pixels_with(&framed, ts, Some(&codec)).unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn encode_pixels_rle_native_and_bundled_default() {
        let ds = geometry_ds(8, 8, 1);
        let raw = vec![7u8; 8 * 8 * 2];
        // RLE needs no codec.
        let frags = encode_pixels(&ds, TransferSyntax::RleLossless, &raw, None).unwrap();
        assert_eq!(frags.fragments.len(), 1);
        // Native syntaxes are rejected: raw already is the encoding.
        let err =
            encode_pixels(&ds, TransferSyntax::ExplicitVrLittleEndian, &raw, None).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedTransferSyntax(_)),
            "{err:?}"
        );
        // JPEG with no codec falls back to the bundled backend.
        let frags = encode_pixels(&ds, TransferSyntax::JpegLsLossless, &raw, None).unwrap();
        assert_eq!(frags.fragments.len(), 1);
        // ...but a codec that declines the syntax is still rejected.
        let declining = StubCodec {
            syntaxes: vec![],
            frame: vec![],
        };
        let err =
            encode_pixels(&ds, TransferSyntax::JpegLsLossless, &raw, Some(&declining)).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedTransferSyntax(_)),
            "{err:?}"
        );
    }
}
