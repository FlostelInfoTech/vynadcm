//! JPEG 2000 pixel codec (Transfer Syntaxes `.90` lossless, `.91` lossy).
//!
//! Backed by `vyna-openjp2` (our `openjp2` fork, a pure-Rust OpenJPEG
//! port): raw codestreams with
//! mono or 3-component frames, precision 1–16 bits. Encoding always writes
//! unsigned components; decoding honours each component's own signedness, so
//! signed lossless data (e.g. CT) round-trips as bit patterns.
//!
//! Memory I/O goes through the fork's stream callbacks (`MemStream`); the
//! `unsafe` there is confined to that wrapper (see its safety notes).

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

use vyna_dcm_core::{FragmentSequence, TransferSyntax};
use vyna_openjp2::openjpeg::{
    OPJ_BOOL, OPJ_OFF_T, OPJ_SIZE_T, opj_stream_default_create, opj_stream_destroy,
    opj_stream_set_read_function, opj_stream_set_seek_function, opj_stream_set_skip_function,
    opj_stream_set_user_data, opj_stream_set_user_data_length, opj_stream_set_write_function,
};
use vyna_openjp2::{
    CODEC_FORMAT, COLOR_SPACE, Codec, Stream, opj_cparameters_t, opj_dparameters_t, opj_image,
    opj_image_comptparm, opj_stream_t,
};

use crate::{PixelCodec, PixelCodecError};

/// Pixel codec for JPEG 2000 Lossless (`.90`) and lossy (`.91`).
///
/// `lossy_ratio` is the target compression ratio used when *encoding* `.91`
/// frames (e.g. 10.0 ≈ 10:1); it is ignored for `.90`.
#[derive(Clone, Copy, Debug)]
pub struct Jpeg2000Codec {
    /// Target compression ratio for `.91` encodes. Default 10.0.
    pub lossy_ratio: f32,
}

impl Default for Jpeg2000Codec {
    fn default() -> Self {
        Self { lossy_ratio: 10.0 }
    }
}

impl PixelCodec for Jpeg2000Codec {
    fn syntaxes(&self) -> &[TransferSyntax] {
        &[TransferSyntax::Jpeg2000Lossless, TransferSyntax::Jpeg2000]
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
        if samples_per_pixel != 1 && samples_per_pixel != 3 {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg 2000 needs 1 or 3 samples per pixel, got {samples_per_pixel}"
            )));
        }
        if !(1..=16).contains(&bits_allocated) {
            return Err(PixelCodecError::Geometry(format!(
                "bits allocated {bits_allocated} outside the 1..=16 range"
            )));
        }
        let mut codec = Codec::new_decoder(CODEC_FORMAT::OPJ_CODEC_J2K)
            .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 decoder creation failed".into()))?;
        let mut params = opj_dparameters_t::default();
        if codec.setup_decoder(&mut params) == 0 {
            return Err(PixelCodecError::Codec(
                "jpeg 2000 decoder setup failed".into(),
            ));
        }
        let mut stream = MemStream::reader(fragments.joined())?;
        // SAFETY: `stream` owns its boxed `Stream`; the borrow is exclusive
        // and never outlives it (see `MemStream`).
        let raw = unsafe { stream.stream() };
        let mut image = codec
            .read_header(raw)
            .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 header unreadable".into()))?;
        // Pre-validate the SIZ grid before decode(): a tiny frame
        // claiming huge dimensions must fail fast instead of
        // triggering SIZ-sized tile allocations.
        check_component_grid(&image, rows, columns, samples_per_pixel)?;
        if codec.decode(raw, &mut image) == 0 {
            return Err(PixelCodecError::Codec("jpeg 2000 decode failed".into()));
        }
        if codec.end_decompress(raw) == 0 {
            return Err(PixelCodecError::Codec(
                "jpeg 2000 end-of-decompress failed".into(),
            ));
        }
        pack_image(&image, rows, columns, samples_per_pixel, bits_allocated)
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
        let lossless = ts == TransferSyntax::Jpeg2000Lossless;
        if !lossless && !(self.lossy_ratio.is_finite() && self.lossy_ratio >= 1.0) {
            return Err(PixelCodecError::Geometry(format!(
                "lossy ratio must be finite and >= 1.0, got {}",
                self.lossy_ratio
            )));
        }
        let mut image = build_image(raw, rows, columns, samples_per_pixel, bits_allocated)?;
        let mut codec = Codec::new_encoder(CODEC_FORMAT::OPJ_CODEC_J2K)
            .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 encoder creation failed".into()))?;
        let mut params =
            encoder_params(rows, columns, samples_per_pixel, lossless, self.lossy_ratio);
        if codec.setup_encoder(&mut params, &mut image) == 0 {
            return Err(PixelCodecError::Codec(
                "jpeg 2000 encoder setup failed".into(),
            ));
        }
        let out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let mut stream = MemStream::writer(Rc::clone(&out))?;
        // SAFETY: as in `decode_frame`.
        let raw_stream = unsafe { stream.stream() };
        if codec.start_compress(&mut image, raw_stream) == 0 {
            return Err(PixelCodecError::Codec(
                "jpeg 2000 start-compress failed".into(),
            ));
        }
        if codec.encode(raw_stream) == 0 {
            return Err(PixelCodecError::Codec("jpeg 2000 encode failed".into()));
        }
        if codec.end_compress(raw_stream) == 0 {
            return Err(PixelCodecError::Codec(
                "jpeg 2000 end-compress failed".into(),
            ));
        }
        raw_stream
            .flush()
            .map_err(|e| PixelCodecError::Codec(format!("jpeg 2000 stream flush failed: {e}")))?;
        drop(stream);
        let bytes = out.borrow().clone();
        if bytes.is_empty() {
            return Err(PixelCodecError::Codec(
                "jpeg 2000 encoder produced no output".into(),
            ));
        }
        Ok(FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![bytes],
        })
    }
}

