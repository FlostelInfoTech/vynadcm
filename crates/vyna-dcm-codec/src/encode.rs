//! Dataset encoding for every supported transfer syntax.
//!
//! Encoding rules (NEMA PS3.5 §7, §8, §10):
//!
//! - Elements are written in ascending tag order (`DataSet` keeps that order).
//! - Sequences use defined-length items; only encapsulated pixel/waveform
//!   data uses undefined length with BOT + fragments + delimitation.
//! - Group-length elements (`EEEE == 0`) are retired and skipped.
//! - Values are padded to even length (`UI` with NUL, other text with
//!   space, binary with zero).
//! - DIMSE command sets are encoded with [`TransferSyntax::ImplicitVrLittleEndian`].

use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;
use vyna_dcm_core::{DataSet, FragmentSequence, Tag, TransferSyntax, Value, Vr, delimiter, tags};

use crate::{
    Error, Result, UNDEFINED_LENGTH,
    charset::{Decoding, encode_text, parse_specific_character_set},
    decode::ItemCapture,
};

/// Default fragment size for [`fragment_payload`] (64 KiB).
pub const DEFAULT_FRAGMENT_LEN: usize = 65536;

/// Encode a whole dataset in `ts`.
pub fn encode_dataset(ds: &DataSet, ts: TransferSyntax) -> Result<Vec<u8>> {
    encode_dataset_inner(ds, ts, &mut ItemCapture::default())
}

/// Encode a dataset, capturing item-tag offsets of every sequence
/// element with `seq_tag`. Offsets are relative to the returned bytes
/// (to the inflated bytes when deflated).
pub(crate) fn encode_dataset_capture_items(
    ds: &DataSet,
    ts: TransferSyntax,
    seq_tag: Tag,
) -> Result<(Vec<u8>, Vec<usize>)> {
    let mut capture = ItemCapture {
        tag: Some(seq_tag),
        offsets: Vec::new(),
    };
    let bytes = encode_dataset_inner(ds, ts, &mut capture)?;
    Ok((bytes, capture.offsets))
}

/// Character-set decoding for one dataset: the top-level
/// Specific Character Set, inherited by every nested sequence.
fn charset_of(ds: &DataSet) -> Decoding {
    match ds.get(tags::SPECIFIC_CHARACTER_SET) {
        Some(el) => match &el.value {
            Value::Text(terms) => parse_specific_character_set(terms),
            _ => Decoding::Default,
        },
        None => Decoding::Default,
    }
}

fn encode_dataset_inner(
    ds: &DataSet,
    ts: TransferSyntax,
    capture: &mut ItemCapture,
) -> Result<Vec<u8>> {
    if ts.is_deflated() {
        let plain = encode_dataset_inner(ds, TransferSyntax::ExplicitVrLittleEndian, capture)?;
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&plain)
            .map_err(|e| Error::Deflate(e.to_string()))?;
        return enc.finish().map_err(|e| Error::Deflate(e.to_string()));
    }
    let little = ts.is_little_endian();
    let explicit = ts.is_explicit_vr() || ts.is_encapsulated();
    let mut out = Vec::new();
    let charset = charset_of(ds);
    for (tag, el) in ds.iter() {
        if tag.is_group_length() {
            continue; // retired; never re-emitted
        }
        encode_element(
            tag,
            el.vr,
            &el.value,
            little,
            explicit,
            ts.is_encapsulated(),
            &mut out,
            capture,
            &charset,
        )?;
    }
    Ok(out)
}

