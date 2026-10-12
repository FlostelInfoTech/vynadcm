//! Single-frame PNG export/import for debugging (dicom-rs
//! `toimage`/`fromimage`-like).
//!
//! - [`frame_to_png`] renders one 1-based frame to PNG bytes.
//!   Supported: `MONOCHROME1/2` at 8/16 bits, interleaved or planar
//!   `RGB` at 8 bits. `MONOCHROME1` is inverted; signed 16-bit
//!   samples are offset by 2^15. No windowing: display stays out of
//!   scope, and YBR, palette, and sub-byte depths are refused.
//! - [`inject_png`] replaces Pixel Data from PNG bytes (`L8`/`L16`/
//!   `RGB8`), rewriting the geometry tags. UIDs are kept: this is
//!   test/debug injection, and the operator owns instance identity.

use image::{DynamicImage, ImageFormat};
use vyna_dcm_core::{DataElement, DataSet, TransferSyntax, Value, Vr, tags};

use crate::transcode::{Geometry, Photometric, deinterleave_planar};
use crate::{BundledCodec, Error, Result, decode_pixels};

/// Render one 1-based frame of `ds` (stored in `src`) to PNG bytes.
pub fn frame_to_png(ds: &DataSet, src: TransferSyntax, frame: usize) -> Result<Vec<u8>> {
    let geo = Geometry::read(ds)?;
    if geo.photometric.is_ybr() {
        return Err(Error::Image(format!(
            "YBR output needs color conversion ({}); transcode to a native syntax first",
            geo.photometric.as_str()
        )));
    }
    match (&geo.photometric, geo.bits) {
        (Photometric::Mono1 | Photometric::Mono2, 8 | 16) => {}
        (Photometric::Rgb, 8) if geo.samples == 3 => {}
        _ => {
            return Err(Error::Image(format!(
                "need MONOCHROME 8/16-bit or RGB 8-bit, have {} {}-bit ({} samples)",
                geo.photometric.as_str(),
                geo.bits,
                geo.samples
            )));
        }
    }
    if frame < 1 || frame > geo.frames {
        return Err(Error::BadValue {
            tag: tags::NUMBER_OF_FRAMES,
            vr: Vr::Is,
            message: format!("frame {frame} of {}", geo.frames),
        });
    }
    let mut raw = decode_pixels(ds, src)?;
    // JPEG decoders return interleaved RGB by construction.
    if geo.planar == 1 && !BundledCodec::SYNTAXES.contains(&src) {
        raw = deinterleave_planar(&raw, &geo);
    }
    let frame_len = raw.len() / geo.frames;
    let bytes = &raw[(frame - 1) * frame_len..frame * frame_len];

    let signed = ds.uint16(tags::PIXEL_REPRESENTATION).unwrap_or(0) == 1;
    let w = u32::from(geo.columns);
    let h = u32::from(geo.rows);
    let invert = geo.photometric == Photometric::Mono1;
    let image = match (&geo.photometric, geo.bits) {
        (Photometric::Rgb, _) => DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(w, h, bytes.to_vec())
                .ok_or_else(|| Error::Image("frame size mismatch".into()))?,
        ),
        (_, 8) => {
            let mut px = bytes.to_vec();
            if invert {
                px.iter_mut().for_each(|v| *v = 255 - *v);
            }
            DynamicImage::ImageLuma8(
                image::GrayImage::from_raw(w, h, px)
                    .ok_or_else(|| Error::Image("frame size mismatch".into()))?,
            )
        }
        (_, 16) => {
            let px: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .map(|v| {
                    let v = if signed { v.wrapping_add(1 << 15) } else { v };
                    if invert { 0xFFFF - v } else { v }
                })
                .collect();
            DynamicImage::ImageLuma16(
                image::ImageBuffer::from_raw(w, h, px)
                    .ok_or_else(|| Error::Image("frame size mismatch".into()))?,
            )
        }
        _ => unreachable!("checked above"),
    };
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| Error::Image(e.to_string()))?;
    Ok(png)
}