/// Encoder parameters mirroring `opj_compress` defaults, sized to the frame.
fn encoder_params(
    rows: u16,
    columns: u16,
    samples: u16,
    lossless: bool,
    lossy_ratio: f32,
) -> opj_cparameters_t {
    let min_dim = rows.min(columns).max(1) as u32;
    // Resolutions must satisfy 2^(n-1) <= min dimension.
    let mut levels = 1i32;
    while levels < 6 && (1u32 << levels) <= min_dim {
        levels += 1;
    }
    let cblock = if min_dim >= 64 { 64 } else { 4 };
    // Single tile spanning the frame, stated explicitly: the port's
    // non-tiling path leaves the tile grid empty and fails validation.
    let mut params = vyna_openjp2::opj_cparameters {
        tcp_numlayers: 1,
        tile_size_on: 1,
        cp_tdx: columns as i32,
        cp_tdy: rows as i32,
        cp_disto_alloc: 1,
        irreversible: if lossless { 0 } else { 1 },
        numresolution: levels,
        cblockw_init: cblock,
        cblockh_init: cblock,
        tcp_mct: if samples == 3 { 1 } else { 0 },
        ..Default::default()
    };
    params.tcp_rates[0] = if lossless { 0.0 } else { lossy_ratio };
    params
}

/// Build an encoder image from interleaved raw samples (unsigned).
fn build_image(
    raw: &[u8],
    rows: u16,
    columns: u16,
    samples: u16,
    bits: u16,
) -> std::result::Result<Box<opj_image>, PixelCodecError> {
    if samples != 1 && samples != 3 {
        return Err(PixelCodecError::Geometry(format!(
            "jpeg 2000 needs 1 or 3 samples per pixel, got {samples}"
        )));
    }
    if !(1..=16).contains(&bits) {
        return Err(PixelCodecError::Geometry(format!(
            "bits allocated {bits} outside the 1..=16 range"
        )));
    }
    let bytes_per_sample = bits.div_ceil(8) as usize;
    // Checked: the u16 product can overflow 32-bit usize (and would
    // panic in debug builds).
    let expected = (rows as usize)
        .checked_mul(columns as usize)
        .and_then(|n| n.checked_mul(samples as usize))
        .and_then(|n| n.checked_mul(bytes_per_sample))
        .ok_or_else(|| {
            PixelCodecError::Geometry(format!(
                "geometry {rows}x{columns}x{samples} overflows address space"
            ))
        })?;
    if raw.len() != expected {
        return Err(PixelCodecError::Geometry(format!(
            "raw frame is {} bytes, expected {expected}",
            raw.len(),
        )));
    }
    // Exact: `expected` is `count * samples * bytes_per_sample`.
    let count = expected / (samples as usize * bytes_per_sample);
    let parms = vec![
        opj_image_comptparm {
            dx: 1,
            dy: 1,
            w: columns.into(),
            h: rows.into(),
            x0: 0,
            y0: 0,
            prec: bits.into(),
            bpp: bits.into(),
            sgnd: 0,
        };
        samples as usize
    ];
    let clrspc = if samples == 3 {
        COLOR_SPACE::OPJ_CLRSPC_SRGB
    } else {
        COLOR_SPACE::OPJ_CLRSPC_GRAY
    };
    let mut image = opj_image::create(&parms, clrspc)
        .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 image creation failed".into()))?;
    // `create` leaves the reference grid at zero; the caller must set it.
    image.x0 = 0;
    image.y0 = 0;
    image.x1 = columns.into();
    image.y1 = rows.into();
    let comps = image
        .comps_mut()
        .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 image has no components".into()))?;
    for (c, comp) in comps.iter_mut().enumerate() {
        let mut plane = vec![0i32; count];
        for (i, slot) in plane.iter_mut().enumerate() {
            let base = (i * samples as usize + c) * bytes_per_sample;
            *slot = if bytes_per_sample == 1 {
                raw[base] as i32
            } else {
                u16::from_le_bytes([raw[base], raw[base + 1]]) as i32
            };
        }
        if !comp.set_data(&plane) {
            return Err(PixelCodecError::Codec(
                "jpeg 2000 component data rejected".into(),
            ));
        }
    }
    Ok(image)
}

