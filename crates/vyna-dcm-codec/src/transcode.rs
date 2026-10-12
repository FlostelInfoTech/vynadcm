//! Pixel-data transcoding across transfer syntaxes.
//!
//! [`transcode_dataset`] decodes pixel data from its source transfer
//! syntax and re-encodes it for a target syntax, updating the pixel
//! descriptors (Photometric Interpretation, Planar Configuration,
//! lossy flags, Pixel Data) so the result stays a valid instance:
//!
//! - Supported sources and targets: the four native syntaxes
//!   (explicit/implicit LE, deflated, big endian), RLE, and the six
//!   bundled JPEG syntaxes (`.50 .70 .80 .81 .90 .91`). Anything else
//!   (`.51`, JPEG 2000 Part 2, HTJ2K) is rejected.
//! - Same-syntax transcoding is a no-op returning the dataset unchanged.
//! - Color normalizes through interleaved RGB: planar data is
//!   de-interleaved, `YBR_FULL` / `YBR_FULL_422` convert via BT.601
//!   (nearest-neighbor chroma upsampling), and `YBR_ICT` / `YBR_RCT`
//!   arrive as RGB from the JPEG 2000 decoder. Targets pick the
//!   photometric their bitstream needs (`.50` → `YBR_FULL_422`,
//!   `.90` → `YBR_RCT`, `.91` → `YBR_ICT`, otherwise unchanged).
//! - Lossy targets (`.50 .81 .91`) get a fresh SOP Instance UID (new
//!   pixels); lossless targets keep it. `Lossy Image Compression`
//!   stays `01` once data has ever been lossy; the ratio and method
//!   follow the target. Encapsulation leftovers (extended offset
//!   tables, total length) are dropped.
//!
//! Out of scope: palette expansion (`PALETTE COLOR` sources are
//! rejected), `YBR_PARTIAL_*` photometrics, SOP-constrains-syntax
//! checks (the caller picks a legal target), and recomputing
//! Smallest/Largest Image Pixel Value after lossy steps.

use vyna_dcm_core::{DataElement, DataSet, Tag, TransferSyntax, Value, Vr, tags};

use crate::{BundledCodec, Error, PixelCodec, Result, decode_pixels_with, encode_pixels};

/// Maximum fragment size on the wire (64 KiB, PS3.5 §8.2).
const MAX_FRAGMENT: usize = 65536;

/// Knobs for lossy target encoders.
#[derive(Clone, Copy, Debug, Default)]
pub struct TranscodeOptions {
    /// Bundled JPEG backends (tune `jpeg.quality`, `jpeg_ls.near`,
    /// `jpeg_2000.lossy_ratio` for `.50` / `.81` / `.91` targets).
    pub codec: BundledCodec,
}

/// Transcode a dataset's pixel data from `source_ts` to `target_ts`.
///
/// Returns a new dataset; the input is untouched. Same-syntax calls
/// return the dataset unchanged.
pub fn transcode_dataset(
    ds: &DataSet,
    source_ts: TransferSyntax,
    target_ts: TransferSyntax,
    opts: &TranscodeOptions,
) -> Result<DataSet> {
    if source_ts == target_ts {
        return Ok(ds.clone());
    }
    if !is_supported(source_ts) || !is_supported(target_ts) {
        let ts = if !is_supported(source_ts) {
            source_ts
        } else {
            target_ts
        };
        return Err(Error::UnsupportedTransferSyntax(format!(
            "{} (transcoding supports the native syntaxes, RLE, and .50/.70/.80/.81/.90/.91)",
            ts.uid()
        )));
    }
    let geo = Geometry::read(ds)?;
    geo.check_source_photometric(source_ts)?;
    geo.check_target(target_ts)?;

    let codec: &dyn PixelCodec = &opts.codec;
    let mut raw = decode_pixels_with(ds, source_ts, Some(codec))?;
    if source_ts == TransferSyntax::ExplicitVrBigEndian && geo.bits > 8 {
        swap_pairs(&mut raw);
    }
    // JPEG decoders return interleaved RGB by construction, so planar
    // only applies to stored (native/RLE) bytes.
    let jpeg_source = BundledCodec::SYNTAXES.contains(&source_ts);
    if geo.planar == 1 && !jpeg_source {
        if geo.photometric == Photometric::YbrFull422 {
            return Err(Error::BadValue {
                tag: tags::PLANAR_CONFIGURATION,
                vr: Vr::Us,
                message: "planar YBR_FULL_422 has unequal planes and is not supported".into(),
            });
        }
        raw = deinterleave_planar(&raw, &geo);
    }
    // Decoded bytes are RGB when stored that way or when a JPEG
    // decoder produced them (native/RLE YBR stays stored-form).
    let decoded_rgb = geo.samples == 3
        && (geo.photometric == Photometric::Rgb || (geo.photometric.is_ybr() && jpeg_source));
    if geo.needs_ybr_to_rgb(target_ts, decoded_rgb) {
        raw = ybr_to_rgb(&raw, &geo)?;
    }

    let target_photo = geo.target_photometric(target_ts, decoded_rgb);
    let out = encode_target(ds, &geo, &raw, target_ts, codec)?;

    let mut out_ds = ds.clone();
    let EncodedTarget {
        element,
        compressed_len,
    } = out;
    out_ds.insert(element);
    out_ds.put_text(
        tags::PHOTOMETRIC_INTERPRETATION,
        Vr::Cs,
        target_photo.as_str(),
    );
    if geo.samples == 1 {
        out_ds.remove(tags::PLANAR_CONFIGURATION);
    } else {
        out_ds.insert(DataElement::u16_single(tags::PLANAR_CONFIGURATION, 0));
    }
    update_lossy_tags(&mut out_ds, ds, source_ts, target_ts, &raw, compressed_len);
    out_ds.remove(tags::EXTENDED_OFFSET_TABLE);
    out_ds.remove(tags::EXTENDED_OFFSET_TABLE_LENGTHS);
    out_ds.remove(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH);
    if target_ts.is_lossy() {
        out_ds.put_uid(tags::SOP_INSTANCE_UID, vyna_dcm_core::new_uid());
    }
    Ok(out_ds)
}