/// Replace Pixel Data from PNG bytes, rewriting the geometry tags.
///
/// Accepts 8/16-bit gray and 8-bit RGB PNGs; the dataset becomes
/// single-frame native (`MONOCHROME2`/`RGB`, unsigned). UIDs are
/// kept (see module docs).
pub fn inject_png(ds: &mut DataSet, png: &[u8]) -> Result<()> {
    let img = image::load_from_memory(png).map_err(|e| Error::Image(e.to_string()))?;
    let (w, h) = (img.width(), img.height());
    let (photo, samples, bits, bytes) = match img {
        DynamicImage::ImageLuma8(g) => ("MONOCHROME2", 1, 8, g.into_raw()),
        DynamicImage::ImageLuma16(g) => (
            "MONOCHROME2",
            1,
            16,
            g.into_raw().iter().flat_map(|v| v.to_le_bytes()).collect(),
        ),
        DynamicImage::ImageRgb8(g) => ("RGB", 3, 8, g.into_raw()),
        other => {
            return Err(Error::Image(format!(
                "need 8/16-bit gray or 8-bit RGB PNG, have {:?}",
                other.color()
            )));
        }
    };
    let (columns, rows) = (
        u16::try_from(w).map_err(|_| Error::Image(format!("width {w} exceeds 16 bits")))?,
        u16::try_from(h).map_err(|_| Error::Image(format!("height {h} exceeds 16 bits")))?,
    );
    ds.put_u16_single(tags::ROWS, rows);
    ds.put_u16_single(tags::COLUMNS, columns);
    ds.put_u16_single(tags::SAMPLES_PER_PIXEL, samples);
    ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, photo);
    ds.put_u16_single(tags::BITS_ALLOCATED, bits);
    ds.put_u16_single(tags::BITS_STORED, bits);
    ds.put_u16_single(tags::HIGH_BIT, bits - 1);
    ds.put_u16_single(tags::PIXEL_REPRESENTATION, 0);
    if samples == 3 {
        ds.put_u16_single(tags::PLANAR_CONFIGURATION, 0);
    } else {
        ds.remove(tags::PLANAR_CONFIGURATION);
    }
    ds.remove(tags::NUMBER_OF_FRAMES);
    let vr = if bits <= 8 { Vr::Ob } else { Vr::Ow };
    ds.insert(DataElement::new(tags::PIXEL_DATA, vr, Value::Bytes(bytes)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono16_signed() -> DataSet {
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::ROWS, 2);
        ds.put_u16_single(tags::COLUMNS, 2);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 1);
        ds.put_u16_single(tags::BITS_ALLOCATED, 16);
        ds.put_u16_single(tags::PIXEL_REPRESENTATION, 1);
        ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "MONOCHROME2");
        let px: Vec<u8> = [-2i16, -1, 0, 1]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        ds.insert(DataElement::bytes(tags::PIXEL_DATA, Vr::Ow, px));
        ds
    }

    #[test]
    fn png_round_trip_mono16_signed() {
        let ds = mono16_signed();
        let png = frame_to_png(&ds, TransferSyntax::ExplicitVrLittleEndian, 1).unwrap();
        // Signed samples offset by 2^15 for presentation.
        let img = image::load_from_memory(&png).unwrap();
        let gray16 = img.into_luma16();
        assert_eq!(gray16.as_raw(), &[32766, 32767, 32768, 32769]);
        // Injection restores the presentation bytes, normalized to unsigned.
        let mut back = DataSet::new();
        inject_png(&mut back, &png).unwrap();
        let expect: Vec<u8> = [32766u16, 32767, 32768, 32769]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(back.bytes(tags::PIXEL_DATA).unwrap(), expect);
        assert_eq!(back.uint16(tags::ROWS), Some(2));
        assert_eq!(back.uint16(tags::BITS_STORED), Some(16));
        assert_eq!(back.uint16(tags::PIXEL_REPRESENTATION), Some(0));
        assert_eq!(back.vr_of(tags::PIXEL_DATA), Some(Vr::Ow));
    }

    #[test]
    fn mono1_inverts_and_planar_rgb_renders() {
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::ROWS, 1);
        ds.put_u16_single(tags::COLUMNS, 2);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 1);
        ds.put_u16_single(tags::BITS_ALLOCATED, 8);
        ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "MONOCHROME1");
        ds.insert(DataElement::bytes(tags::PIXEL_DATA, Vr::Ob, vec![0, 255]));
        let png = frame_to_png(&ds, TransferSyntax::ExplicitVrLittleEndian, 1).unwrap();
        let gray8 = image::load_from_memory(&png).unwrap().into_luma8();
        assert_eq!(gray8.as_raw(), &[255, 0]);

        // Planar RGB (R..R G..G B..B) deinterleaves to RGBRGB.
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::ROWS, 1);
        ds.put_u16_single(tags::COLUMNS, 2);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 3);
        ds.put_u16_single(tags::BITS_ALLOCATED, 8);
        ds.put_u16_single(tags::PLANAR_CONFIGURATION, 1);
        ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "RGB");
        ds.insert(DataElement::bytes(
            tags::PIXEL_DATA,
            Vr::Ob,
            vec![10, 20, 30, 40, 50, 60],
        ));
        let png = frame_to_png(&ds, TransferSyntax::ExplicitVrLittleEndian, 1).unwrap();
        let rgb = image::load_from_memory(&png).unwrap().into_rgb8();
        assert_eq!(rgb.as_raw(), &[10, 30, 50, 20, 40, 60]);
    }

    fn gray8_png(px: &[u8], w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, px.to_vec()).unwrap());
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn injected_png_survives_lossless_targets() {
        use crate::transcode::{TranscodeOptions, transcode_dataset};
        let png = gray8_png(&[0, 64, 128, 255], 2, 2);
        let mut native = DataSet::new();
        inject_png(&mut native, &png).unwrap();
        let expect = decode_pixels(&native, TransferSyntax::ExplicitVrLittleEndian).unwrap();
        let opts = TranscodeOptions::default();
        for target in [
            TransferSyntax::JpegLossless,
            TransferSyntax::JpegLsLossless,
            TransferSyntax::Jpeg2000Lossless,
            TransferSyntax::RleLossless,
        ] {
            let enc = transcode_dataset(
                &native,
                TransferSyntax::ExplicitVrLittleEndian,
                target,
                &opts,
            )
            .unwrap();
            assert_eq!(decode_pixels(&enc, target).unwrap(), expect, "{target:?}");
        }
    }

    #[test]
    fn rejects_unsupported() {
        // YBR output is refused (transcode first).
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::ROWS, 1);
        ds.put_u16_single(tags::COLUMNS, 1);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 3);
        ds.put_u16_single(tags::BITS_ALLOCATED, 8);
        ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL_422");
        ds.insert(DataElement::bytes(tags::PIXEL_DATA, Vr::Ob, vec![0, 128]));
        assert!(frame_to_png(&ds, TransferSyntax::ExplicitVrLittleEndian, 1).is_err());
        // Palette color needs LUT expansion: refused.
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::ROWS, 1);
        ds.put_u16_single(tags::COLUMNS, 1);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 1);
        ds.put_u16_single(tags::BITS_ALLOCATED, 8);
        ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "PALETTE COLOR");
        ds.insert(DataElement::bytes(tags::PIXEL_DATA, Vr::Ob, vec![0]));
        assert!(frame_to_png(&ds, TransferSyntax::ExplicitVrLittleEndian, 1).is_err());
        // Frame numbers are 1-based and bounded.
        let ds = mono16_signed();
        let ts = TransferSyntax::ExplicitVrLittleEndian;
        assert!(frame_to_png(&ds, ts, 0).is_err());
        assert!(frame_to_png(&ds, ts, 2).is_err());
        // RGBA PNGs and garbage are refused on injection.
        let rgba = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([1, 2, 3, 4]),
        ));
        let mut buf = Vec::new();
        rgba.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        let mut ds = DataSet::new();
        assert!(inject_png(&mut ds, &buf).is_err());
        assert!(inject_png(&mut ds, b"not a png").is_err());
    }
}
