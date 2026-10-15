//! Pixel redaction: paint constant-filled rectangles over native frames.
//!
//! [`redact_regions`] mutates Pixel Data in place (burned-in annotation
//! removal for de-identification). Only native (uncompressed) syntaxes
//! with 8/16-bit samples are supported: encapsulated data, packed bit
//! depths, and palette color are refused — silently redacting less
//! than requested would leak PHI, so every mismatch is an error and
//! regions are never clamped to fit.

use vyna_dcm_core::{DataSet, TransferSyntax, Value, Vr, tags};

use crate::error::{Error, Result};
use crate::transcode::{Geometry, Photometric};

/// One rectangular redaction area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedactRegion {
    /// Left column (0-based).
    pub x: u32,
    /// Top row (0-based).
    pub y: u32,
    /// Width in pixels (must be nonzero).
    pub width: u32,
    /// Height in pixels (must be nonzero).
    pub height: u32,
    /// Which frames the rectangle applies to.
    pub frames: RedactFrames,
}

/// Frame selector for [`RedactRegion`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedactFrames {
    /// Every frame in the pixel data.
    All,
    /// One 1-based frame number.
    One(u32),
}

/// Paint `regions` over the native pixel data of `ds` (`ts` is the
/// dataset's transfer syntax), in place.
///
/// Fill destroys the covered samples: zero bytes, except
/// `MONOCHROME1` where black is the maximum sample value. No-op when
/// `regions` is empty.
pub fn redact_regions(
    ds: &mut DataSet,
    ts: TransferSyntax,
    regions: &[RedactRegion],
) -> Result<()> {
    if regions.is_empty() {
        return Ok(());
    }
    if !ts.is_uncompressed() {
        return Err(Error::UnsupportedTransferSyntax(ts.uid().to_string()));
    }
    let geo = Geometry::read(ds)?;
    if geo.bits != 8 && geo.bits != 16 {
        return Err(Error::BadValue {
            tag: tags::BITS_ALLOCATED,
            vr: Vr::Us,
            message: format!("redaction needs 8/16-bit samples, have {}-bit", geo.bits),
        });
    }
    if geo.samples != 1 && geo.samples != 3 {
        return Err(Error::BadValue {
            tag: tags::SAMPLES_PER_PIXEL,
            vr: Vr::Us,
            message: format!(
                "redaction needs 1 or 3 samples per pixel, have {}",
                geo.samples
            ),
        });
    }
    let pixel = ds.get(tags::PIXEL_DATA).ok_or_else(|| Error::BadValue {
        tag: tags::PIXEL_DATA,
        vr: Vr::Ow,
        message: "redaction needs native Pixel Data".into(),
    })?;
    let vr = pixel.vr;
    let Value::Bytes(bytes) = &pixel.value else {
        return Err(Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr,
            message: "redaction needs native single-fragment Pixel Data".into(),
        });
    };
    let bytes = bytes.clone();
    let bps = u64::from(geo.bits / 8);
    let spp = u64::from(geo.samples);
    let expected = u64::from(geo.rows)
        .checked_mul(u64::from(geo.columns))
        .and_then(|n| n.checked_mul(spp))
        .and_then(|n| n.checked_mul(bps))
        .and_then(|n| n.checked_mul(geo.frames as u64))
        .filter(|&n| n == bytes.len() as u64)
        .ok_or_else(|| Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr,
            message: format!(
                "pixel byte length {} disagrees with frame geometry",
                bytes.len()
            ),
        })?;
    debug_assert!(expected > 0);
    for region in regions {
        check_region(region, &geo)?;
    }
    let mut bytes = bytes;
    let fill = if geo.photometric == Photometric::Mono1 {
        0xFF
    } else {
        0x00
    };
    let frame_len = bytes.len() / geo.frames;
    let stride = geo.columns as u64 * spp * bps;
    for region in regions {
        let frames: Vec<usize> = match region.frames {
            RedactFrames::All => (0..geo.frames).collect(),
            RedactFrames::One(f) => vec![f as usize - 1],
        };
        for f in frames {
            paint_region(
                &mut bytes,
                &geo,
                region,
                f * frame_len,
                stride as usize,
                bps as usize,
                spp as usize,
                fill,
            );
        }
    }
    ds.put_bytes(tags::PIXEL_DATA, vr, bytes);
    Ok(())
}