/// Transfer syntaxes both decodable and encodable here.
fn is_supported(ts: TransferSyntax) -> bool {
    ts.is_uncompressed()
        || ts == TransferSyntax::RleLossless
        || BundledCodec::SYNTAXES.contains(&ts)
}

/// Decoded image geometry plus color interpretation.
struct Geometry {
    rows: u16,
    columns: u16,
    samples: u16,
    bits: u16,
    frames: usize,
    photometric: Photometric,
    planar: u16,
}

/// Photometric Interpretations the transcoder understands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Photometric {
    Mono1,
    Mono2,
    Rgb,
    YbrFull,
    YbrFull422,
    YbrIct,
    YbrRct,
}

impl Photometric {
    fn is_ybr(&self) -> bool {
        matches!(
            self,
            Photometric::YbrFull
                | Photometric::YbrFull422
                | Photometric::YbrIct
                | Photometric::YbrRct
        )
    }

    fn as_str(&self) -> &'static str {
        match self {
            Photometric::Mono1 => "MONOCHROME1",
            Photometric::Mono2 => "MONOCHROME2",
            Photometric::Rgb => "RGB",
            Photometric::YbrFull => "YBR_FULL",
            Photometric::YbrFull422 => "YBR_FULL_422",
            Photometric::YbrIct => "YBR_ICT",
            Photometric::YbrRct => "YBR_RCT",
        }
    }
}

impl Geometry {
    fn read(ds: &DataSet) -> Result<Self> {
        let req_u16 = |tag: Tag| {
            ds.uint16(tag).ok_or_else(|| Error::BadValue {
                tag,
                vr: Vr::Us,
                message: "missing pixel descriptor".into(),
            })
        };
        let rows = req_u16(tags::ROWS)?;
        let columns = req_u16(tags::COLUMNS)?;
        if rows == 0 || columns == 0 {
            return Err(Error::BadValue {
                tag: tags::ROWS,
                vr: Vr::Us,
                message: "rows and columns must be nonzero".into(),
            });
        }
        let samples = ds.uint16(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
        let bits = req_u16(tags::BITS_ALLOCATED)?;
        let frames = match ds.string(tags::NUMBER_OF_FRAMES) {
            None => 1,
            Some(s) => {
                s.parse::<usize>()
                    .ok()
                    .filter(|&n| n >= 1)
                    .ok_or_else(|| Error::BadValue {
                        tag: tags::NUMBER_OF_FRAMES,
                        vr: Vr::Is,
                        message: format!("bad frame count {s:?}"),
                    })?
            }
        };
        let photo = ds
            .string(tags::PHOTOMETRIC_INTERPRETATION)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::BadValue {
                tag: tags::PHOTOMETRIC_INTERPRETATION,
                vr: Vr::Cs,
                message: "missing Photometric Interpretation".into(),
            })?;
        let photometric = match photo {
            "MONOCHROME1" => Photometric::Mono1,
            "MONOCHROME2" => Photometric::Mono2,
            "RGB" => Photometric::Rgb,
            "YBR_FULL" => Photometric::YbrFull,
            "YBR_FULL_422" => Photometric::YbrFull422,
            "YBR_ICT" => Photometric::YbrIct,
            "YBR_RCT" => Photometric::YbrRct,
            "PALETTE COLOR" => {
                return Err(Error::BadValue {
                    tag: tags::PHOTOMETRIC_INTERPRETATION,
                    vr: Vr::Cs,
                    message: "PALETTE COLOR needs LUT expansion, which transcoding does not do"
                        .into(),
                });
            }
            other => {
                return Err(Error::BadValue {
                    tag: tags::PHOTOMETRIC_INTERPRETATION,
                    vr: Vr::Cs,
                    message: format!("unsupported Photometric Interpretation {other:?}"),
                });
            }
        };
        let mono = matches!(photometric, Photometric::Mono1 | Photometric::Mono2);
        if mono != (samples == 1) {
            return Err(Error::BadValue {
                tag: tags::PHOTOMETRIC_INTERPRETATION,
                vr: Vr::Cs,
                message: format!("{photo} does not match Samples Per Pixel {samples}",),
            });
        }
        let planar = ds.uint16(tags::PLANAR_CONFIGURATION).unwrap_or(0);
        if planar > 1 {
            return Err(Error::BadValue {
                tag: tags::PLANAR_CONFIGURATION,
                vr: Vr::Us,
                message: format!("Planar Configuration {planar} (want 0 or 1)"),
            });
        }
        Ok(Self {
            rows,
            columns,
            samples,
            bits,
            frames,
            photometric,
            planar,
        })
    }

    /// Source-side consistency the decoder alone cannot see.
    fn check_source_photometric(&self, source_ts: TransferSyntax) -> Result<()> {
        if matches!(self.photometric, Photometric::YbrIct | Photometric::YbrRct)
            && !matches!(
                source_ts,
                TransferSyntax::Jpeg2000Lossless | TransferSyntax::Jpeg2000
            )
        {
            return Err(Error::BadValue {
                tag: tags::PHOTOMETRIC_INTERPRETATION,
                vr: Vr::Cs,
                message: format!(
                    "{} is only valid with JPEG 2000 data",
                    self.photometric.as_str()
                ),
            });
        }
        Ok(())
    }