/// Encode one element.
#[allow(clippy::too_many_arguments)]
fn encode_element(
    tag: Tag,
    vr: Vr,
    value: &Value,
    little: bool,
    explicit: bool,
    encapsulated: bool,
    out: &mut Vec<u8>,
    capture: &mut ItemCapture,
    charset: &Decoding,
) -> Result<()> {
    put_tag(out, tag, little);
    let unencodable = |message: String| Error::Unencodable { tag, vr, message };

    // Undefined-length framing applies only to encapsulated fragments.
    if let Value::Fragments(f) = value {
        if !matches!(vr, Vr::Ob | Vr::Ow) {
            return Err(unencodable("fragments require OB or OW".into()));
        }
        if !encapsulated {
            return Err(unencodable(
                "fragments need an encapsulated transfer syntax; join or re-encode first".into(),
            ));
        }
        put_vr_len(out, vr, explicit, little, UNDEFINED_LENGTH);
        encode_bot(out, f, little);
        for frag in &f.fragments {
            put_tag(out, delimiter::ITEM, little);
            put_u32(out, frag.len() as u32, little);
            out.extend_from_slice(frag);
        }
        put_tag(out, delimiter::SEQUENCE_DELIMITATION, little);
        put_u32(out, 0, little);
        return Ok(());
    }

    if vr == Vr::Sq {
        let Value::Sequence(items) = value else {
            return Err(unencodable("SQ element without sequence value".into()));
        };
        if !explicit {
            // Implicit VR sequences are always undefined-length.
            put_u32(out, UNDEFINED_LENGTH, little);
            for item in items {
                capture.record(tag, out.len());
                put_tag(out, delimiter::ITEM, little);
                put_u32(out, UNDEFINED_LENGTH, little);
                let bytes = encode_dataset_in(item, little, false, capture, charset)?;
                out.extend_from_slice(&bytes);
                put_tag(out, delimiter::ITEM_DELIMITATION, little);
                put_u32(out, 0, little);
            }
            put_tag(out, delimiter::SEQUENCE_DELIMITATION, little);
            put_u32(out, 0, little);
            return Ok(());
        }
        if items.is_empty() {
            put_vr_len(out, vr, true, little, 0);
            return Ok(());
        }
        let mut body = Vec::new();
        // Item positions are measured against the body, then rebased
        // past the tag + value-length header once it is written.
        let capturing = capture.tag == Some(tag);
        let mut rel = Vec::new();
        for item in items {
            if capturing {
                rel.push(body.len());
            }
            put_tag(&mut body, delimiter::ITEM, little);
            let bytes = encode_dataset_in(item, little, true, capture, charset)?;
            put_u32(&mut body, bytes.len() as u32, little);
            body.extend_from_slice(&bytes);
        }
        put_vr_len(out, vr, true, little, body.len() as u32);
        let base = out.len();
        for r in rel {
            capture.record(tag, base + r);
        }
        out.extend_from_slice(&body);
        return Ok(());
    }

    if matches!(value, Value::Sequence(_)) {
        return Err(unencodable("sequence value with non-SQ VR".into()));
    }

    let bytes = encode_primitive(tag, vr, value, little, charset)?;
    if !explicit && bytes.len() > u32::MAX as usize {
        return Err(unencodable("value exceeds 32-bit length".into()));
    }
    if explicit && !vr.uses_long_form() && bytes.len() > u16::MAX as usize {
        return Err(unencodable(format!(
            "value length {} exceeds 16-bit short form for {vr}",
            bytes.len()
        )));
    }
    put_vr_len(out, vr, explicit, little, bytes.len() as u32);
    out.extend_from_slice(&bytes);
    Ok(())
}

fn encode_dataset_in(
    ds: &DataSet,
    little: bool,
    explicit: bool,
    capture: &mut ItemCapture,
    charset: &Decoding,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (tag, el) in ds.iter() {
        if tag.is_group_length() {
            continue;
        }
        encode_element(
            tag, el.vr, &el.value, little, explicit, false, &mut out, capture, charset,
        )?;
    }
    Ok(out)
}

fn encode_bot(out: &mut Vec<u8>, f: &FragmentSequence, little: bool) {
    put_tag(out, delimiter::ITEM, little);
    put_u32(out, (f.basic_offset_table.len() * 4) as u32, little);
    for o in &f.basic_offset_table {
        put_u32(out, *o, little);
    }
}