/// Validate the SIZ component grid against the dataset geometry.
///
/// Runs twice: right after `read_header` (so a lying SIZ fails before
/// any tile allocation) and again in [`pack_image`] (defense in depth
/// after decode). Reads header fields only — never component data.
fn check_component_grid(
    image: &opj_image,
    rows: u16,
    columns: u16,
    samples: u16,
) -> std::result::Result<(), PixelCodecError> {
    let comps = image
        .comps()
        .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 image has no components".into()))?;
    if comps.len() != samples as usize {
        return Err(PixelCodecError::Geometry(format!(
            "jpeg 2000 frame has {} component(s), dataset says {samples}",
            comps.len()
        )));
    }
    for comp in comps {
        if comp.w != columns as u32 || comp.h != rows as u32 {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg 2000 component is {}x{}, dataset says {columns}x{rows}",
                comp.w, comp.h
            )));
        }
        if comp.prec < 1 || comp.prec > 16 {
            return Err(PixelCodecError::Codec(format!(
                "jpeg 2000 precision {} outside the 1..=16 range",
                comp.prec
            )));
        }
        if comp.dx != 1 || comp.dy != 1 {
            return Err(PixelCodecError::Codec(
                "subsampled jpeg 2000 components are not supported".into(),
            ));
        }
    }
    Ok(())
}

/// Validate a decoded image against the dataset geometry and pack it.
fn pack_image(
    image: &opj_image,
    rows: u16,
    columns: u16,
    samples: u16,
    bits: u16,
) -> std::result::Result<Vec<u8>, PixelCodecError> {
    check_component_grid(image, rows, columns, samples)?;
    let comps = image
        .comps()
        .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 image has no components".into()))?;
    // Checked: the u16 product can overflow 32-bit usize (and would
    // panic in debug builds).
    let count = (rows as usize)
        .checked_mul(columns as usize)
        .ok_or_else(|| {
            PixelCodecError::Geometry(format!("geometry {rows}x{columns} overflows address space"))
        })?;
    let mut planes: Vec<Vec<u16>> = Vec::with_capacity(comps.len());
    for comp in comps {
        let data = comp
            .data()
            .ok_or_else(|| PixelCodecError::Codec("jpeg 2000 component has no data".into()))?;
        if data.len() != count {
            return Err(PixelCodecError::Geometry(format!(
                "jpeg 2000 component holds {} samples, expected {count}",
                data.len()
            )));
        }
        planes.push(pack_plane(data, comp.prec, comp.sgnd != 0, bits)?);
    }
    // Interleave planes; mono is already in order.
    if samples == 1 {
        return samples_to_packed(&planes[0], bits);
    }
    let capacity = count
        .checked_mul(samples as usize)
        .and_then(|n| n.checked_mul(bits.div_ceil(8) as usize))
        .ok_or_else(|| {
            PixelCodecError::Geometry(format!(
                "geometry {rows}x{columns}x{samples} overflows address space"
            ))
        })?;
    let mut out = Vec::with_capacity(capacity);
    for i in 0..count {
        for plane in &planes {
            push_sample(&mut out, plane[i], bits);
        }
    }
    Ok(out)
}