    /// Target-side limits, with errors that name the target.
    fn check_target(&self, target_ts: TransferSyntax) -> Result<()> {
        match target_ts {
            TransferSyntax::JpegBaseline8Bit if self.bits != 8 => Err(Error::BadValue {
                tag: tags::PIXEL_DATA,
                vr: Vr::Ob,
                message: format!(
                    "JPEG Baseline needs 8 bits allocated, dataset has {}",
                    self.bits
                ),
            }),
            TransferSyntax::JpegLossless
            | TransferSyntax::JpegLsLossless
            | TransferSyntax::JpegLsNearLossless
                if self.samples != 1 =>
            {
                Err(Error::BadValue {
                    tag: tags::PIXEL_DATA,
                    vr: Vr::Ob,
                    message: format!("{} is monochrome-only", target_ts.uid()),
                })
            }
            _ => Ok(()),
        }
    }

    /// True when the target needs RGB bytes but the source stores YBR.
    fn needs_ybr_to_rgb(&self, target_ts: TransferSyntax, decoded_rgb: bool) -> bool {
        !decoded_rgb
            && matches!(
                target_ts,
                TransferSyntax::JpegBaseline8Bit
                    | TransferSyntax::Jpeg2000Lossless
                    | TransferSyntax::Jpeg2000
            )
            && matches!(
                self.photometric,
                Photometric::YbrFull | Photometric::YbrFull422
            )
    }

    /// Photometric Interpretation the target bitstream requires.
    fn target_photometric(&self, target_ts: TransferSyntax, decoded_rgb: bool) -> Photometric {
        if self.samples == 1 {
            return self.photometric.clone();
        }
        match target_ts {
            TransferSyntax::JpegBaseline8Bit => Photometric::YbrFull422,
            TransferSyntax::Jpeg2000Lossless => Photometric::YbrRct,
            TransferSyntax::Jpeg2000 => Photometric::YbrIct,
            _ if decoded_rgb => Photometric::Rgb,
            _ => self.photometric.clone(),
        }
    }
}

/// Swap 16-bit sample byte order (big-endian working conversion).
fn swap_pairs(raw: &mut [u8]) {
    for pair in raw.as_chunks_mut::<2>().0 {
        pair.swap(0, 1);
    }
}

/// Color-by-plane (concatenated planes) to color-by-pixel (interleaved).
fn deinterleave_planar(raw: &[u8], geo: &Geometry) -> Vec<u8> {
    let pixels = geo.rows as usize * geo.columns as usize;
    let bps = geo.bits.div_ceil(8) as usize;
    let plane = pixels * bps;
    let mut out = vec![0u8; raw.len()];
    for p in 0..pixels {
        for c in 0..geo.samples as usize {
            let src = c * plane + p * bps;
            let dst = (p * geo.samples as usize + c) * bps;
            out[dst..dst + bps].copy_from_slice(&raw[src..src + bps]);
        }
    }
    out
}

/// BT.601 `YBR_FULL` / `YBR_FULL_422` to interleaved RGB (8-bit only).
fn ybr_to_rgb(raw: &[u8], geo: &Geometry) -> Result<Vec<u8>> {
    if geo.bits != 8 {
        return Err(Error::BadValue {
            tag: tags::PHOTOMETRIC_INTERPRETATION,
            vr: Vr::Cs,
            message: format!(
                "YBR conversion needs 8 bits allocated, dataset has {}",
                geo.bits
            ),
        });
    }
    let pixels = geo.rows as usize * geo.columns as usize;
    let mut out = Vec::with_capacity(pixels * 3);
    match geo.photometric {
        Photometric::YbrFull => {
            for triplet in raw.as_chunks::<3>().0 {
                let [y, cb, cr] = triplet;
                out.extend_from_slice(&ybr_pixel(*y, *cb, *cr));
            }
        }
        Photometric::YbrFull422 => {
            if !geo.columns.is_multiple_of(2) {
                return Err(Error::BadValue {
                    tag: tags::PHOTOMETRIC_INTERPRETATION,
                    vr: Vr::Cs,
                    message: "YBR_FULL_422 needs even columns".into(),
                });
            }
            // DICOM packs each pixel pair as Y1 Y2 Cb Cr.
            for quad in raw.as_chunks::<4>().0 {
                let [y1, y2, cb, cr] = quad;
                out.extend_from_slice(&ybr_pixel(*y1, *cb, *cr));
                out.extend_from_slice(&ybr_pixel(*y2, *cb, *cr));
            }
        }
        _ => unreachable!("caller gates YBR photometrics"),
    }
    Ok(out)
}