/// Split a payload into fragments (empty BOT).
///
/// Non-final fragments keep even length, so at most the very last
/// fragment carries a pad byte: pads inside the stream would corrupt
/// consumers that join fragments back together.
pub fn fragment_payload(data: &[u8], fragment_len: usize) -> FragmentSequence {
    let len = fragment_len.max(2);
    let mut fragments = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let mut n = rest.len().min(len);
        if n < rest.len() && n % 2 == 1 {
            n -= 1; // spill the odd byte into the next fragment
        }
        let mut frag = rest[..n].to_vec();
        if frag.len() % 2 == 1 {
            frag.push(0); // fragments are always even-length
        }
        fragments.push(frag);
        rest = &rest[n..];
    }
    FragmentSequence {
        basic_offset_table: Vec::new(),
        fragments,
    }
}

/// Encode a defined-length value for one VR.
fn encode_primitive(
    tag: Tag,
    vr: Vr,
    value: &Value,
    little: bool,
    charset: &Decoding,
) -> Result<Vec<u8>> {
    let unencodable = |message: String| Error::Unencodable { tag, vr, message };
    match value {
        Value::Empty => Ok(Vec::new()),
        Value::Text(v) => {
            if !vr.is_text() {
                return Err(unencodable(format!("text value with {vr}")));
            }
            if !is_multivalued_text(vr) && v.len() > 1 {
                return Err(unencodable(format!("{vr} holds a single value")));
            }
            for s in v {
                if is_multivalued_text(vr) && s.contains('\\') {
                    return Err(unencodable("backslash inside multi-valued text".into()));
                }
                if vr == Vr::Ui && !vyna_dcm_core::is_valid_uid(s.trim_end_matches('\0')) {
                    return Err(unencodable(format!("invalid UID {s:?}")));
                }
            }
            let raw = match vr {
                // One code-extension run per value, joined by backslash.
                Vr::Lo | Vr::Sh | Vr::St | Vr::Lt | Vr::Ut => {
                    let mut raw = Vec::new();
                    for (i, text) in v.iter().enumerate() {
                        if i > 0 {
                            raw.push(b'\\');
                        }
                        raw.extend_from_slice(&encode_text(text, charset));
                    }
                    raw
                }
                // UC/UR are always UTF-8; the rest keep the historical
                // UTF-8 passthrough.
                _ => v.join("\\").into_bytes(),
            };
            Ok(pad(raw, vr.padding_byte()))
        }
        Value::PersonName(names) => {
            if vr != Vr::Pn {
                return Err(unencodable(format!("person names with {vr}")));
            }
            let mut raw = Vec::new();
            for (i, name) in names.iter().enumerate() {
                if i > 0 {
                    raw.push(b'\\');
                }
                // One code-extension run per name, spanning its groups.
                raw.extend_from_slice(&encode_text(&name.to_dicom_string(), charset));
            }
            Ok(pad(raw, 0x20))
        }
        Value::Tags(tags) => {
            if vr != Vr::At {
                return Err(unencodable(format!("tag list with {vr}")));
            }
            let mut out = Vec::with_capacity(tags.len() * 4);
            for t in tags {
                put_u16(&mut out, t.group(), little);
                put_u16(&mut out, t.element(), little);
            }
            Ok(out)
        }
        Value::U16(v) => {
            if !matches!(vr, Vr::Us | Vr::Ow) {
                return Err(unencodable(format!("u16 values with {vr}")));
            }
            Ok(words(v.iter().map(|x| x.to_le_bytes()), little))
        }
        Value::I16(v) => {
            if !matches!(vr, Vr::Ss | Vr::Ow) {
                return Err(unencodable(format!("i16 values with {vr}")));
            }
            Ok(words(v.iter().map(|x| x.to_le_bytes()), little))
        }
        Value::U32(v) => {
            if !matches!(vr, Vr::Ul | Vr::Ol) {
                return Err(unencodable(format!("u32 values with {vr}")));
            }
            let mut out = Vec::with_capacity(v.len() * 4);
            for x in v {
                put_u32(&mut out, *x, little);
            }
            Ok(out)
        }
        Value::I32(v) => {
            if vr != Vr::Sl {
                return Err(unencodable(format!("i32 values with {vr}")));
            }
            let mut out = Vec::with_capacity(v.len() * 4);
            for x in v {
                put_u32(&mut out, *x as u32, little);
            }
            Ok(out)
        }
        Value::U64(v) => {
            if !matches!(vr, Vr::Uv | Vr::Ov) {
                return Err(unencodable(format!("u64 values with {vr}")));
            }
            let mut out = Vec::with_capacity(v.len() * 8);
            for x in v {
                let b = x.to_le_bytes();
                let b = if little { b } else { x.to_be_bytes() };
                out.extend_from_slice(&b);
            }
            Ok(out)
        }
        Value::I64(_) => Err(unencodable(
            "no DICOM VR encodes bare i64 (use SL/SS)".into(),
        )),
        Value::F32(v) => {
            if !matches!(vr, Vr::Fl | Vr::Of) {
                return Err(unencodable(format!("f32 values with {vr}")));
            }
            let mut out = Vec::with_capacity(v.len() * 4);
            for x in v {
                put_u32(&mut out, x.to_bits(), little);
            }
            Ok(out)
        }
        Value::F64(v) => {
            if !matches!(vr, Vr::Fd | Vr::Od) {
                return Err(unencodable(format!("f64 values with {vr}")));
            }
            let mut out = Vec::with_capacity(v.len() * 8);
            for x in v {
                let b = x.to_bits().to_le_bytes();
                let b = if little { b } else { x.to_bits().to_be_bytes() };
                out.extend_from_slice(&b);
            }
            Ok(out)
        }
        Value::Bytes(b) => {
            if !matches!(
                vr,
                Vr::Ob | Vr::Ow | Vr::Od | Vr::Of | Vr::Ol | Vr::Ov | Vr::Un | Vr::Si
            ) {
                return Err(unencodable(format!("raw bytes with {vr}")));
            }
            Ok(pad(b.clone(), 0x00))
        }
        Value::Sequence(_) => Err(unencodable(
            "sequence value reached primitive encoder".into(),
        )),
        Value::Fragments(_) => Err(unencodable("fragments reached primitive encoder".into())),
    }
}