/// Pack one component plane to u16 samples, honouring precision/signedness.
fn pack_plane(
    data: &[i32],
    prec: u32,
    signed: bool,
    bits: u16,
) -> std::result::Result<Vec<u16>, PixelCodecError> {
    let mut out = Vec::with_capacity(data.len());
    for &v in data {
        out.push(pack_sample(v, prec, signed, bits)?);
    }
    Ok(out)
}

/// Normalize one decoded sample to the dataset bit depth.
///
/// Lossy overshoot is clamped into the component range; a component range
/// that cannot fit the dataset depth is a geometry error.
fn pack_sample(
    v: i32,
    prec: u32,
    signed: bool,
    bits: u16,
) -> std::result::Result<u16, PixelCodecError> {
    let value: i32 = if signed {
        let half = 1i32 << (prec - 1);
        v.clamp(-half, half - 1)
    } else {
        v.clamp(0, (1i32 << prec) - 1)
    };
    if signed {
        if !signed_fits(value, bits) {
            return Err(PixelCodecError::Geometry(format!(
                "signed sample {value} does not fit in {bits} bits"
            )));
        }
    } else if value > 0xFFFF || (bits <= 8 && value > 0xFF) {
        return Err(PixelCodecError::Geometry(format!(
            "sample {value} does not fit in {bits} bits"
        )));
    }
    Ok(value as u16)
}

/// Whether a signed value fits the dataset depth (two's complement).
fn signed_fits(value: i32, bits: u16) -> bool {
    if bits >= 16 {
        return (-32768..=32767).contains(&value);
    }
    if bits <= 8 {
        return (-128..=127).contains(&value);
    }
    let half = 1i32 << (bits - 1);
    (-half..half).contains(&value)
}

fn samples_to_packed(samples: &[u16], bits: u16) -> std::result::Result<Vec<u8>, PixelCodecError> {
    let capacity = samples
        .len()
        .checked_mul(bits.div_ceil(8) as usize)
        .ok_or_else(|| {
            PixelCodecError::Geometry(format!("{} samples overflows address space", samples.len()))
        })?;
    let mut out = Vec::with_capacity(capacity);
    for &s in samples {
        push_sample(&mut out, s, bits);
    }
    Ok(out)
}

fn push_sample(out: &mut Vec<u8>, sample: u16, bits: u16) {
    if bits <= 8 {
        out.push(sample as u8);
    } else {
        out.extend_from_slice(&sample.to_le_bytes());
    }
}

/// Callback state for [`MemStream`]: input bytes for decode, shared output
/// for encode, and the cursor both directions move.
struct MemState {
    data: Vec<u8>,
    out: Rc<RefCell<Vec<u8>>>,
    pos: usize,
    /// Reader seeks clamp to `data.len()`; the writer (empty `data`)
    /// must not be clamped, so the callbacks need the direction.
    writable: bool,
}

/// RAII handle over a raw fork memory stream.
///
/// The port exposes memory I/O only through its C-style stream callbacks, so
/// this wrapper owns both the `Box<Stream>` (created by
/// `opj_stream_default_create`, freed by `opj_stream_destroy` on drop) and
/// the `Box<MemState>` the callbacks dereference (freed by `mem_free` when
/// the stream drops).
struct MemStream {
    raw: *mut opj_stream_t,
}

impl MemStream {
    fn reader(data: Vec<u8>) -> std::result::Result<Self, PixelCodecError> {
        let len = data.len();
        let state = Box::new(MemState {
            data,
            out: Rc::new(RefCell::new(Vec::new())),
            pos: 0,
            writable: false,
        });
        // SAFETY: `opj_stream_default_create` returns an owned boxed `Stream`
        // (or null on allocation failure); the callbacks only dereference
        // the `MemState` box installed as user data, which the stream frees.
        unsafe {
            let raw = opj_stream_default_create(1);
            if raw.is_null() {
                return Err(PixelCodecError::Codec(
                    "jpeg 2000 stream creation failed".into(),
                ));
            }
            opj_stream_set_read_function(raw, Some(mem_read));
            opj_stream_set_skip_function(raw, Some(mem_skip));
            opj_stream_set_seek_function(raw, Some(mem_seek));
            opj_stream_set_user_data_length(raw, len as u64);
            opj_stream_set_user_data(raw, Box::into_raw(state).cast::<c_void>(), Some(mem_free));
            Ok(Self { raw })
        }
    }