/// Validate one region against the geometry: nonzero size, inside the
/// frame, and (for [`RedactFrames::One`]) a live frame number. Bounds
/// use `u64` so `u32::MAX` coordinates fail instead of overflowing.
fn check_region(region: &RedactRegion, geo: &Geometry) -> Result<()> {
    if region.width == 0 || region.height == 0 {
        return Err(Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr: Vr::Ow,
            message: "redaction region must have nonzero width and height".into(),
        });
    }
    if region.x as u64 + region.width as u64 > geo.columns as u64
        || region.y as u64 + region.height as u64 > geo.rows as u64
    {
        return Err(Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr: Vr::Ow,
            message: format!(
                "redaction region {}x{} at {},{} exceeds {}x{} frames",
                region.width, region.height, region.x, region.y, geo.columns, geo.rows
            ),
        });
    }
    if let RedactFrames::One(f) = region.frames
        && (f == 0 || f as u64 > geo.frames as u64)
    {
        return Err(Error::BadValue {
            tag: tags::PIXEL_DATA,
            vr: Vr::Us,
            message: format!("redaction frame {f} outside 1..={}", geo.frames),
        });
    }
    Ok(())
}

/// Paint one validated region on one frame: every index below is
/// inside `bytes` by [`check_region`] plus the length check in
/// [`redact_regions`] (planar-1 writes stay inside their plane by the
/// same row/column bounds).
#[allow(clippy::too_many_arguments)]
fn paint_region(
    bytes: &mut [u8],
    geo: &Geometry,
    region: &RedactRegion,
    frame_off: usize,
    stride: usize,
    bps: usize,
    spp: usize,
    fill: u8,
) {
    let (x, y, w, h) = (
        region.x as usize,
        region.y as usize,
        region.width as usize,
        region.height as usize,
    );
    if geo.planar == 0 {
        for r in y..y + h {
            let off = frame_off + r * stride + x * spp * bps;
            bytes[off..off + w * spp * bps].fill(fill);
        }
    } else {
        let plane_len = geo.rows as usize * geo.columns as usize * bps;
        for p in 0..spp {
            let poff = frame_off + p * plane_len;
            for r in y..y + h {
                let off = poff + r * geo.columns as usize * bps + x * bps;
                bytes[off..off + w * bps].fill(fill);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::TransferSyntax as Ts;

    fn native_ds(
        rows: u16,
        cols: u16,
        samples: u16,
        bits: u16,
        frames: u16,
        photo: &str,
        planar: Option<u16>,
    ) -> DataSet {
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::ROWS, rows);
        ds.put_u16_single(tags::COLUMNS, cols);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, samples);
        ds.put_u16_single(tags::BITS_ALLOCATED, bits);
        ds.put_u16_single(tags::BITS_STORED, bits);
        ds.put_u16_single(tags::HIGH_BIT, bits - 1);
        ds.put_text(tags::PHOTOMETRIC_INTERPRETATION, Vr::Cs, photo);
        if let Some(p) = planar {
            ds.put_u16_single(tags::PLANAR_CONFIGURATION, p);
        }
        if frames > 1 {
            ds.put_text(tags::NUMBER_OF_FRAMES, Vr::Is, frames.to_string());
        }
        let bps = bits as usize / 8;
        let len = rows as usize * cols as usize * samples as usize * bps * frames as usize;
        // Nonzero pattern: redaction to zero is observable everywhere.
        let px: Vec<u8> = (0..len).map(|i| (i % 251 + 1) as u8).collect();
        let vr = if bits == 8 { Vr::Ob } else { Vr::Ow };
        ds.put_bytes(tags::PIXEL_DATA, vr, px);
        ds
    }

    fn pixel(ds: &DataSet) -> Vec<u8> {
        match &ds.get(tags::PIXEL_DATA).unwrap().value {
            Value::Bytes(b) => b.clone(),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    fn region(x: u32, y: u32, w: u32, h: u32) -> RedactRegion {
        RedactRegion {
            x,
            y,
            width: w,
            height: h,
            frames: RedactFrames::All,
        }
    }

    #[test]
    fn mono8_all_frames_redacts_inside_only() {
        let mut ds = native_ds(8, 8, 1, 8, 2, "MONOCHROME2", None);
        redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[region(2, 3, 4, 2)]).unwrap();
        let px = pixel(&ds);
        for f in 0..2 {
            for r in 0..8 {
                for c in 0..8 {
                    let v = px[f * 64 + r * 8 + c];
                    let inside = (2..6).contains(&c) && (3..5).contains(&r);
                    assert_eq!(v == 0, inside, "f={f} r={r} c={c}");
                }
            }
        }
    }

    #[test]
    fn rgb_planar_layouts_redact_all_components() {
        for planar in [0, 1] {
            let mut ds = native_ds(4, 6, 3, 8, 1, "RGB", Some(planar));
            redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[region(1, 1, 2, 2)]).unwrap();
            let px = pixel(&ds);
            for r in 0..4 {
                for c in 0..6 {
                    let inside = (1..3).contains(&c) && (1..3).contains(&r);
                    for s in 0..3 {
                        let off = if planar == 0 {
                            (r * 6 + c) * 3 + s
                        } else {
                            s * 24 + r * 6 + c
                        };
                        assert_eq!(px[off] == 0, inside, "planar={planar} r={r} c={c} s={s}");
                    }
                }
            }
        }
    }

    #[test]
    fn mono1_fills_max_and_single_frame_scopes() {
        let mut ds = native_ds(4, 4, 1, 8, 3, "MONOCHROME1", None);
        redact_regions(
            &mut ds,
            Ts::ExplicitVrLittleEndian,
            &[RedactRegion {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
                frames: RedactFrames::One(2),
            }],
        )
        .unwrap();
        let px = pixel(&ds);
        for f in 0..3 {
            let all_max = px[f * 16..(f + 1) * 16].iter().all(|&v| v == 0xFF);
            assert_eq!(all_max, f == 1, "frame {f}");
        }
    }

    #[test]
    fn mono16_fills_whole_samples() {
        let mut ds = native_ds(4, 4, 1, 16, 1, "MONOCHROME2", None);
        redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[region(1, 1, 2, 2)]).unwrap();
        let px = pixel(&ds);
        for r in 0..4 {
            for c in 0..4 {
                let off = (r * 4 + c) * 2;
                let word = u16::from_le_bytes([px[off], px[off + 1]]);
                let inside = (1..3).contains(&c) && (1..3).contains(&r);
                assert_eq!(word == 0, inside, "r={r} c={c}");
            }
        }
    }

    #[test]
    fn bad_regions_geometries_and_syntaxes_fail_closed() {
        // Out-of-bounds rectangles (never clamped), zero sizes, bad frames.
        for bad in [
            region(7, 0, 2, 2),
            region(0, 7, 2, 2),
            region(0, 0, 0, 2),
            region(0, 0, 2, 0),
            region(u32::MAX, 0, 1, 1),
        ] {
            let mut ds = native_ds(8, 8, 1, 8, 1, "MONOCHROME2", None);
            assert!(redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[bad]).is_err());
        }
        for frame in [0, 2, 99] {
            let mut ds = native_ds(8, 8, 1, 8, 1, "MONOCHROME2", None);
            let r = RedactRegion {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                frames: RedactFrames::One(frame),
            };
            assert!(redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[r]).is_err());
        }
        // Encapsulated syntax: no silent transcode-and-redact.
        let mut ds = native_ds(8, 8, 1, 8, 1, "MONOCHROME2", None);
        let err = redact_regions(&mut ds, Ts::Jpeg2000Lossless, &[region(0, 0, 1, 1)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("1.2.840.10008.1.2.4.90"), "{err}");
        // Packed bit depth.
        let mut ds = native_ds(8, 8, 1, 8, 1, "MONOCHROME2", None);
        ds.put_u16_single(tags::BITS_ALLOCATED, 12);
        assert!(
            redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[region(0, 0, 1, 1)]).is_err()
        );
        // Truncated pixel data.
        let mut ds = native_ds(8, 8, 1, 8, 1, "MONOCHROME2", None);
        let mut px = pixel(&ds);
        px.truncate(10);
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ob, px);
        assert!(
            redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[region(0, 0, 1, 1)]).is_err()
        );
        // Missing pixel data.
        let mut ds = DataSet::new();
        assert!(
            redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[region(0, 0, 1, 1)]).is_err()
        );
        // Empty region list is a no-op.
        let mut ds = native_ds(8, 8, 1, 8, 1, "MONOCHROME2", None);
        let before = pixel(&ds);
        redact_regions(&mut ds, Ts::ExplicitVrLittleEndian, &[]).unwrap();
        assert_eq!(pixel(&ds), before);
    }
}