fn words<'a>(it: impl Iterator<Item = [u8; 2]> + 'a, little: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for w in it {
        if little {
            out.extend_from_slice(&w);
        } else {
            out.extend_from_slice(&[w[1], w[0]]);
        }
    }
    out
}

/// Text VRs that split/join on backslash (all but `LT ST UT UR`).
fn is_multivalued_text(vr: Vr) -> bool {
    vr.is_text() && !matches!(vr, Vr::Lt | Vr::St | Vr::Ut | Vr::Ur)
}

fn pad(mut bytes: Vec<u8>, pad: u8) -> Vec<u8> {
    if bytes.len() % 2 == 1 {
        bytes.push(pad);
    }
    bytes
}

fn put_u16(out: &mut Vec<u8>, v: u16, little: bool) {
    let b = if little {
        v.to_le_bytes()
    } else {
        v.to_be_bytes()
    };
    out.extend_from_slice(&b);
}

fn put_u32(out: &mut Vec<u8>, v: u32, little: bool) {
    let b = if little {
        v.to_le_bytes()
    } else {
        v.to_be_bytes()
    };
    out.extend_from_slice(&b);
}

fn put_tag(out: &mut Vec<u8>, tag: Tag, little: bool) {
    put_u16(out, tag.group(), little);
    put_u16(out, tag.element(), little);
}