/// One BT.601 YCbCr triplet to RGB, clamped.
fn ybr_pixel(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, cb, cr) = (f32::from(y), f32::from(cb) - 128.0, f32::from(cr) - 128.0);
    [
        (y + 1.402 * cr).round().clamp(0.0, 255.0) as u8,
        (y - 0.344_136 * cb - 0.714_136 * cr)
            .round()
            .clamp(0.0, 255.0) as u8,
        (y + 1.772 * cb).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Encoded Pixel Data plus its compressed size (for the lossy ratio).
struct EncodedTarget {
    element: DataElement,
    compressed_len: usize,
}

fn encode_target(
    ds: &DataSet,
    geo: &Geometry,
    raw: &[u8],
    target_ts: TransferSyntax,
    codec: &dyn PixelCodec,
) -> Result<EncodedTarget> {
    if target_ts.is_uncompressed() {
        let mut bytes = raw.to_vec();
        if target_ts == TransferSyntax::ExplicitVrBigEndian && geo.bits > 8 {
            swap_pairs(&mut bytes);
        }
        let vr = if geo.bits <= 8 { Vr::Ob } else { Vr::Ow };
        return Ok(EncodedTarget {
            element: DataElement::bytes(tags::PIXEL_DATA, vr, bytes),
            compressed_len: raw.len(),
        });
    }
    let seq = encode_pixels(ds, target_ts, raw, Some(codec))?;
    let frames: Vec<Vec<u8>> = if geo.frames == 1 {
        vec![seq.fragments.concat()]
    } else {
        // encode_pixels emits one fragment per frame.
        seq.fragments
    };
    let compressed_len: usize = frames.iter().map(Vec::len).sum();
    let fragments = refragment(frames);
    Ok(EncodedTarget {
        element: DataElement::new(tags::PIXEL_DATA, Vr::Ob, Value::Fragments(fragments)),
        compressed_len,
    })
}

/// Re-fragment per-frame bytes: one fragment per frame when everything
/// fits, else ≤64 KiB fragments with a basic offset table (multi
/// fragments per frame without a table would be ambiguous).
fn refragment(frames: Vec<Vec<u8>>) -> vyna_dcm_core::FragmentSequence {
    use vyna_dcm_core::FragmentSequence;
    if frames.iter().all(|f| f.len() <= MAX_FRAGMENT) {
        return FragmentSequence {
            basic_offset_table: vec![],
            fragments: frames,
        };
    }
    let mut offsets = Vec::with_capacity(frames.len());
    let mut fragments = Vec::new();
    let mut pos: u32 = 0;
    for frame in &frames {
        offsets.push(pos);
        for chunk in frame.chunks(MAX_FRAGMENT) {
            pos += chunk.len() as u32;
            fragments.push(chunk.to_vec());
        }
    }
    FragmentSequence {
        basic_offset_table: offsets,
        fragments,
    }
}

/// Lossy bookkeeping: the flag stays `01` once data has ever been
/// lossy; ratio and method follow a lossy target.
fn update_lossy_tags(
    out: &mut DataSet,
    ds: &DataSet,
    source_ts: TransferSyntax,
    target_ts: TransferSyntax,
    raw: &[u8],
    compressed_len: usize,
) {
    let was_flagged = ds
        .string(tags::LOSSY_IMAGE_COMPRESSION)
        .is_some_and(|s| s.trim() == "01");
    let degraded = source_ts.is_lossy() || was_flagged;
    let lossy_target = target_ts.is_lossy();
    out.put_text(
        tags::LOSSY_IMAGE_COMPRESSION,
        Vr::Cs,
        if degraded || lossy_target { "01" } else { "00" },
    );
    if lossy_target {
        let ratio = raw.len() as f64 / compressed_len.max(1) as f64;
        out.put_text(
            tags::LOSSY_IMAGE_COMPRESSION_RATIO,
            Vr::Ds,
            ds_format(ratio),
        );
        let method = match target_ts {
            TransferSyntax::JpegBaseline8Bit => "ISO_10918_1",
            TransferSyntax::JpegLsNearLossless => "ISO_14495_1",
            TransferSyntax::Jpeg2000 => "ISO_15444_1",
            _ => return,
        };
        let mut methods = out
            .strings(tags::LOSSY_IMAGE_COMPRESSION_METHOD)
            .map_or_else(Vec::new, |v| v.to_vec());
        if !methods.iter().any(|m| m == method) {
            methods.push(method.to_string());
        }
        out.insert(DataElement::texts(
            tags::LOSSY_IMAGE_COMPRESSION_METHOD,
            Vr::Cs,
            methods,
        ));
    } else if degraded && !out.contains(tags::LOSSY_IMAGE_COMPRESSION_RATIO) {
        // Lossy history without a ratio (invalid source): keep the flag
        // honest with a vacuous ratio rather than emitting flag-only.
        out.put_text(tags::LOSSY_IMAGE_COMPRESSION_RATIO, Vr::Ds, "1");
    }
}

/// Format an `f64` as a `DS` value (≤ 16 chars, no exponent).
fn ds_format(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let mut s = format!("{v:.3}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s.len() > 16 {
        s.truncate(16);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jpeg::mean_abs_diff;
    use vyna_dcm_core::PersonName;

    const TS_EXPLICIT: TransferSyntax = TransferSyntax::ExplicitVrLittleEndian;

    fn descriptors(
        rows: u16,
        cols: u16,
        spp: u16,
        bits: u16,
        frames: usize,
        photo: &str,
        planar: Option<u16>,
    ) -> DataSet {
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, "1.2.840.10008.5.1.4.1.1.2");
        ds.put_uid(tags::SOP_INSTANCE_UID, "1.2.3.4.5");
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("Test^Pat")]);
        ds.put_u16_single(tags::ROWS, rows);
        ds.put_u16_single(tags::COLUMNS, cols);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, spp);
        ds.put_u16_single(tags::BITS_ALLOCATED, bits);
        ds.put_u16_single(tags::BITS_STORED, bits);
        ds.put_u16_single(tags::HIGH_BIT, bits - 1);
        ds.put_u16_single(tags::PIXEL_REPRESENTATION, 0);
        if frames > 1 {
            ds.put_text(tags::NUMBER_OF_FRAMES, Vr::Is, frames.to_string());
        }
        ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, photo);
        if let Some(p) = planar {
            ds.put_u16_single(tags::PLANAR_CONFIGURATION, p);
        }
        ds
    }

    fn mono8(w: u16, h: u16) -> Vec<u8> {
        (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    (x as u8)
                        .wrapping_mul(7)
                        .wrapping_add(y as u8)
                        .wrapping_mul(3)
                })
            })
            .collect()
    }

    fn mono16(w: u16, h: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(w as usize * h as usize * 2);
        for y in 0..h {
            for x in 0..w {
                let v = (x as u32 * 257 + y as u32 * 511) % 4096;
                out.extend_from_slice(&(v as u16).to_le_bytes());
            }
        }
        out
    }

    fn rgb(w: u16, h: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            for x in 0..w {
                // Smooth gradients (kind to chroma subsampling) plus an edge.
                let r = (x as u32 * 255 / w.max(1) as u32) as u8;
                let g = (y as u32 * 255 / h.max(1) as u32) as u8;
                let b = if x > w / 2 { 200 } else { 40 };
                out.extend_from_slice(&[r, g, b]);
            }
        }
        out
    }

    /// Forward BT.601 RGB→YBR triplet (test-side inverse of `ybr_pixel`).
    fn rgb_to_ybr(r: u8, g: u8, b: u8) -> [u8; 3] {
        let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
        [
            (0.299 * r + 0.587 * g + 0.114 * b)
                .round()
                .clamp(0.0, 255.0) as u8,
            (128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b)
                .round()
                .clamp(0.0, 255.0) as u8,
            (128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b)
                .round()
                .clamp(0.0, 255.0) as u8,
        ]
    }

    fn to_ybr_full422(rgb: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rgb.len() * 2 / 3);
        for pair in rgb.as_chunks::<6>().0 {
            let a = rgb_to_ybr(pair[0], pair[1], pair[2]);
            let b = rgb_to_ybr(pair[3], pair[4], pair[5]);
            let cb = ((u16::from(a[1]) + u16::from(b[1])) / 2) as u8;
            let cr = ((u16::from(a[2]) + u16::from(b[2])) / 2) as u8;
            out.extend_from_slice(&[a[0], b[0], cb, cr]);
        }
        out
    }

    fn to_planar(rgb: &[u8]) -> Vec<u8> {
        let pixels = rgb.len() / 3;
        let mut out = vec![0u8; rgb.len()];
        for (p, triplet) in rgb.as_chunks::<3>().0.iter().enumerate() {
            for c in 0..3 {
                out[c * pixels + p] = triplet[c];
            }
        }
        out
    }

    fn opts() -> TranscodeOptions {
        TranscodeOptions::default()
    }

    fn pixel_bytes(ds: &DataSet) -> Vec<u8> {
        match &ds.get(tags::PIXEL_DATA).unwrap().value {
            Value::Bytes(b) => b.clone(),
            Value::Fragments(f) => f.joined(),
            v => panic!("unexpected pixel shape {v:?}"),
        }
    }

    #[test]
    fn same_syntax_is_noop() {
        let mut ds = descriptors(8, 8, 1, 8, 1, "MONOCHROME2", None);
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, mono8(8, 8));
        let out = transcode_dataset(&ds, TS_EXPLICIT, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(out, ds);
    }

    #[test]
    fn mono8_lossless_matrix_is_exact() {
        use TransferSyntax as Ts;
        for target in [
            Ts::ImplicitVrLittleEndian,
            Ts::DeflatedExplicitVrLittleEndian,
            Ts::RleLossless,
            Ts::JpegLossless,
            Ts::JpegLsLossless,
            Ts::Jpeg2000Lossless,
        ] {
            let raw = mono8(17, 33);
            let mut ds = descriptors(33, 17, 1, 8, 1, "MONOCHROME2", None);
            ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, raw.clone());
            let mid = transcode_dataset(&ds, TS_EXPLICIT, target, &opts()).unwrap();
            assert_eq!(
                mid.string(tags::PHOTOMETRIC_INTERPRETATION),
                Some("MONOCHROME2")
            );
            assert_eq!(mid.string(tags::LOSSY_IMAGE_COMPRESSION), Some("00"));
            assert_eq!(mid.string(tags::SOP_INSTANCE_UID), Some("1.2.3.4.5"));
            let back = transcode_dataset(&mid, target, TS_EXPLICIT, &opts()).unwrap();
            assert_eq!(pixel_bytes(&back), raw, "target {target:?}");
            assert_eq!(back.string(tags::LOSSY_IMAGE_COMPRESSION), Some("00"));
        }
    }

    #[test]
    fn mono16_and_bigendian_roundtrip_exact() {
        use TransferSyntax as Ts;
        for target in [
            Ts::RleLossless,
            Ts::JpegLossless,
            Ts::JpegLsLossless,
            Ts::Jpeg2000Lossless,
        ] {
            let raw = mono16(16, 16);
            let mut ds = descriptors(16, 16, 1, 16, 1, "MONOCHROME2", None);
            ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, raw.clone());
            let mid = transcode_dataset(&ds, TS_EXPLICIT, target, &opts()).unwrap();
            let back = transcode_dataset(&mid, target, TS_EXPLICIT, &opts()).unwrap();
            assert_eq!(pixel_bytes(&back), raw, "target {target:?}");
        }
        // Big-endian swaps both ways.
        let raw = mono16(16, 16);
        let mut be = raw.clone();
        swap_pairs(&mut be);
        let mut ds = descriptors(16, 16, 1, 16, 1, "MONOCHROME2", None);
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, be);
        let le = transcode_dataset(&ds, Ts::ExplicitVrBigEndian, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(pixel_bytes(&le), raw);
        let back = transcode_dataset(&le, TS_EXPLICIT, Ts::ExplicitVrBigEndian, &opts()).unwrap();
        let mut be2 = pixel_bytes(&back);
        swap_pairs(&mut be2);
        assert_eq!(be2, raw);
    }

    #[test]
    fn rgb_planar0_lossless_keeps_layout() {
        let raw = rgb(16, 16);
        let mut ds = descriptors(16, 16, 3, 8, 1, "RGB", Some(0));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, raw.clone());
        let mid =
            transcode_dataset(&ds, TS_EXPLICIT, TransferSyntax::RleLossless, &opts()).unwrap();
        assert_eq!(mid.string(tags::PHOTOMETRIC_INTERPRETATION), Some("RGB"));
        assert_eq!(mid.uint16(tags::PLANAR_CONFIGURATION), Some(0));
        let back =
            transcode_dataset(&mid, TransferSyntax::RleLossless, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(pixel_bytes(&back), raw);
    }

    #[test]
    fn rgb_planar1_denormalizes_to_interleaved() {
        let raw = rgb(16, 16);
        let mut ds = descriptors(16, 16, 3, 8, 1, "RGB", Some(1));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, to_planar(&raw));
        let out =
            transcode_dataset(&ds, TS_EXPLICIT, TransferSyntax::RleLossless, &opts()).unwrap();
        assert_eq!(out.uint16(tags::PLANAR_CONFIGURATION), Some(0));
        let back =
            transcode_dataset(&out, TransferSyntax::RleLossless, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(pixel_bytes(&back), raw);
    }

    #[test]
    fn rgb_to_baseline_flips_photometric_and_uid() {
        use TransferSyntax as Ts;
        let raw = rgb(64, 64);
        let mut ds = descriptors(64, 64, 3, 8, 1, "RGB", Some(0));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, raw.clone());
        let mid = transcode_dataset(&ds, TS_EXPLICIT, Ts::JpegBaseline8Bit, &opts()).unwrap();
        assert_eq!(
            mid.string(tags::PHOTOMETRIC_INTERPRETATION),
            Some("YBR_FULL_422")
        );
        assert_eq!(mid.uint16(tags::PLANAR_CONFIGURATION), Some(0));
        assert_eq!(mid.string(tags::LOSSY_IMAGE_COMPRESSION), Some("01"));
        let ratio: f64 = mid
            .string(tags::LOSSY_IMAGE_COMPRESSION_RATIO)
            .unwrap()
            .parse()
            .unwrap();
        assert!(ratio > 1.0, "ratio {ratio}");
        assert_eq!(
            mid.strings(tags::LOSSY_IMAGE_COMPRESSION_METHOD).unwrap(),
            &["ISO_10918_1".to_string()]
        );
        let uid2 = mid.string(tags::SOP_INSTANCE_UID).unwrap().to_string();
        assert_ne!(uid2, "1.2.3.4.5");
        // Back to native: photo returns to RGB, lossy history persists.
        let back = transcode_dataset(&mid, Ts::JpegBaseline8Bit, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(back.string(tags::PHOTOMETRIC_INTERPRETATION), Some("RGB"));
        assert_eq!(back.string(tags::LOSSY_IMAGE_COMPRESSION), Some("01"));
        assert_eq!(back.string(tags::SOP_INSTANCE_UID), Some(uid2.as_str()));
        let err = mean_abs_diff(&pixel_bytes(&back), &raw);
        assert!(err < 12.0, "mean abs err {err}");
    }

    #[test]
    fn rgb_to_j2k_lossless_is_exact_with_rct_label() {
        use TransferSyntax as Ts;
        let raw = rgb(16, 16);
        let mut ds = descriptors(16, 16, 3, 8, 1, "RGB", Some(0));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, raw.clone());
        let mid = transcode_dataset(&ds, TS_EXPLICIT, Ts::Jpeg2000Lossless, &opts()).unwrap();
        assert_eq!(
            mid.string(tags::PHOTOMETRIC_INTERPRETATION),
            Some("YBR_RCT")
        );
        assert_eq!(mid.string(tags::LOSSY_IMAGE_COMPRESSION), Some("00"));
        assert_eq!(mid.string(tags::SOP_INSTANCE_UID), Some("1.2.3.4.5"));
        let back = transcode_dataset(&mid, Ts::Jpeg2000Lossless, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(back.string(tags::PHOTOMETRIC_INTERPRETATION), Some("RGB"));
        assert_eq!(pixel_bytes(&back), raw);
    }

    #[test]
    fn rgb_to_j2k_lossy_labels_ict() {
        use TransferSyntax as Ts;
        let raw = rgb(64, 64);
        let mut ds = descriptors(64, 64, 3, 8, 1, "RGB", Some(0));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, raw.clone());
        let mut opts = opts();
        opts.codec.jpeg_2000.lossy_ratio = 8.0;
        let mid = transcode_dataset(&ds, TS_EXPLICIT, Ts::Jpeg2000, &opts).unwrap();
        assert_eq!(
            mid.string(tags::PHOTOMETRIC_INTERPRETATION),
            Some("YBR_ICT")
        );
        assert_eq!(mid.string(tags::LOSSY_IMAGE_COMPRESSION), Some("01"));
        assert_ne!(mid.string(tags::SOP_INSTANCE_UID), Some("1.2.3.4.5"));
        let back = transcode_dataset(&mid, Ts::Jpeg2000, TS_EXPLICIT, &opts).unwrap();
        assert_eq!(back.string(tags::PHOTOMETRIC_INTERPRETATION), Some("RGB"));
        let err = mean_abs_diff(&pixel_bytes(&back), &raw);
        assert!(err < 10.0, "mean abs err {err}");
    }

    #[test]
    fn ybr422_native_keeps_bytes_but_converts_for_jpeg() {
        use TransferSyntax as Ts;
        let raw = rgb(16, 16);
        let ybr = to_ybr_full422(&raw);
        let mut ds = descriptors(16, 16, 3, 8, 1, "YBR_FULL_422", Some(0));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, ybr.clone());
        // Native target: passthrough, not even planar-touched.
        let kept =
            transcode_dataset(&ds, TS_EXPLICIT, Ts::ImplicitVrLittleEndian, &opts()).unwrap();
        assert_eq!(
            kept.string(tags::PHOTOMETRIC_INTERPRETATION),
            Some("YBR_FULL_422")
        );
        assert_eq!(pixel_bytes(&kept), ybr);
        // RLE preserves the subsampled bytes too.
        let rle = transcode_dataset(&ds, TS_EXPLICIT, Ts::RleLossless, &opts()).unwrap();
        assert_eq!(
            rle.string(tags::PHOTOMETRIC_INTERPRETATION),
            Some("YBR_FULL_422")
        );
        let unrle = transcode_dataset(&rle, Ts::RleLossless, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(pixel_bytes(&unrle), ybr);
        // JPEG target: upsampled to RGB first, ends RGB on the way back.
        let mid = transcode_dataset(&ds, TS_EXPLICIT, Ts::JpegBaseline8Bit, &opts()).unwrap();
        assert_eq!(
            mid.string(tags::PHOTOMETRIC_INTERPRETATION),
            Some("YBR_FULL_422")
        );
        let back = transcode_dataset(&mid, Ts::JpegBaseline8Bit, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(back.string(tags::PHOTOMETRIC_INTERPRETATION), Some("RGB"));
        let err = mean_abs_diff(&pixel_bytes(&back), &raw);
        assert!(err < 14.0, "mean abs err {err}");
    }

    #[test]
    fn ybr_full_to_lossless_j2k_matches_direct_conversion() {
        use TransferSyntax as Ts;
        let raw = rgb(16, 16);
        let ybr: Vec<u8> = raw
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|t| rgb_to_ybr(t[0], t[1], t[2]))
            .collect();
        let mut ds = descriptors(16, 16, 3, 8, 1, "YBR_FULL", Some(0));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, ybr);
        let mid = transcode_dataset(&ds, TS_EXPLICIT, Ts::Jpeg2000Lossless, &opts()).unwrap();
        let back = transcode_dataset(&mid, Ts::Jpeg2000Lossless, TS_EXPLICIT, &opts()).unwrap();
        // Lossless path: final RGB equals the direct conversion exactly.
        let geo = Geometry::read(&ds).unwrap();
        let expect = ybr_to_rgb(&pixel_bytes(&ds), &geo).unwrap();
        assert_eq!(pixel_bytes(&back), expect);
    }

    #[test]
    fn lossy_source_keeps_history_through_lossless_target() {
        use TransferSyntax as Ts;
        let raw = rgb(16, 16);
        let mut ds = descriptors(16, 16, 3, 8, 1, "RGB", Some(0));
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, raw.clone());
        let lossy = transcode_dataset(&ds, TS_EXPLICIT, Ts::JpegBaseline8Bit, &opts()).unwrap();
        let uid2 = lossy.string(tags::SOP_INSTANCE_UID).unwrap().to_string();
        let back = transcode_dataset(&lossy, Ts::JpegBaseline8Bit, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(back.string(tags::LOSSY_IMAGE_COMPRESSION), Some("01"));
        assert!(back.string(tags::LOSSY_IMAGE_COMPRESSION_RATIO).is_some());
        assert_eq!(back.string(tags::SOP_INSTANCE_UID), Some(uid2.as_str()));
        assert_eq!(pixel_bytes(&back).len(), raw.len());
    }

    #[test]
    fn multiframe_mono_rle_roundtrip_is_exact() {
        use TransferSyntax as Ts;
        let mut raw = mono8(16, 16);
        raw.extend(mono8(16, 16).iter().map(|b| 255 - b));
        let mut ds = descriptors(16, 16, 1, 8, 2, "MONOCHROME2", None);
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, raw.clone());
        let mid = transcode_dataset(&ds, TS_EXPLICIT, Ts::RleLossless, &opts()).unwrap();
        let back = transcode_dataset(&mid, Ts::RleLossless, TS_EXPLICIT, &opts()).unwrap();
        assert_eq!(pixel_bytes(&back), raw);
    }

    #[test]
    fn gray_ybr_converts_exactly() {
        let geo = Geometry {
            rows: 4,
            columns: 4,
            samples: 3,
            bits: 8,
            frames: 1,
            photometric: Photometric::YbrFull,
            planar: 0,
        };
        let ybr: Vec<u8> = (0u8..16).flat_map(|y| [y * 16, 128, 128]).collect();
        let rgb = ybr_to_rgb(&ybr, &geo).unwrap();
        for (i, triplet) in rgb.as_chunks::<3>().0.iter().enumerate() {
            assert_eq!(*triplet, [(i as u8) * 16; 3]);
        }
        // 422 packs pairs; odd columns are rejected.
        let geo422 = Geometry {
            photometric: Photometric::YbrFull422,
            ..geo_clone(&geo)
        };
        let ybr422: Vec<u8> = (0u8..8)
            .flat_map(|i| [i * 16, i * 16 + 8, 128, 128])
            .collect();
        let rgb422 = ybr_to_rgb(&ybr422, &geo422).unwrap();
        assert_eq!(rgb422.len(), 16 * 3);
        let odd = Geometry {
            columns: 3,
            photometric: Photometric::YbrFull422,
            ..geo_clone(&geo)
        };
        assert!(ybr_to_rgb(&ybr422, &odd).is_err());
    }

    fn geo_clone(geo: &Geometry) -> Geometry {
        Geometry {
            rows: geo.rows,
            columns: geo.columns,
            samples: geo.samples,
            bits: geo.bits,
            frames: geo.frames,
            photometric: geo.photometric.clone(),
            planar: geo.planar,
        }
    }

    #[test]
    fn refragment_rules() {
        // Small frames: one fragment each, no table.
        let seq = refragment(vec![vec![1u8; 100], vec![2u8; 200]]);
        assert!(seq.basic_offset_table.is_empty());
        assert_eq!(seq.fragments.len(), 2);
        // A large frame forces splits plus offsets.
        let big = vec![7u8; MAX_FRAGMENT + 1000];
        let seq = refragment(vec![vec![1u8; 10], big.clone()]);
        assert_eq!(seq.basic_offset_table, vec![0, 10]);
        assert!(seq.fragments.iter().all(|f| f.len() <= MAX_FRAGMENT));
        assert_eq!(seq.fragments.concat().len(), 10 + big.len());
    }

    #[test]
    fn bad_inputs_rejected() {
        use TransferSyntax as Ts;
        let mut ds = descriptors(8, 8, 1, 8, 1, "MONOCHROME2", None);
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, mono8(8, 8));
        // Unsupported target.
        assert!(transcode_dataset(&ds, TS_EXPLICIT, Ts::JpegExtended12Bit, &opts()).is_err());
        // Color to mono-only targets.
        let mut rgb_ds = descriptors(8, 8, 3, 8, 1, "RGB", Some(0));
        rgb_ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, rgb(8, 8));
        for target in [Ts::JpegLossless, Ts::JpegLsLossless, Ts::JpegLsNearLossless] {
            assert!(
                transcode_dataset(&rgb_ds, TS_EXPLICIT, target, &opts()).is_err(),
                "{target:?}"
            );
        }
        // 16-bit to baseline.
        let mut ds16 = descriptors(8, 8, 1, 16, 1, "MONOCHROME2", None);
        ds16.put_bytes(tags::PIXEL_DATA, Vr::Ow, mono16(8, 8));
        assert!(transcode_dataset(&ds16, TS_EXPLICIT, Ts::JpegBaseline8Bit, &opts()).is_err());
        // Palette and partial photometrics.
        for photo in ["PALETTE COLOR", "YBR_PARTIAL_422", "WEIRD"] {
            let mut bad = descriptors(8, 8, 1, 8, 1, "MONOCHROME2", None);
            bad.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, photo);
            bad.put_bytes(tags::PIXEL_DATA, Vr::Ow, mono8(8, 8));
            assert!(
                transcode_dataset(&bad, TS_EXPLICIT, Ts::RleLossless, &opts()).is_err(),
                "{photo}"
            );
        }
        // Samples/photometric mismatch.
        let mut bad = descriptors(8, 8, 1, 8, 1, "MONOCHROME2", None);
        bad.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "RGB");
        bad.put_bytes(tags::PIXEL_DATA, Vr::Ow, mono8(8, 8));
        assert!(transcode_dataset(&bad, TS_EXPLICIT, Ts::RleLossless, &opts()).is_err());
        // Missing photometric, zero rows, bad frame count, bad planar.
        let mut bad = descriptors(8, 8, 1, 8, 1, "MONOCHROME2", None);
        bad.remove(tags::PHOTOMETRIC_INTERPRETATION);
        bad.put_bytes(tags::PIXEL_DATA, Vr::Ow, mono8(8, 8));
        assert!(transcode_dataset(&bad, TS_EXPLICIT, Ts::RleLossless, &opts()).is_err());
        let mut bad = descriptors(8, 8, 1, 8, 1, "MONOCHROME2", None);
        bad.put_text(tags::NUMBER_OF_FRAMES, Vr::Is, "bogus");
        bad.put_bytes(tags::PIXEL_DATA, Vr::Ow, mono8(8, 8));
        assert!(transcode_dataset(&bad, TS_EXPLICIT, Ts::RleLossless, &opts()).is_err());
        let mut bad = descriptors(8, 8, 3, 8, 1, "RGB", Some(2));
        bad.put_bytes(tags::PIXEL_DATA, Vr::Ob, rgb(8, 8));
        assert!(transcode_dataset(&bad, TS_EXPLICIT, Ts::RleLossless, &opts()).is_err());
        // YBR_ICT outside JPEG 2000.
        let mut bad = descriptors(8, 8, 3, 8, 1, "YBR_ICT", Some(0));
        bad.put_bytes(tags::PIXEL_DATA, Vr::Ob, rgb(8, 8));
        assert!(transcode_dataset(&bad, TS_EXPLICIT, Ts::RleLossless, &opts()).is_err());
    }

    #[test]
    fn ds_format_trims() {
        assert_eq!(ds_format(8.5), "8.5");
        assert_eq!(ds_format(1.0), "1");
        assert_eq!(ds_format(0.0), "0");
    }
}