    fn writer(out: Rc<RefCell<Vec<u8>>>) -> std::result::Result<Self, PixelCodecError> {
        let state = Box::new(MemState {
            data: Vec::new(),
            out,
            pos: 0,
            writable: true,
        });
        // SAFETY: as in `reader`.
        unsafe {
            let raw = opj_stream_default_create(0);
            if raw.is_null() {
                return Err(PixelCodecError::Codec(
                    "jpeg 2000 stream creation failed".into(),
                ));
            }
            opj_stream_set_write_function(raw, Some(mem_write));
            opj_stream_set_skip_function(raw, Some(mem_skip));
            opj_stream_set_seek_function(raw, Some(mem_seek));
            opj_stream_set_user_data(raw, Box::into_raw(state).cast::<c_void>(), Some(mem_free));
            Ok(Self { raw })
        }
    }

    /// Borrow the owned boxed `Stream` for the safe [`Codec`] methods.
    ///
    /// # Safety
    ///
    /// The pointer always refers to the live `Box<Stream>` created in
    /// [`MemStream::reader`] / [`MemStream::writer`]; callers must not use the
    /// borrow after `drop` and must hold no second `&mut` while it is live.
    unsafe fn stream(&mut self) -> &mut Stream {
        unsafe { &mut *(self.raw as *mut Stream) }
    }
}

impl Drop for MemStream {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: `raw` is an owned boxed `Stream`; the stream's own
            // drop runs `mem_free` on the user-data box.
            unsafe { opj_stream_destroy(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

// The callbacks below run single-threaded inside the codec calls; each
// dereferences exactly the `MemState` box installed as user data.

unsafe extern "C" fn mem_free(data: *mut c_void) {
    if !data.is_null() {
        drop(unsafe { Box::from_raw(data.cast::<MemState>()) });
    }
}

unsafe extern "C" fn mem_read(buf: *mut c_void, nb: OPJ_SIZE_T, data: *mut c_void) -> OPJ_SIZE_T {
    if buf.is_null() || data.is_null() {
        return OPJ_SIZE_T::MAX;
    }
    // A zero-length read is a no-op success: the stream layer casts
    // the return to isize (negative fails), so the failure sentinel
    // here would kill a decodable stream.
    if nb == 0 {
        return 0;
    }
    let state = unsafe { &mut *data.cast::<MemState>() };
    let remaining = state.data.len().saturating_sub(state.pos);
    if remaining == 0 {
        return OPJ_SIZE_T::MAX;
    }
    let n = remaining.min(nb);
    let out = unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), n) };
    out.copy_from_slice(&state.data[state.pos..state.pos + n]);
    state.pos += n;
    n
}

unsafe extern "C" fn mem_write(buf: *mut c_void, nb: OPJ_SIZE_T, data: *mut c_void) -> OPJ_SIZE_T {
    if buf.is_null() || data.is_null() {
        return OPJ_SIZE_T::MAX;
    }
    let state = unsafe { &mut *data.cast::<MemState>() };
    let src = unsafe { std::slice::from_raw_parts(buf as *const u8, nb) };
    let mut out = state.out.borrow_mut();
    if state.pos > out.len() {
        // The encoder writes sequentially and backpatches lengths; a
        // forward jump past EOF would zero-fill an unbounded gap, so a
        // wild seek fails the encode instead of ballooning memory.
        if state.pos - out.len() > 1024 * 1024 {
            return OPJ_SIZE_T::MAX;
        }
        out.resize(state.pos, 0);
    }
    let Some(end) = state.pos.checked_add(nb) else {
        return OPJ_SIZE_T::MAX;
    };
    if end > out.len() {
        out.resize(end, 0);
    }
    out[state.pos..end].copy_from_slice(src);
    state.pos = end;
    nb
}

unsafe extern "C" fn mem_skip(nb: OPJ_OFF_T, data: *mut c_void) -> OPJ_OFF_T {
    if data.is_null() {
        return -1;
    }
    let state = unsafe { &mut *data.cast::<MemState>() };
    if nb >= 0 {
        // `try_from`: a bare `as usize` truncates past 4 GiB on 32-bit.
        let step = usize::try_from(nb).unwrap_or(usize::MAX);
        state.pos = state.pos.saturating_add(step);
        if !state.writable {
            state.pos = state.pos.min(state.data.len());
        }
    }
    // `pos` never exceeds a `Vec` length (`<= isize::MAX`), so the
    // narrowing to `i64` is exact on every platform.
    state.pos as OPJ_OFF_T
}