fn put_vr_len(out: &mut Vec<u8>, vr: Vr, explicit: bool, little: bool, len: u32) {
    if !explicit {
        // Implicit VR: bare 32-bit length (Implicit VR is always LE).
        out.extend_from_slice(&len.to_le_bytes());
        return;
    }
    out.extend_from_slice(&vr.code());
    if vr.uses_long_form() {
        out.extend_from_slice(&[0x00, 0x00]);
        put_u32(out, len, little);
    } else {
        put_u16(out, len as u16, little);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_dataset;
    use vyna_dcm_core::{DataElement, PersonName, tags};

    fn kitchen_sink() -> DataSet {
        DataSet::new()
            .with(DataElement::uid(tags::SOP_INSTANCE_UID, "1.2.3.4.5"))
            .with(DataElement::person_name(
                tags::PATIENT_NAME,
                vec![PersonName::parse("Doe^Jane")],
            ))
            .with(DataElement::text(tags::MODALITY, Vr::Cs, "ECG"))
            .with(DataElement::u16(tags::ROWS, vec![512, 256]))
            .with(DataElement::u32_single(
                tags::NUMBER_OF_WAVEFORM_SAMPLES,
                5000,
            ))
            .with(DataElement::f32(
                tags::WAVEFORM_DATA_DISPLAY_SCALE,
                vec![0.5],
            ))
            .with(DataElement::text(tags::RESCALE_SLOPE, Vr::Ds, "1.25"))
            .with(DataElement::bytes(
                tags::PIXEL_DATA,
                Vr::Ow,
                vec![1, 2, 3, 4],
            ))
            .with(DataElement::sequence(
                tags::REFERENCED_IMAGE_SEQUENCE,
                vec![
                    DataSet::new().with(DataElement::uid(tags::REFERENCED_SOP_CLASS_UID, "1.2.3")),
                ],
            ))
    }

    #[test]
    fn round_trips_all_uncompressed_syntaxes() {
        let ds = kitchen_sink();
        for ts in [
            TransferSyntax::ImplicitVrLittleEndian,
            TransferSyntax::ExplicitVrLittleEndian,
            TransferSyntax::ExplicitVrBigEndian,
            TransferSyntax::DeflatedExplicitVrLittleEndian,
        ] {
            let bytes = encode_dataset(&ds, ts).unwrap();
            let back = decode_dataset(&bytes, ts).unwrap();
            assert_eq!(back, ds, "round trip failed for {ts:?}");
        }
    }

    #[test]
    fn fragments_pad_only_at_the_end() {
        // Odd split length: pad bytes must not land mid-stream.
        let f = fragment_payload(&[1, 2, 3, 4, 5, 6, 7], 3);
        assert!(f.fragments.iter().all(|f| f.len() % 2 == 0));
        assert_eq!(f.joined(), vec![1, 2, 3, 4, 5, 6, 7, 0]);
        // Even data needs no padding at all.
        let f = fragment_payload(&[1, 2, 3, 4, 5, 6], 4);
        assert_eq!(f.joined(), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn padding_and_group_lengths() {
        // Odd-length text is space-padded; group lengths are dropped.
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "ABC");
        ds.put_u32_single(Tag::new(0x0010, 0x0000), 42);
        let bytes = encode_dataset(&ds, TransferSyntax::ExplicitVrLittleEndian).unwrap();
        assert_eq!(&bytes[8..12], b"ABC ");
        let back = decode_dataset(&bytes, TransferSyntax::ExplicitVrLittleEndian).unwrap();
        assert_eq!(back.string(tags::PATIENT_ID), Some("ABC"));
        assert!(!back.contains(Tag::new(0x0010, 0x0000)));
    }

    #[test]
    fn fragments_need_encapsulated_syntax() {
        let mut ds = DataSet::new();
        ds.insert(DataElement::new(
            tags::PIXEL_DATA,
            Vr::Ob,
            Value::Fragments(fragment_payload(&[1, 2, 3, 4], 2)),
        ));
        assert!(encode_dataset(&ds, TransferSyntax::ExplicitVrLittleEndian).is_err());
        let bytes = encode_dataset(&ds, TransferSyntax::RleLossless).unwrap();
        let back = decode_dataset(&bytes, TransferSyntax::RleLossless).unwrap();
        assert_eq!(back, ds);
    }

    #[test]
    fn rejects_mismatched_values() {
        let mut ds = DataSet::new();
        ds.insert(DataElement::new(
            tags::ROWS,
            Vr::Us,
            Value::Text(vec!["x".into()]),
        ));
        assert!(encode_dataset(&ds, TransferSyntax::ExplicitVrLittleEndian).is_err());
        let mut ds = DataSet::new();
        ds.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, "not a uid!!");
        assert!(encode_dataset(&ds, TransferSyntax::ExplicitVrLittleEndian).is_err());
    }

    fn with_scs(ds: &mut DataSet, terms: &[&str]) {
        ds.insert(DataElement::texts(
            tags::SPECIFIC_CHARACTER_SET,
            Vr::Cs,
            terms.iter().map(ToString::to_string).collect(),
        ));
    }

    #[test]
    fn round_trips_latin1_patient_name() {
        // Oracle (CPython latin-1): "Müller^Jörg".
        let mut ds = DataSet::new();
        with_scs(&mut ds, &["ISO_IR 100"]);
        ds.insert(DataElement::person_name(
            tags::PATIENT_NAME,
            vec![PersonName::parse("Müller^Jörg")],
        ));
        let ts = TransferSyntax::ExplicitVrLittleEndian;
        let bytes = encode_dataset(&ds, ts).unwrap();
        let oracle = "Müller^Jörg".chars().map(|c| c as u8).collect::<Vec<_>>();
        assert!(bytes.windows(oracle.len()).any(|w| w == oracle));
        let back = decode_dataset(&bytes, ts).unwrap();
        assert_eq!(
            back.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            "Müller^Jörg"
        );
    }

    #[test]
    fn round_trips_japanese_pn_component_groups() {
        // Oracle (CPython iso2022-jp): 3-group PN.
        let name = "Yamada^Tarou=山田^太郎=やまだ^たろう";
        let mut ds = DataSet::new();
        with_scs(&mut ds, &["ISO 2022 IR 13", "ISO 2022 IR 87"]);
        ds.insert(DataElement::person_name(
            tags::PATIENT_NAME,
            vec![PersonName::parse(name)],
        ));
        let ts = TransferSyntax::ExplicitVrLittleEndian;
        let bytes = encode_dataset(&ds, ts).unwrap();
        // ISO-2022 escape sequences are on the wire.
        assert!(bytes.windows(3).any(|w| w == b"\x1b$B"));
        let back = decode_dataset(&bytes, ts).unwrap();
        assert_eq!(
            back.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            name
        );
    }

    #[test]
    fn sequence_items_inherit_charset() {
        // Oracle (CPython gbk): 王伟.
        let mut ds = DataSet::new();
        with_scs(&mut ds, &["GBK"]);
        let mut item = DataSet::new();
        item.put_text(tags::PATIENT_ID, Vr::Lo, "王伟");
        ds.insert(DataElement::sequence(
            tags::REFERENCED_IMAGE_SEQUENCE,
            vec![item],
        ));
        let ts = TransferSyntax::ExplicitVrLittleEndian;
        let bytes = encode_dataset(&ds, ts).unwrap();
        assert!(bytes.windows(4).any(|w| w == b"\xcd\xf5\xce\xb0"));
        let back = decode_dataset(&bytes, ts).unwrap();
        let items = back.sequence(tags::REFERENCED_IMAGE_SEQUENCE).unwrap();
        assert_eq!(items[0].string(tags::PATIENT_ID), Some("王伟"));
    }

    #[test]
    fn decodes_oracle_gb18030_bytes() {
        // SCS "GB18030 " + PN 王伟 as CPython gb18030 emits them.
        let mut bytes = vec![
            0x08, 0x00, 0x05, 0x00, b'C', b'S', 0x08, 0x00, b'G', b'B', b'1', b'8', b'0', b'3',
            b'0', b' ',
        ];
        bytes.extend([
            0x10, 0x00, 0x10, 0x00, b'P', b'N', 0x04, 0x00, 0xcd, 0xf5, 0xce, 0xb0,
        ]);
        let ds = decode_dataset(&bytes, TransferSyntax::ExplicitVrLittleEndian).unwrap();
        assert_eq!(
            ds.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            "王伟"
        );
    }
}
