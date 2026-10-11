//! Dataset decoding for all uncompressed transfer syntaxes.
//!
//! Handles Explicit/Implicit VR Little Endian, Explicit VR Big Endian
//! (retired) and Deflated Explicit VR Little Endian, including defined and
//! undefined length sequences, encapsulated fragments, and the
//! context-dependent VR rules for pixel/waveform attributes (PS3.5 §6.2,
//! §7.5, §8, §10).

use flate2::bufread::ZlibDecoder;
use std::io::Read;
use vyna_dcm_core::{
    DataElement, DataSet, FragmentSequence, PersonName, Tag, TransferSyntax, Value, Vr, delimiter,
    dictionary_vr, is_context_dependent_vr, tags,
};

use crate::{Error, Result};

/// Undefined length marker (`0xFFFFFFFF`).
pub const UNDEFINED_LENGTH: u32 = 0xFFFF_FFFF;
/// Maximum sequence nesting depth (guards against malicious input).
const MAX_DEPTH: usize = 64;

/// Decode a whole dataset buffer in `ts`.
pub fn decode_dataset(bytes: &[u8], ts: TransferSyntax) -> Result<DataSet> {
    if ts.is_deflated() {
        let mut dec = ZlibDecoder::new(bytes);
        let mut inflated = Vec::new();
        dec.read_to_end(&mut inflated)
            .map_err(|e| Error::Deflate(e.to_string()))?;
        return decode_dataset(&inflated, TransferSyntax::ExplicitVrLittleEndian);
    }
    let (ds, used) = decode_up_to(bytes, ts, None)?;
    if used != bytes.len() {
        return Err(Error::Truncated {
            offset: used,
            what: "trailing bytes after dataset",
        });
    }
    Ok(ds)
}

/// Decode elements until `stop_before` (exclusive), returning the dataset
/// and the bytes consumed. Used for file meta (`stop_before` group `0x0002`
/// end) and DIMSE command sets.
pub(crate) fn decode_up_to(
    bytes: &[u8],
    ts: TransferSyntax,
    stop_before: Option<Tag>,
) -> Result<(DataSet, usize)> {
    let mut r = Reader {
        buf: bytes,
        pos: 0,
        little: ts.is_little_endian(),
        // Encapsulated syntaxes still encode the *dataset* in Explicit VR
        // Little Endian; only pixel/waveform data is fragmented.
        explicit: ts.is_explicit_vr() || ts.is_encapsulated(),
        encapsulated: ts.is_encapsulated(),
    };
    let ds = decode_elements(&mut r, bytes.len(), 0, stop_before)?;
    Ok((ds, r.pos))
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    little: bool,
    explicit: bool,
    encapsulated: bool,
}