unsafe extern "C" fn mem_seek(nb: OPJ_OFF_T, data: *mut c_void) -> OPJ_BOOL {
    if data.is_null() || nb < 0 {
        return 0;
    }
    let Ok(off) = usize::try_from(nb) else {
        return 0;
    };
    let state = unsafe { &mut *data.cast::<MemState>() };
    state.pos = if state.writable {
        off
    } else {
        // Reads past EOF already fail in `mem_read`; clamping keeps
        // `pos` a valid index instead of a wrapped/truncated one.
        off.min(state.data.len())
    };
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jpeg::mean_abs_diff;

    #[test]
    fn mem_read_zero_length_is_success() {
        let mut st = MemState {
            data: vec![1u8; 8],
            out: Rc::new(RefCell::new(Vec::new())),
            pos: 0,
            writable: false,
        };
        let p = (&mut st as *mut MemState).cast::<c_void>();
        let mut buf = [0u8; 8];
        unsafe {
            // Zero-length reads succeed (0): the stream layer casts
            // the return to isize, where negative means failure.
            assert_eq!(mem_read(buf.as_mut_ptr().cast::<c_void>(), 0, p), 0);
            assert_eq!(st.pos, 0);
            // Null pointers still fail loudly.
            assert_eq!(mem_read(std::ptr::null_mut(), 4, p), OPJ_SIZE_T::MAX);
            assert_eq!(
                mem_read(buf.as_mut_ptr().cast::<c_void>(), 4, std::ptr::null_mut()),
                OPJ_SIZE_T::MAX
            );
            // Normal reads still work.
            assert_eq!(mem_read(buf.as_mut_ptr().cast::<c_void>(), 4, p), 4);
            assert_eq!(st.pos, 4);
        }
    }

    #[test]
    fn stream_callbacks_clamp_and_cap() {
        // Reader: seeks/skips past EOF clamp to the input length.
        let mut st = MemState {
            data: vec![0u8; 16],
            out: Rc::new(RefCell::new(Vec::new())),
            pos: 0,
            writable: false,
        };
        let p = (&mut st as *mut MemState).cast::<c_void>();
        unsafe {
            assert_eq!(mem_seek(100, p), 1);
            assert_eq!(st.pos, 16);
            st.pos = 0;
            assert_eq!(mem_skip(100, p), 16);
            assert_eq!(st.pos, 16);
            assert_eq!(mem_seek(-1, p), 0);
            assert_eq!(st.pos, 16);
            // Negative skips leave the position alone.
            assert_eq!(mem_skip(-5, p), 16);
        }
        // Writer: a wild forward jump fails the write instead of
        // zero-filling gigabytes; a small gap still works.
        let out = Rc::new(RefCell::new(Vec::new()));
        let mut wst = MemState {
            data: Vec::new(),
            out: out.clone(),
            pos: 0,
            writable: true,
        };
        let wp = (&mut wst as *mut MemState).cast::<c_void>();
        let buf = [7u8; 4];
        unsafe {
            assert_eq!(mem_seek(i64::MAX, wp), 1);
            assert_eq!(
                mem_write(buf.as_ptr() as *mut c_void, 4, wp),
                OPJ_SIZE_T::MAX
            );
            assert!(out.borrow().is_empty());
            wst.pos = 0;
            assert_eq!(mem_seek(8, wp), 1);
            assert_eq!(mem_write(buf.as_ptr() as *mut c_void, 4, wp), 4);
            assert_eq!(out.borrow().len(), 12);
        }
    }

    fn pattern(w: u16, h: u16, samples: u16, bits: u16) -> Vec<u8> {
        let mut out = Vec::new();
        let max = if bits <= 8 { 255u32 } else { 65535u32 };
        for y in 0..h {
            for x in 0..w {
                for s in 0..samples {
                    let v =
                        (x as u32 * 131 + y as u32 * 197 + s as u32 * 7919 + ((x * y) as u32 * 13))
                            % (max + 1);
                    if bits <= 8 {
                        out.push(v as u8);
                    } else {
                        out.extend_from_slice(&(v as u16).to_le_bytes());
                    }
                }
            }
        }
        out
    }

    fn roundtrip(
        raw: &[u8],
        rows: u16,
        cols: u16,
        samples: u16,
        bits: u16,
        ts: TransferSyntax,
    ) -> Vec<u8> {
        let codec = Jpeg2000Codec::default();
        let frags = codec
            .encode_frame(ts, raw, rows, cols, samples, bits)
            .unwrap();
        // Raw codestream magic: SOC followed by SIZ.
        assert_eq!(&frags.joined()[..4], &[0xFF, 0x4F, 0xFF, 0x51]);
        codec
            .decode_frame(ts, &frags, rows, cols, samples, bits)
            .unwrap()
    }

    #[test]
    fn lossless_roundtrips_exactly() {
        let raw = pattern(33, 17, 1, 16);
        assert_eq!(
            roundtrip(&raw, 17, 33, 1, 16, TransferSyntax::Jpeg2000Lossless),
            raw
        );
        let raw = pattern(16, 16, 1, 8);
        assert_eq!(
            roundtrip(&raw, 16, 16, 1, 8, TransferSyntax::Jpeg2000Lossless),
            raw
        );
        let raw = pattern(24, 20, 3, 8);
        assert_eq!(
            roundtrip(&raw, 20, 24, 3, 8, TransferSyntax::Jpeg2000Lossless),
            raw
        );
        // Signed bit patterns survive lossless coding.
        let signed: Vec<u8> = [0x00, 0x80, 0xFF, 0x7F, 0x34, 0x12, 0xCB, 0xED]
            .into_iter()
            .cycle()
            .take(12 * 12 * 2)
            .collect();
        assert_eq!(
            roundtrip(&signed, 12, 12, 1, 16, TransferSyntax::Jpeg2000Lossless),
            signed
        );
    }

    #[test]
    fn flat_frames_roundtrip() {
        // Flat regions produce tag-tree-skipped code blocks — the input
        // class that aborted the unpatched decoder on drop.
        let flat16 = vec![0xABu8; 48 * 32 * 2];
        assert_eq!(
            roundtrip(&flat16, 32, 48, 1, 16, TransferSyntax::Jpeg2000Lossless),
            flat16
        );
        let flat_rgb = vec![0x80u8; 40 * 28 * 3];
        assert_eq!(
            roundtrip(&flat_rgb, 28, 40, 3, 8, TransferSyntax::Jpeg2000Lossless),
            flat_rgb
        );
    }

    #[test]
    fn lossy_roundtrip_is_close() {
        // Smooth, image-like gradient with one hard edge: fair lossy fodder
        // (white noise at 8:1 would legitimately score a huge error).
        let mut raw = Vec::with_capacity(64 * 64);
        for y in 0..64u16 {
            for x in 0..64u16 {
                let v = (x as u32 * 2 + y as u32 * 3) % 200;
                let v = if x > 40 { v + 55 } else { v };
                raw.push(v as u8);
            }
        }
        let codec = Jpeg2000Codec { lossy_ratio: 8.0 };
        let frags = codec
            .encode_frame(TransferSyntax::Jpeg2000, &raw, 64, 64, 1, 8)
            .unwrap();
        let back = codec
            .decode_frame(TransferSyntax::Jpeg2000, &frags, 64, 64, 1, 8)
            .unwrap();
        let err = mean_abs_diff(&raw, &back);
        assert!(err < 6.0, "mean abs err {err} too high at ratio 8");
        assert!(frags.joined().len() < raw.len());
    }

    #[test]
    fn cross_checks_against_independent_j2k() {
        // pure_jpeg2k is an independent implementation: streams must decode
        // identically in both directions.
        use jpeg2k::Jpeg2kOptions;
        let (rows, cols) = (32u16, 48u16);
        let raw = pattern(cols, rows, 1, 16);
        let samples: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();

        // Independent encode -> our decode.
        let mut foreign = Vec::new();
        jpeg2k::encode(
            &samples,
            cols.into(),
            rows.into(),
            &Jpeg2kOptions::default(),
            &mut foreign,
        )
        .unwrap();
        let codec = Jpeg2000Codec::default();
        let frags = FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![foreign],
        };
        let back = codec
            .decode_frame(TransferSyntax::Jpeg2000Lossless, &frags, rows, cols, 1, 16)
            .unwrap();
        assert_eq!(back, raw);

        // Our encode -> independent decode.
        let ours = codec
            .encode_frame(TransferSyntax::Jpeg2000Lossless, &raw, rows, cols, 1, 16)
            .unwrap();
        let (pixels, w, h) = jpeg2k::decode(&ours.joined(), cols.into(), rows.into()).unwrap();
        assert_eq!((w, h), (cols as u32, rows as u32));
        assert_eq!(pixels, samples);
    }

    #[test]
    fn truncated_input_errors_without_panic() {
        let codec = Jpeg2000Codec::default();
        let raw = pattern(32, 32, 1, 16);
        let frags = codec
            .encode_frame(TransferSyntax::Jpeg2000Lossless, &raw, 32, 32, 1, 16)
            .unwrap();
        for end in [16, frags.joined().len() / 2] {
            let cut = FragmentSequence {
                basic_offset_table: vec![],
                fragments: vec![frags.joined()[..end].to_vec()],
            };
            let err = codec
                .decode_frame(TransferSyntax::Jpeg2000Lossless, &cut, 32, 32, 1, 16)
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    PixelCodecError::Codec(_) | PixelCodecError::Geometry(_)
                ),
                "{err:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_input() {
        let codec = Jpeg2000Codec::default();
        // Not a codestream.
        let garbage = FragmentSequence {
            basic_offset_table: vec![],
            fragments: vec![vec![0xFF, 0xD8, 0xFF, 0xD9]],
        };
        let err = codec
            .decode_frame(TransferSyntax::Jpeg2000Lossless, &garbage, 8, 8, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Codec(_)), "{err:?}");
        // Bad lossy ratio.
        let bad = Jpeg2000Codec { lossy_ratio: 0.5 };
        let err = bad
            .encode_frame(TransferSyntax::Jpeg2000, &[0u8; 64], 8, 8, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
        // Part 2 stays unsupported.
        let err = codec
            .decode_frame(TransferSyntax::Jpeg2000Part2, &garbage, 8, 8, 1, 8)
            .unwrap_err();
        assert!(matches!(err, PixelCodecError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn lying_siz_fails_before_tile_alloc() {
        let codec = Jpeg2000Codec::default();
        let raw = pattern(4, 4, 1, 8);
        let frags = codec
            .encode_frame(TransferSyntax::Jpeg2000Lossless, &raw, 4, 4, 1, 8)
            .unwrap();
        let mut lying = frags.joined();
        // SOC + SIZ, then Lsiz/Rsiz; Xsiz/Ysiz are BE u32 at [8..16].
        assert_eq!(&lying[..4], &[0xFF, 0x4F, 0xFF, 0x51]);
        let mut decode_patched = |w: u32, h: u32| {
            lying[8..12].copy_from_slice(&w.to_be_bytes());
            lying[12..16].copy_from_slice(&h.to_be_bytes());
            let cut = FragmentSequence {
                basic_offset_table: vec![],
                fragments: vec![lying.clone()],
            };
            codec.decode_frame(TransferSyntax::Jpeg2000Lossless, &cut, 4, 4, 1, 8)
        };
        // A sane-but-wrong SIZ parses, then the grid pre-check rejects
        // it before decode() can act on the claimed grid.
        let err = decode_patched(8, 8).unwrap_err();
        assert!(matches!(err, PixelCodecError::Geometry(_)), "{err:?}");
        // A 10^10-sample SIZ claim: the port's own header validation
        // rejects it during read_header, so no tile alloc is attempted
        // at any layer (either layer failing fast is acceptable).
        let err = decode_patched(100_000, 100_000).unwrap_err();
        assert!(
            matches!(
                err,
                PixelCodecError::Geometry(_) | PixelCodecError::Codec(_)
            ),
            "{err:?}"
        );
    }

    #[test]
    fn sample_packing_edges() {
        // Unsigned clamp + fit.
        assert_eq!(pack_sample(300, 9, false, 16).unwrap(), 300);
        assert_eq!(pack_sample(-5, 8, false, 8).unwrap(), 0);
        assert_eq!(pack_sample(999, 8, false, 8).unwrap(), 255);
        assert!(pack_sample(300, 9, false, 8).is_err());
        // Signed two's complement.
        assert_eq!(pack_sample(-1, 16, true, 16).unwrap(), 0xFFFF);
        assert_eq!(pack_sample(-128, 8, true, 8).unwrap(), 0xFF80);
        assert!(pack_sample(-129, 9, true, 8).is_err());
        // Overshoot clamps into range first: 200 at signed prec-8 is 127.
        assert_eq!(pack_sample(200, 8, true, 8).unwrap(), 127);
        // ...but a 9-bit 200 genuinely cannot fit 8-bit signed.
        assert!(pack_sample(200, 9, true, 8).is_err());
        assert_eq!(pack_sample(200, 9, true, 16).unwrap(), 200);
    }
}