impl<'a> Reader<'a> {
    fn rest(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8]> {
        if self.rest() < n {
            return Err(Error::Truncated {
                offset: self.pos,
                what,
            });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u16(&mut self, what: &'static str) -> Result<u16> {
        let b = self.take(2, what)?;
        Ok(if self.little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&mut self, what: &'static str) -> Result<u32> {
        let b = self.take(4, what)?;
        Ok(if self.little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    fn tag(&mut self) -> Result<Tag> {
        let g = self.u16("tag group")?;
        let e = self.u16("tag element")?;
        Ok(Tag::new(g, e))
    }
}

fn decode_elements(
    r: &mut Reader<'_>,
    end: usize,
    depth: usize,
    stop_before: Option<Tag>,
) -> Result<DataSet> {
    if depth > MAX_DEPTH {
        return Err(Error::BadSequence {
            offset: r.pos,
            message: "nesting too deep".into(),
        });
    }
    let mut ds = DataSet::new();
    while r.pos < end {
        let elem_offset = r.pos;
        let tag = r.tag()?;
        if tag.is_delimiter() {
            return Err(Error::BadSequence {
                offset: elem_offset,
                message: format!("unexpected delimiter {tag}"),
            });
        }
        if let Some(stop) = stop_before
            && tag >= stop
        {
            r.pos = elem_offset; // rewind: caller owns this element
            break;
        }
        let (vr, len) = read_header(r, tag, elem_offset, &ds)?;
        let value = read_value(r, tag, vr, len, elem_offset, depth)?;
        ds.insert(DataElement::new(tag, vr, value));
    }
    Ok(ds)
}

fn read_header(r: &mut Reader<'_>, tag: Tag, offset: usize, ds: &DataSet) -> Result<(Vr, u32)> {
    if r.explicit {
        let raw = r.take(2, "explicit VR")?;
        let vr = Vr::from_bytes([raw[0], raw[1]]).ok_or_else(|| Error::UnknownVr {
            tag,
            code: String::from_utf8_lossy(raw).into_owned(),
            offset,
        })?;
        let len = if vr.uses_long_form() {
            r.take(2, "reserved bytes")?;
            r.u32("long-form length")?
        } else {
            r.u16("short-form length")? as u32
        };
        Ok((vr, len))
    } else {
        let len = r.u32("implicit-VR length")?;
        let vr = implicit_vr(tag, len, ds).ok_or(Error::MissingDictionaryVr { tag, offset })?;
        Ok((vr, len))
    }
}

/// Resolve the VR for implicit-VR decoding, applying context rules.
fn implicit_vr(tag: Tag, len: u32, ds: &DataSet) -> Option<Vr> {
    // Group length elements are retired UL.
    if tag.is_group_length() {
        return Some(Vr::Ul);
    }
    if dictionary_vr(tag).is_some() {
        if is_context_dependent_vr(tag) {
            return Some(resolve_ambiguous(tag, ds));
        }
        return dictionary_vr(tag);
    }
    // Unknown/private tag: opaque bytes, except that an undefined length
    // is first attempted as a sequence (`read_undefined` falls back to
    // concatenated bytes when the content is not sequence-shaped).
    let _ = len;
    Some(Vr::Un)
}

/// Resolve `ox`/`px`/`xs` tags from already-decoded siblings.
fn resolve_ambiguous(tag: Tag, ds: &DataSet) -> Vr {
    match (tag.0, tag.1) {
        (0x7FE0, 0x0010) => {
            if ds.uint16(tags::BITS_ALLOCATED).is_some_and(|b| b <= 8) {
                Vr::Ob
            } else {
                Vr::Ow
            }
        }
        (0x5400, 0x0110) | (0x5400, 0x0112) | (0x5400, 0x100A) | (0x5400, 0x1010) => {
            if ds
                .uint16(tags::WAVEFORM_BITS_ALLOCATED)
                .is_some_and(|b| b <= 8)
            {
                Vr::Ob
            } else {
                Vr::Ow
            }
        }
        (0x0028, 0x0106) | (0x0028, 0x0107) | (0x0028, 0x0120) => {
            if ds.uint16(tags::PIXEL_REPRESENTATION) == Some(1) {
                Vr::Ss
            } else {
                Vr::Us
            }
        }
        _ => dictionary_vr(tag).unwrap_or(Vr::Un),
    }
}

fn read_value(
    r: &mut Reader<'_>,
    tag: Tag,
    vr: Vr,
    len: u32,
    offset: usize,
    depth: usize,
) -> Result<Value> {
    if len == UNDEFINED_LENGTH {
        return read_undefined(r, tag, vr, offset, depth);
    }
    let len = len as usize;
    if vr == Vr::Sq {
        return Ok(Value::Sequence(read_defined_items(r, r.pos + len, depth)?));
    }
    let raw = r.take(len, "element value")?.to_vec();
    decode_primitive(tag, vr, &raw, r.little)
}

fn read_undefined(
    r: &mut Reader<'_>,
    tag: Tag,
    vr: Vr,
    offset: usize,
    depth: usize,
) -> Result<Value> {
    match vr {
        Vr::Sq => Ok(Value::Sequence(read_undefined_items(r, depth)?)),
        Vr::Ob | Vr::Ow => {
            // Encapsulated syntaxes: BOT item + fragments. Native
            // syntaxes: concatenated item bytes (retired but seen).
            read_fragments_or_bytes(r, tag, vr, offset, r.encapsulated)
        }
        Vr::Un => {
            // Try sequence first (implicit-VR unknown sequences arrive as
            // UN); fall back to concatenated item bytes.
            let save = r.pos;
            match read_undefined_items(r, depth) {
                Ok(items) => Ok(Value::Sequence(items)),
                Err(_) => {
                    r.pos = save;
                    Ok(Value::Bytes(read_concatenated_items(r)?))
                }
            }
        }
        Vr::Od | Vr::Of | Vr::Ol | Vr::Ov | Vr::Uc | Vr::Ur | Vr::Ut => {
            let bytes = read_concatenated_items(r)?;
            decode_primitive(tag, vr, &bytes, r.little)
        }
        _ => Err(Error::BadUndefinedLength { tag, vr, offset }),
    }
}

/// Read `(FFFE,E000)` items of a defined-length sequence.
fn read_defined_items(r: &mut Reader<'_>, end: usize, depth: usize) -> Result<Vec<DataSet>> {
    if depth + 1 > MAX_DEPTH {
        return Err(Error::BadSequence {
            offset: r.pos,
            message: "nesting too deep".into(),
        });
    }
    if end > r.buf.len() {
        return Err(Error::Truncated {
            offset: r.pos,
            what: "sequence content",
        });
    }
    let mut items = Vec::new();
    while r.pos < end {
        let at = r.pos;
        let tag = r.tag()?;
        if tag != delimiter::ITEM {
            return Err(Error::BadSequence {
                offset: at,
                message: format!("expected item tag, found {tag}"),
            });
        }
        let len = r.u32("item length")?;
        if len == UNDEFINED_LENGTH {
            items.push(read_until_item_delimiter(r, depth + 1)?);
        } else {
            let item_end = r.pos + len as usize;
            if item_end > end {
                return Err(Error::Truncated {
                    offset: r.pos,
                    what: "item content",
                });
            }
            let mut sub = Reader {
                buf: r.buf,
                pos: r.pos,
                little: r.little,
                explicit: r.explicit,
                encapsulated: r.encapsulated,
            };
            items.push(decode_elements(&mut sub, item_end, depth + 1, None)?);
            r.pos = item_end;
        }
    }
    Ok(items)
}

/// Read items until `(FFFE,E0DD)` sequence delimitation.
fn read_undefined_items(r: &mut Reader<'_>, depth: usize) -> Result<Vec<DataSet>> {
    if depth + 1 > MAX_DEPTH {
        return Err(Error::BadSequence {
            offset: r.pos,
            message: "nesting too deep".into(),
        });
    }
    let mut items = Vec::new();
    loop {
        let at = r.pos;
        let tag = r.tag()?;
        if tag == delimiter::SEQUENCE_DELIMITATION {
            let len = r.u32("sequence delimitation length")?;
            if len != 0 {
                return Err(Error::BadSequence {
                    offset: at,
                    message: "sequence delimitation length must be 0".into(),
                });
            }
            return Ok(items);
        }
        if tag != delimiter::ITEM {
            return Err(Error::BadSequence {
                offset: at,
                message: format!("expected item tag, found {tag}"),
            });
        }
        let len = r.u32("item length")?;
        if len == UNDEFINED_LENGTH {
            items.push(read_until_item_delimiter(r, depth + 1)?);
        } else {
            let item_end = r.pos + len as usize;
            if item_end > r.buf.len() {
                return Err(Error::Truncated {
                    offset: r.pos,
                    what: "item content",
                });
            }
            let mut sub = Reader {
                buf: r.buf,
                pos: r.pos,
                little: r.little,
                explicit: r.explicit,
                encapsulated: r.encapsulated,
            };
            items.push(decode_elements(&mut sub, item_end, depth + 1, None)?);
            r.pos = item_end;
        }
    }
}

/// Read elements until `(FFFE,E00D)` item delimitation.
fn read_until_item_delimiter(r: &mut Reader<'_>, depth: usize) -> Result<DataSet> {
    if depth > MAX_DEPTH {
        return Err(Error::BadSequence {
            offset: r.pos,
            message: "nesting too deep".into(),
        });
    }
    let mut ds = DataSet::new();
    loop {
        let at = r.pos;
        let tag = r.tag()?;
        if tag == delimiter::ITEM_DELIMITATION {
            let len = r.u32("item delimitation length")?;
            if len != 0 {
                return Err(Error::BadSequence {
                    offset: at,
                    message: "item delimitation length must be 0".into(),
                });
            }
            return Ok(ds);
        }
        if tag == delimiter::SEQUENCE_DELIMITATION {
            return Err(Error::BadSequence {
                offset: at,
                message: "sequence delimitation inside undefined item".into(),
            });
        }
        let (vr, len) = read_header(r, tag, at, &ds)?;
        let value = read_value(r, tag, vr, len, at, depth)?;
        ds.insert(DataElement::new(tag, vr, value));
    }
}

/// Encapsulated fragments vs. native undefined-length bytes.
fn read_fragments_or_bytes(
    r: &mut Reader<'_>,
    tag: Tag,
    vr: Vr,
    offset: usize,
    encapsulated: bool,
) -> Result<Value> {
    let mut items: Vec<Vec<u8>> = Vec::new();
    loop {
        let at = r.pos;
        let t = r.tag()?;
        if t == delimiter::SEQUENCE_DELIMITATION {
            let len = r.u32("sequence delimitation length")?;
            if len != 0 {
                return Err(Error::BadFragments {
                    offset: at,
                    message: "sequence delimitation length must be 0".into(),
                });
            }
            break;
        }
        if t != delimiter::ITEM {
            return Err(Error::BadFragments {
                offset: at,
                message: format!("expected item tag, found {t}"),
            });
        }
        let len = r.u32("fragment length")?;
        if len == UNDEFINED_LENGTH {
            return Err(Error::BadFragments {
                offset: at,
                message: "fragment with undefined length".into(),
            });
        }
        items.push(r.take(len as usize, "fragment bytes")?.to_vec());
    }

    if items.is_empty() {
        return Ok(Value::Empty);
    }
    if !encapsulated {
        // Native undefined length: plain concatenation, no BOT.
        let mut bytes = Vec::new();
        for item in &items {
            bytes.extend_from_slice(item);
        }
        return decode_primitive(tag, vr, &bytes, r.little);
    }
    // Encapsulated: first item is the Basic Offset Table.
    let bot_bytes = &items[0];
    if !bot_bytes.len().is_multiple_of(4) {
        return Err(Error::BadFragments {
            offset,
            message: "BOT length not a multiple of 4".into(),
        });
    }
    let mut bot = Vec::with_capacity(bot_bytes.len() / 4);
    for c in bot_bytes.as_chunks::<4>().0 {
        bot.push(u32::from_le_bytes(*c));
    }
    Ok(Value::Fragments(FragmentSequence {
        basic_offset_table: bot,
        fragments: items.into_iter().skip(1).collect(),
    }))
}

/// Concatenate undefined-length item payloads (native `OD OF OL OV UC UR UT`).
fn read_concatenated_items(r: &mut Reader<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let at = r.pos;
        let t = r.tag()?;
        if t == delimiter::SEQUENCE_DELIMITATION {
            let len = r.u32("sequence delimitation length")?;
            if len != 0 {
                return Err(Error::BadFragments {
                    offset: at,
                    message: "sequence delimitation length must be 0".into(),
                });
            }
            return Ok(out);
        }
        if t != delimiter::ITEM {
            return Err(Error::BadFragments {
                offset: at,
                message: format!("expected item tag, found {t}"),
            });
        }
        let len = r.u32("item length")?;
        if len == UNDEFINED_LENGTH {
            return Err(Error::BadFragments {
                offset: at,
                message: "concatenated item with undefined length".into(),
            });
        }
        out.extend_from_slice(r.take(len as usize, "item bytes")?);
    }
}

/// Decode defined-length value bytes for one VR.
fn decode_primitive(tag: Tag, vr: Vr, raw: &[u8], little: bool) -> Result<Value> {
    if raw.is_empty() {
        return Ok(Value::Empty);
    }
    let bad = |message: String| Error::BadValue { tag, vr, message };
    match vr {
        Vr::Sq => Err(bad("SQ must use sequence framing".into())),
        Vr::Pn => Ok(Value::PersonName(
            split_text(raw, vr)
                .into_iter()
                .map(|s| PersonName::parse(&s))
                .collect(),
        )),
        text @ (Vr::Ae
        | Vr::As
        | Vr::Cs
        | Vr::Da
        | Vr::Ds
        | Vr::Dt
        | Vr::Is
        | Vr::Lo
        | Vr::Lt
        | Vr::Sh
        | Vr::St
        | Vr::Tm
        | Vr::Uc
        | Vr::Ui
        | Vr::Ur
        | Vr::Ut) => Ok(Value::Text(split_text(raw, text))),
        Vr::At => {
            if !raw.len().is_multiple_of(4) {
                return Err(bad(format!("AT length {} not a multiple of 4", raw.len())));
            }
            let mut tags = Vec::with_capacity(raw.len() / 4);
            for c in raw.as_chunks::<4>().0 {
                let g = u16_from(c, 0, little);
                let e = u16_from(c, 2, little);
                tags.push(Tag::new(g, e));
            }
            Ok(Value::Tags(tags))
        }
        Vr::Us => Ok(Value::U16(numerics(raw, 2, little, u16_from_c, tag, vr)?)),
        Vr::Ss => Ok(Value::I16(numerics(raw, 2, little, i16_from, tag, vr)?)),
        Vr::Ul | Vr::Ol => Ok(Value::U32(numerics(raw, 4, little, u32_from, tag, vr)?)),
        Vr::Sl => Ok(Value::I32(numerics(raw, 4, little, i32_from, tag, vr)?)),
        Vr::Uv | Vr::Ov => Ok(Value::U64(numerics(raw, 8, little, u64_from, tag, vr)?)),
        Vr::Fl | Vr::Of => Ok(Value::F32(numerics(raw, 4, little, f32_from, tag, vr)?)),
        Vr::Fd | Vr::Od => Ok(Value::F64(numerics(raw, 8, little, f64_from, tag, vr)?)),
        Vr::Ob | Vr::Ow | Vr::Un | Vr::Si => {
            // SI is retired 64-bit; treat remaining binary VRs as bytes.
            Ok(Value::Bytes(raw.to_vec()))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn numerics<T, F>(raw: &[u8], unit: usize, little: bool, f: F, tag: Tag, vr: Vr) -> Result<Vec<T>>
where
    F: Fn(&[u8], bool) -> T,
{
    let mut raw = raw;
    if !raw.len().is_multiple_of(unit) {
        // Tolerate a single trailing pad byte on odd-length values.
        if raw.len() % unit == 1 && raw.last() == Some(&vr.padding_byte()) {
            raw = &raw[..raw.len() - 1];
        } else {
            return Err(Error::BadValue {
                tag,
                vr,
                message: format!("length {} not a multiple of {unit}", raw.len()),
            });
        }
    }
    Ok(raw.chunks_exact(unit).map(|c| f(c, little)).collect())
}

fn u16_from(c: &[u8], o: usize, little: bool) -> u16 {
    let b = [c[o], c[o + 1]];
    if little {
        u16::from_le_bytes(b)
    } else {
        u16::from_be_bytes(b)
    }
}

fn u16_from_c(c: &[u8], little: bool) -> u16 {
    u16_from(c, 0, little)
}
fn i16_from(c: &[u8], little: bool) -> i16 {
    u16_from_c(c, little) as i16
}
fn u32_from(c: &[u8], little: bool) -> u32 {
    let b = [c[0], c[1], c[2], c[3]];
    if little {
        u32::from_le_bytes(b)
    } else {
        u32::from_be_bytes(b)
    }
}
fn i32_from(c: &[u8], little: bool) -> i32 {
    u32_from(c, little) as i32
}
fn u64_from(c: &[u8], little: bool) -> u64 {
    let b = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
    if little {
        u64::from_le_bytes(b)
    } else {
        u64::from_be_bytes(b)
    }
}
fn f32_from(c: &[u8], little: bool) -> f32 {
    f32::from_bits(u32_from(c, little))
}
fn f64_from(c: &[u8], little: bool) -> f64 {
    f64::from_bits(u64_from(c, little))
}

/// Split a text value on backslashes (except single-valued `LT ST UT UR`).
fn split_text(raw: &[u8], vr: Vr) -> Vec<String> {
    let mut bytes = raw;
    // Strip value padding (DICOM pads to even length).
    while bytes.last() == Some(&vr.padding_byte()) && !bytes.is_empty() {
        // UI strips NULs; other text strips spaces. Also strip a stray NUL
        // some writers append to text VRs.
        bytes = &bytes[..bytes.len() - 1];
    }
    if vr != Vr::Ui {
        while bytes.last() == Some(&0x00) && !bytes.is_empty() {
            bytes = &bytes[..bytes.len() - 1];
        }
    }
    let decode = |b: &[u8]| {
        if b.is_ascii() {
            // Fast path; DICOM default repertoire is ASCII.
            String::from_utf8_lossy(b).into_owned()
        } else {
            // No Specific Character Set decoding yet: fall back to Latin-1
            // so European text survives instead of becoming U+FFFD.
            b.iter().map(|c| *c as char).collect()
        }
    };
    match vr {
        Vr::Lt | Vr::St | Vr::Ut | Vr::Ur => vec![decode(bytes)],
        _ => bytes.split(|b| *b == b'\\').map(decode).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explicit_le(bytes: &[u8]) -> Result<DataSet> {
        decode_dataset(bytes, TransferSyntax::ExplicitVrLittleEndian)
    }

    #[test]
    fn decodes_explicit_text_and_numbers() {
        // (0010,0010) PN len 8 "Doe^John", (0028,0010) US len 2 = 512.
        let bytes = [
            0x10, 0x00, 0x10, 0x00, b'P', b'N', 0x08, 0x00, b'D', b'o', b'e', b'^', b'J', b'o',
            b'h', b'n', 0x28, 0x00, 0x10, 0x00, b'U', b'S', 0x02, 0x00, 0x00, 0x02,
        ];
        let ds = explicit_le(&bytes).unwrap();
        let names = ds.person_names(tags::PATIENT_NAME).unwrap();
        assert_eq!(names[0].family_given(), "Doe, John");
        assert_eq!(ds.uint16(tags::ROWS), Some(512));
    }

    #[test]
    fn decodes_implicit_vr_with_dictionary() {
        // (0010,0010) len 8 "Doe^John" (implicit: no VR on wire).
        let bytes = [
            0x10, 0x00, 0x10, 0x00, 0x08, 0x00, 0x00, 0x00, b'D', b'o', b'e', b'^', b'J', b'o',
            b'h', b'n',
        ];
        let ds = decode_dataset(&bytes, TransferSyntax::ImplicitVrLittleEndian).unwrap();
        assert_eq!(ds.vr_of(tags::PATIENT_NAME), Some(Vr::Pn));
        // Unknown/private tag falls back to UN bytes.
        let priv_el = [
            0x09, 0x00, 0x01, 0x10, 0x04, 0x00, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF,
        ];
        let ds = decode_dataset(&priv_el, TransferSyntax::ImplicitVrLittleEndian).unwrap();
        assert_eq!(ds.vr_of(Tag::new(0x0009, 0x1001)), Some(Vr::Un));
        assert_eq!(
            ds.bytes(Tag::new(0x0009, 0x1001)),
            Some([0xDE, 0xAD, 0xBE, 0xEF].as_slice())
        );
    }

    #[test]
    fn resolves_context_vrs() {
        // Bits Allocated = 8 -> implicit Pixel Data is OB.
        let mut bytes = vec![];
        bytes.extend([0x28, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00, 0x08, 0x00]); // BitsAllocated=8
        bytes.extend([0xE0, 0x7F, 0x10, 0x00, 0x04, 0x00, 0x00, 0x00, 1, 2, 3, 4]); // PixelData
        let ds = decode_dataset(&bytes, TransferSyntax::ImplicitVrLittleEndian).unwrap();
        assert_eq!(ds.vr_of(tags::PIXEL_DATA), Some(Vr::Ob));
    }

    #[test]
    fn decodes_defined_length_sequence() {
        // (0008,1140) SQ defined len 16: one item len 8 holding nothing... use a real item:
        // item: (0008,1150) UI len 8 "1.2.3\0" -> item content 8+8=16 bytes.
        let mut bytes = vec![0x08, 0x00, 0x40, 0x11, b'S', b'Q', 0x00, 0x00];
        let item_content = [
            0x08, 0x00, 0x50, 0x11, b'U', b'I', 0x06, 0x00, b'1', b'.', b'2', b'.', b'3', 0x00,
        ];
        let item_len = item_content.len() as u32; // 14
        let sq_len = 8 + item_len; // item header + content
        bytes.extend(sq_len.to_le_bytes());
        bytes.extend([0xFE, 0xFF, 0x00, 0xE0]);
        bytes.extend(item_len.to_le_bytes());
        bytes.extend(item_content);
        let ds = explicit_le(&bytes).unwrap();
        let items = ds.sequence(tags::REFERENCED_IMAGE_SEQUENCE).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].string(tags::REFERENCED_SOP_CLASS_UID),
            Some("1.2.3")
        );
    }

    #[test]
    fn decodes_fragments_with_bot() {
        // Pixel Data undefined length: BOT item (1 frame @0), one fragment "AB".
        let bytes = [
            0xE0, 0x7F, 0x10, 0x00, b'O', b'W', 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xFF,
            0x00, 0xE0, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFE, 0xFF, 0x00, 0xE0,
            0x02, 0x00, 0x00, 0x00, b'A', b'B', 0xFE, 0xFF, 0xDD, 0xE0, 0x00, 0x00, 0x00, 0x00,
        ];
        // Encapsulated TS: BOT + fragments framing applies.
        let ds = decode_dataset(&bytes, TransferSyntax::RleLossless).unwrap();
        match &ds.get(tags::PIXEL_DATA).unwrap().value {
            Value::Fragments(f) => {
                assert_eq!(f.basic_offset_table, vec![0]);
                assert_eq!(f.fragments, vec![vec![b'A', b'B']]);
            }
            v => panic!("expected fragments, got {v:?}"),
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(explicit_le(&[0x10, 0x00]).is_err()); // truncated tag
        assert!(explicit_le(&[0x10, 0x00, 0x10, 0x00, b'X', b'X', 0x00, 0x00]).is_err()); // bad VR
        // Undefined-length US is illegal (only expressible in implicit VR).
        let bad = [0x28, 0x00, 0x10, 0x00, 0xFF, 0xFF, 0xFF, 0xFF];
        let err = decode_dataset(&bad, TransferSyntax::ImplicitVrLittleEndian).unwrap_err();
        assert!(matches!(err, Error::BadUndefinedLength { .. }), "{err:?}");
    }
}
