//! Native DICOM Model XML (NEMA PS3.19 Annex A).
//!
//! - Elements become `<DicomAttribute tag="00100010" vr="PN"
//!   keyword="PatientName">` under `<NativeDicomModel>`.
//! - Text, numbers, and `AT` (8-hex-digit strings) become `<Value
//!   number="n">` children; `DS` keeps its wire lexical form
//!   (unlike the JSON mapping, no float trip).
//! - `PN` becomes `<PersonName number="n">` with `Alphabetic` /
//!   `Ideographic` / `Phonetic` groups of `FamilyName` etc.
//! - Byte values become `<InlineBinary>` (base64); sequences nest
//!   `<Item number="n">` datasets. `OL`/`OV`/`OD`/`OF` stay numeric
//!   `<Value>`s (as in the JSON mapping, not base64).
//! - Empty attributes are self-closing and parse back as
//!   [`Value::Empty`] (empty vectors normalize to it).
//! - `<BulkData>` references are refused: only inline values parse.
//!   DCMTK's proprietary `<element>` format is out of scope.

use base64::Engine;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use vyna_dcm_core::{DataElement, DataSet, NameComponents, PersonName, Tag, Value, Vr, lookup};

use crate::json::{parse_tag_key, tag_key};
use crate::{Error, Result};

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Convert a dataset to a Native DICOM Model XML document.
#[must_use]
pub fn dataset_to_xml(ds: &DataSet) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<NativeDicomModel xml:space=\"preserve\">\n",
    );
    write_attributes(&mut out, ds, 1);
    out.push_str("</NativeDicomModel>\n");
    out
}

fn write_attributes(out: &mut String, ds: &DataSet, indent: usize) {
    for (tag, el) in ds.iter() {
        write_attribute(out, tag, el, indent);
    }
}

fn write_attribute(out: &mut String, tag: Tag, el: &DataElement, indent: usize) {
    use std::fmt::Write;
    let pad = "  ".repeat(indent);
    let _ = write!(
        out,
        "{pad}<DicomAttribute tag=\"{}\" vr=\"{}\"",
        tag_key(tag),
        el.vr.as_str()
    );
    if let Some(entry) = lookup(tag) {
        let _ = write!(out, " keyword=\"{}\"", entry.keyword);
    }
    if childless(&el.value) {
        out.push_str("/>\n");
        return;
    }
    out.push_str(">\n");
    write_value(out, &el.value, indent + 1);
    let _ = writeln!(out, "{pad}</DicomAttribute>");
}

fn write_value(out: &mut String, value: &Value, indent: usize) {
    use std::fmt::Write;
    let pad = "  ".repeat(indent);
    match value {
        Value::Empty => {}
        Value::Text(v) => {
            for (i, s) in v.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "{pad}<Value number=\"{}\">{}</Value>",
                    i + 1,
                    escape(s)
                );
            }
        }
        Value::PersonName(names) => {
            for (i, name) in names.iter().enumerate() {
                write_person_name(out, name, i + 1, &pad);
            }
        }
        Value::Tags(tags) => {
            for (i, t) in tags.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "{pad}<Value number=\"{}\">{}</Value>",
                    i + 1,
                    tag_key(*t)
                );
            }
        }
        Value::U16(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::I16(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::U32(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::I32(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::U64(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::I64(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::F32(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::F64(v) => write_numbers(out, &pad, v.iter().map(ToString::to_string)),
        Value::Bytes(b) => {
            let _ = writeln!(out, "{pad}<InlineBinary>{}</InlineBinary>", B64.encode(b));
        }
        Value::Sequence(items) => {
            for (i, item) in items.iter().enumerate() {
                let _ = writeln!(out, "{pad}<Item number=\"{}\">", i + 1);
                write_attributes(out, item, indent + 1);
                let _ = writeln!(out, "{pad}</Item>");
            }
        }
        Value::Fragments(f) => {
            // No BasicOffsetTable slot in PS3.19: emit the joined bytes.
            let _ = writeln!(
                out,
                "{pad}<InlineBinary>{}</InlineBinary>",
                B64.encode(f.joined())
            );
        }
    }
}

fn write_numbers(out: &mut String, pad: &str, it: impl Iterator<Item = String>) {
    use std::fmt::Write;
    for (i, n) in it.enumerate() {
        let _ = writeln!(out, "{pad}<Value number=\"{}\">{n}</Value>", i + 1);
    }
}

/// True when the writer would emit no children (self-closing tag).
fn childless(value: &Value) -> bool {
    match value {
        Value::Empty => true,
        Value::Text(v) => v.is_empty(),
        Value::PersonName(v) => v.is_empty(),
        Value::Tags(v) => v.is_empty(),
        Value::U16(v) => v.is_empty(),
        Value::I16(v) => v.is_empty(),
        Value::U32(v) => v.is_empty(),
        Value::I32(v) => v.is_empty(),
        Value::U64(v) => v.is_empty(),
        Value::I64(v) => v.is_empty(),
        Value::F32(v) => v.is_empty(),
        Value::F64(v) => v.is_empty(),
        Value::Bytes(v) => v.is_empty(),
        Value::Sequence(v) => v.is_empty(),
        Value::Fragments(f) => f.fragments.is_empty() && f.basic_offset_table.is_empty(),
    }
}

fn write_person_name(out: &mut String, name: &PersonName, number: usize, pad: &str) {
    use std::fmt::Write;
    let shown: Vec<(&str, &NameComponents)> = [
        ("Alphabetic", Some(&name.alphabetic)),
        ("Ideographic", name.ideographic.as_ref()),
        ("Phonetic", name.phonetic.as_ref()),
    ]
    .into_iter()
    .filter_map(|(label, g)| g.filter(|g| !g.is_empty()).map(|g| (label, g)))
    .collect();
    if shown.is_empty() {
        let _ = writeln!(out, "{pad}<PersonName number=\"{number}\"/>");
        return;
    }
    let _ = writeln!(out, "{pad}<PersonName number=\"{number}\">");
    for (label, group) in shown {
        let _ = writeln!(out, "{pad}  <{label}>");
        for (component, text) in [
            ("FamilyName", &group.family),
            ("GivenName", &group.given),
            ("MiddleName", &group.middle),
            ("NamePrefix", &group.prefix),
            ("NameSuffix", &group.suffix),
        ] {
            if !text.is_empty() {
                let _ = writeln!(out, "{pad}    <{component}>{}</{component}>", escape(text));
            }
        }
        let _ = writeln!(out, "{pad}  </{label}>");
    }
    let _ = writeln!(out, "{pad}</PersonName>");
}

/// Escape text/attribute content (`&`, `<`, `>`).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

// --- Reader ---------------------------------------------------------------

/// Parse a Native DICOM Model XML document back into a dataset.
///
/// Liberal where it aids interop: `keyword` and `number` attributes
/// are accepted but never required; strict where it aids debugging:
/// mistyped numbers, `BulkData` references, and children under the
/// wrong VR are errors.
pub fn dataset_from_xml(xml: &str) -> Result<DataSet> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match next(&mut reader, &mut buf)? {
            Event::Start(e) if is(&e, b"NativeDicomModel") => {
                return read_attributes(&mut reader, &mut buf, b"NativeDicomModel");
            }
            Event::Empty(e) if is(&e, b"NativeDicomModel") => return Ok(DataSet::new()),
            Event::Eof => return Err(Error::Xml("missing NativeDicomModel root".into())),
            _ => {}
        }
    }
}

fn next<'b>(reader: &mut Reader<&[u8]>, buf: &'b mut Vec<u8>) -> Result<Event<'b>> {
    buf.clear();
    reader
        .read_event_into(buf)
        .map_err(|e| Error::Xml(e.to_string()))
}

fn is(e: &BytesStart<'_>, name: &[u8]) -> bool {
    e.name().as_ref() == name
}

/// Read `DicomAttribute` children until `</end>`.
fn read_attributes(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>, end: &[u8]) -> Result<DataSet> {
    let mut ds = DataSet::new();
    loop {
        match next(reader, buf)? {
            Event::Start(e) if is(&e, b"DicomAttribute") => {
                let (tag, vr) = attribute_head(&e)?;
                ds.insert(read_attribute_body(reader, buf, tag, vr)?);
            }
            Event::Empty(e) if is(&e, b"DicomAttribute") => {
                let (tag, vr) = attribute_head(&e)?;
                ds.insert(DataElement::new(tag, vr, Value::Empty));
            }
            Event::End(e) if e.name().as_ref() == end => return Ok(ds),
            Event::Text(e) => check_ws(&e)?,
            Event::Eof => return Err(Error::Xml("unexpected end of input".into())),
            Event::Start(e) | Event::Empty(e) => {
                return Err(Error::Xml(format!(
                    "unexpected <{}>",
                    String::from_utf8_lossy(e.name().as_ref())
                )));
            }
            _ => {}
        }
    }
}

/// Required `tag` + `vr` from a `DicomAttribute` open tag.
fn attribute_head(e: &BytesStart<'_>) -> Result<(Tag, Vr)> {
    let mut tag = None;
    let mut vr = None;
    for attr in e.attributes() {
        let attr = attr.map_err(|e| Error::Xml(e.to_string()))?;
        let value = attr
            .unescape_value()
            .map_err(|e| Error::Xml(e.to_string()))?;
        match attr.key.as_ref() {
            b"tag" => tag = Some(value.into_owned()),
            b"vr" => vr = Some(value.into_owned()),
            _ => {}
        }
    }
    let tag = tag.ok_or_else(|| Error::Xml("DicomAttribute without tag".into()))?;
    let vr = vr.ok_or_else(|| Error::Xml(format!("{tag}: missing vr")))?;
    let tag = parse_tag_key(&tag).map_err(|_| Error::Xml(format!("bad tag {tag:?}")))?;
    let vr: Vr = vr
        .parse()
        .map_err(|_| Error::Xml(format!("{tag}: bad vr {vr:?}")))?;
    Ok((tag, vr))
}

/// Read one attribute body: values, names, items, or binary.
fn read_attribute_body(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    tag: Tag,
    vr: Vr,
) -> Result<DataElement> {
    let mut texts = Vec::new();
    let mut names = Vec::new();
    let mut items = Vec::new();
    let mut binary: Option<Vec<u8>> = None;
    let mut saw = Vec::new();
    loop {
        match next(reader, buf)? {
            Event::Start(e) if is(&e, b"Value") => {
                texts.push(leaf_text(reader, buf, b"Value", tag)?);
                saw.push("Value");
            }
            Event::Empty(e) if is(&e, b"Value") => {
                texts.push(String::new());
                saw.push("Value");
            }
            Event::Start(e) if is(&e, b"PersonName") => {
                names.push(read_person_name(reader, buf, tag)?);
                saw.push("PersonName");
            }
            Event::Empty(e) if is(&e, b"PersonName") => {
                names.push(PersonName::default());
                saw.push("PersonName");
            }
            Event::Start(e) if is(&e, b"Item") => {
                items.push(read_attributes(reader, buf, b"Item")?);
                saw.push("Item");
            }
            Event::Empty(e) if is(&e, b"Item") => {
                items.push(DataSet::new());
                saw.push("Item");
            }
            Event::Start(e) if is(&e, b"InlineBinary") => {
                binary = Some(decode_inline_binary(
                    &leaf_text(reader, buf, b"InlineBinary", tag)?,
                    tag,
                )?);
                saw.push("InlineBinary");
            }
            Event::Empty(e) if is(&e, b"InlineBinary") => {
                binary = Some(Vec::new());
                saw.push("InlineBinary");
            }
            Event::Start(e) | Event::Empty(e) if is(&e, b"BulkData") => {
                return Err(Error::Xml(format!(
                    "{tag}: BulkData references are not resolved"
                )));
            }
            Event::End(e) if e.name().as_ref() == b"DicomAttribute" => {
                let value = build_value(tag, vr, texts, names, items, binary, &saw)?;
                return Ok(DataElement::new(tag, vr, value));
            }
            Event::Text(e) => check_ws(&e)?,
            Event::Eof => return Err(Error::Xml(format!("{tag}: unexpected end of input"))),
            Event::Start(e) | Event::Empty(e) => {
                return Err(Error::Xml(format!(
                    "{tag}: unexpected <{}>",
                    String::from_utf8_lossy(e.name().as_ref())
                )));
            }
            _ => {}
        }
    }
}

/// Verbatim leaf text until `</end>` (empty elements handled by callers).
fn leaf_text(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    end: &[u8],
    tag: Tag,
) -> Result<String> {
    let mut out = String::new();
    loop {
        match next(reader, buf)? {
            Event::Text(e) => {
                out.push_str(&e.xml_content().map_err(|e| Error::Xml(e.to_string()))?)
            }
            Event::GeneralRef(e) => out.push_str(&resolve_entity(e.as_ref(), tag)?),
            Event::CData(e) => out.push_str(
                std::str::from_utf8(e.as_ref())
                    .map_err(|_| Error::Xml(format!("{tag}: bad CDATA")))?,
            ),
            Event::End(e) if e.name().as_ref() == end => return Ok(out),
            Event::Eof => return Err(Error::Xml(format!("{tag}: unexpected end of input"))),
            _ => return Err(Error::Xml(format!("{tag}: nested markup in leaf"))),
        }
    }
}

/// Resolve one predefined entity or character reference.
fn resolve_entity(raw: &[u8], tag: Tag) -> Result<String> {
    let bad = || Error::Xml(format!("{tag}: unknown entity {raw:?}"));
    let text = std::str::from_utf8(raw).map_err(|_| bad())?;
    match text {
        "amp" => Ok("&".into()),
        "lt" => Ok("<".into()),
        "gt" => Ok(">".into()),
        "quot" => Ok("\"".into()),
        "apos" => Ok("'".into()),
        _ => {
            let digits = text.strip_prefix('#').ok_or_else(bad)?;
            let code = if let Some(hex) = digits
                .strip_prefix('x')
                .or_else(|| digits.strip_prefix('X'))
            {
                u32::from_str_radix(hex, 16).map_err(|_| bad())?
            } else {
                digits.parse::<u32>().map_err(|_| bad())?
            };
            char::from_u32(code).map(String::from).ok_or_else(bad)
        }
    }
}

fn check_ws(e: &quick_xml::events::BytesText<'_>) -> Result<()> {
    if e.as_ref().iter().all(|b| b.is_ascii_whitespace()) {
        Ok(())
    } else {
        Err(Error::Xml("unexpected text content".into()))
    }
}

fn decode_inline_binary(text: &str, tag: Tag) -> Result<Vec<u8>> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    B64.decode(compact.as_bytes())
        .map_err(|e| Error::Xml(format!("{tag}: bad InlineBinary ({e})")))
}

fn read_person_name(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>, tag: Tag) -> Result<PersonName> {
    let mut groups: [Option<NameComponents>; 3] = [None, None, None];
    let slot = |name: &[u8]| match name {
        b"Alphabetic" => Some(0),
        b"Ideographic" => Some(1),
        b"Phonetic" => Some(2),
        _ => None,
    };
    loop {
        match next(reader, buf)? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_vec();
                match slot(&name) {
                    Some(i) => groups[i] = Some(read_name_group(reader, buf, &name, tag)?),
                    None => {
                        return Err(Error::Xml(format!(
                            "{tag}: unexpected <{}>",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                }
            }
            Event::Empty(e) => {
                if slot(e.name().as_ref()).is_none() {
                    return Err(Error::Xml(format!(
                        "{tag}: unexpected <{}>",
                        String::from_utf8_lossy(e.name().as_ref())
                    )));
                }
            }
            Event::End(e) if e.name().as_ref() == b"PersonName" => {
                let opt = |g: Option<NameComponents>| g.filter(|g| !g.is_empty());
                return Ok(PersonName {
                    alphabetic: groups[0]
                        .clone()
                        .filter(|g| !g.is_empty())
                        .unwrap_or_default(),
                    ideographic: opt(groups[1].clone()),
                    phonetic: opt(groups[2].clone()),
                });
            }
            Event::Text(e) => check_ws(&e)?,
            Event::Eof => return Err(Error::Xml(format!("{tag}: unexpected end of input"))),
            _ => {}
        }
    }
}

fn read_name_group(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    group: &[u8],
    tag: Tag,
) -> Result<NameComponents> {
    let mut out = NameComponents {
        family: String::new(),
        given: String::new(),
        middle: String::new(),
        prefix: String::new(),
        suffix: String::new(),
    };
    loop {
        match next(reader, buf)? {
            Event::Start(e) => {
                let field = match e.name().as_ref() {
                    b"FamilyName" => &mut out.family,
                    b"GivenName" => &mut out.given,
                    b"MiddleName" => &mut out.middle,
                    b"NamePrefix" => &mut out.prefix,
                    b"NameSuffix" => &mut out.suffix,
                    other => {
                        return Err(Error::Xml(format!(
                            "{tag}: unexpected <{}>",
                            String::from_utf8_lossy(other)
                        )));
                    }
                };
                let name = e.name().as_ref().to_vec();
                *field = leaf_text(reader, buf, &name, tag)?;
            }
            Event::Empty(e) => match e.name().as_ref() {
                b"FamilyName" | b"GivenName" | b"MiddleName" | b"NamePrefix" | b"NameSuffix" => {}
                other => {
                    return Err(Error::Xml(format!(
                        "{tag}: unexpected <{}>",
                        String::from_utf8_lossy(other)
                    )));
                }
            },
            Event::End(e) if e.name().as_ref() == group => return Ok(out),
            Event::Text(e) => check_ws(&e)?,
            Event::Eof => return Err(Error::Xml(format!("{tag}: unexpected end of input"))),
            _ => {}
        }
    }
}

/// Assemble the value, enforcing the VR's child kind.
#[allow(clippy::too_many_arguments)]
fn build_value(
    tag: Tag,
    vr: Vr,
    texts: Vec<String>,
    names: Vec<PersonName>,
    items: Vec<DataSet>,
    binary: Option<Vec<u8>>,
    saw: &[&str],
) -> Result<Value> {
    let bad = |m: &str| Error::Xml(format!("{tag}: {m}"));
    if let Some(bytes) = binary {
        if saw.len() != 1 {
            return Err(bad("InlineBinary must be the only child"));
        }
        if vr == Vr::Sq {
            return Err(bad("InlineBinary with SQ"));
        }
        // Fragments arrive joined (as in the JSON mapping); callers
        // re-fragment for encapsulated syntaxes.
        return Ok(Value::Bytes(bytes));
    }
    if saw.is_empty() {
        return Ok(Value::Empty);
    }
    match vr {
        Vr::Sq => {
            if !texts.is_empty() || !names.is_empty() {
                return Err(bad("SQ holds Item children only"));
            }
            Ok(Value::Sequence(items))
        }
        Vr::Pn => {
            if !texts.is_empty() || !items.is_empty() {
                return Err(bad("PN holds PersonName children only"));
            }
            Ok(Value::PersonName(names))
        }
        Vr::At => {
            if !names.is_empty() || !items.is_empty() {
                return Err(bad("AT holds Value children only"));
            }
            texts
                .iter()
                .map(|s| parse_tag_key(s.trim()).map_err(|_| bad("bad AT value")))
                .collect::<Result<Vec<_>>>()
                .map(Value::Tags)
        }
        Vr::Ae
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
        | Vr::Ut => {
            if !names.is_empty() || !items.is_empty() {
                return Err(bad("text VRs hold Value children only"));
            }
            Ok(Value::Text(texts))
        }
        Vr::Us => texts
            .iter()
            .map(|s| parse_int(s, tag))
            .collect::<Result<_>>()
            .map(Value::U16),
        Vr::Ss => texts
            .iter()
            .map(|s| parse_int(s, tag))
            .collect::<Result<_>>()
            .map(Value::I16),
        Vr::Ul | Vr::Ol => texts
            .iter()
            .map(|s| parse_int(s, tag))
            .collect::<Result<_>>()
            .map(Value::U32),
        Vr::Sl => texts
            .iter()
            .map(|s| parse_int(s, tag))
            .collect::<Result<_>>()
            .map(Value::I32),
        Vr::Uv | Vr::Ov => texts
            .iter()
            .map(|s| parse_int(s, tag))
            .collect::<Result<_>>()
            .map(Value::U64),
        Vr::Sv => texts
            .iter()
            .map(|s| parse_int(s, tag))
            .collect::<Result<_>>()
            .map(Value::I64),
        Vr::Fl | Vr::Of => texts
            .iter()
            .map(|s| parse_float(s, tag))
            .collect::<Result<_>>()
            .map(Value::F32),
        Vr::Fd | Vr::Od => texts
            .iter()
            .map(|s| parse_float(s, tag))
            .collect::<Result<_>>()
            .map(Value::F64),
        Vr::Ob | Vr::Ow | Vr::Un => Err(bad("byte VRs need InlineBinary")),
        Vr::Si => Err(bad("retired SI unsupported in XML")),
    }
}

fn parse_int<T>(s: &str, tag: Tag) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    s.trim()
        .parse()
        .map_err(|e| Error::Xml(format!("{tag}: bad integer {s:?} ({e})")))
}

fn parse_float<T>(s: &str, tag: Tag) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    s.trim()
        .parse()
        .map_err(|e| Error::Xml(format!("{tag}: bad float {s:?} ({e})")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::tags;

    fn mixed() -> DataSet {
        DataSet::new()
            .with(DataElement::person_name(
                tags::PATIENT_NAME,
                vec![PersonName::parse("Yamada^Tarou=山田^太郎=やまだ^たろう")],
            ))
            .with(DataElement::texts(
                tags::PATIENT_ID,
                Vr::Lo,
                vec!["a&b<c>d".to_string(), "plain".to_string()],
            ))
            .with(DataElement::text(tags::RESCALE_SLOPE, Vr::Ds, "500"))
            .with(DataElement::u16(tags::ROWS, vec![512]))
            .with(DataElement::f32(
                tags::WAVEFORM_DATA_DISPLAY_SCALE,
                vec![0.5],
            ))
            .with(DataElement::new(
                Tag::new(0x0072, 0x0026),
                Vr::At,
                Value::Tags(vec![Tag::new(0x0028, 0x0002)]),
            ))
            .with(DataElement::bytes(
                tags::PIXEL_DATA,
                Vr::Ow,
                vec![1, 2, 3, 4],
            ))
            .with(DataElement::sequence(
                tags::REFERENCED_IMAGE_SEQUENCE,
                vec![
                    DataSet::new().with(DataElement::uid(tags::REFERENCED_SOP_CLASS_UID, "1.2.3")),
                    DataSet::new(),
                ],
            ))
            .with(DataElement::new(tags::MODALITY, Vr::Cs, Value::Empty))
    }

    #[test]
    fn round_trips_mixed_dataset() {
        let ds = mixed();
        let xml = dataset_to_xml(&ds);
        // DS keeps its wire lexical form (no float trip, unlike JSON).
        assert!(xml.contains(">500</Value>"));
        // XML-special characters are escaped.
        assert!(xml.contains("a&amp;b&lt;c&gt;d"));
        // Empty attributes self-close.
        assert!(xml.contains("<DicomAttribute tag=\"00080060\" vr=\"CS\" keyword=\"Modality\"/>"));
        let back = dataset_from_xml(&xml).unwrap();
        assert_eq!(back, ds);
    }

    #[test]
    fn person_name_component_groups() {
        let ds = DataSet::new().with(DataElement::person_name(
            tags::PATIENT_NAME,
            vec![PersonName::parse(
                "Doe^John^Q^DR^JR=山田^太郎=やまだ^たろう",
            )],
        ));
        let xml = dataset_to_xml(&ds);
        for probe in [
            "<Alphabetic>",
            "<FamilyName>Doe</FamilyName>",
            "<GivenName>John</GivenName>",
            "<MiddleName>Q</MiddleName>",
            "<NamePrefix>DR</NamePrefix>",
            "<NameSuffix>JR</NameSuffix>",
            "<Ideographic>",
            "<FamilyName>山田</FamilyName>",
            "<Phonetic>",
        ] {
            assert!(xml.contains(probe), "missing {probe}");
        }
        let back = dataset_from_xml(&xml).unwrap();
        assert_eq!(back, ds);
    }

    #[test]
    fn rejects_bad_inputs() {
        let wrap = |body: &str| format!("<NativeDicomModel>{body}</NativeDicomModel>");
        // BulkData references are refused.
        let bad = wrap(
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData uri="http://x/bulk"/></DicomAttribute>"#,
        );
        assert!(dataset_from_xml(&bad).is_err());
        // Mistyped numbers are refused.
        let bad = wrap(
            r#"<DicomAttribute tag="00280010" vr="US"><Value number="1">abc</Value></DicomAttribute>"#,
        );
        assert!(dataset_from_xml(&bad).is_err());
        // Missing vr is refused.
        let bad = wrap(r#"<DicomAttribute tag="00280010"></DicomAttribute>"#);
        assert!(dataset_from_xml(&bad).is_err());
        // Bad tag is refused.
        let bad = wrap(r#"<DicomAttribute tag="nope" vr="US"/>"#);
        assert!(dataset_from_xml(&bad).is_err());
        // Wrong child kind is refused.
        let bad = wrap(
            r#"<DicomAttribute tag="00081140" vr="SQ"><Value number="1">x</Value></DicomAttribute>"#,
        );
        assert!(dataset_from_xml(&bad).is_err());
        // Missing root is refused.
        assert!(dataset_from_xml("<Nothing/>").is_err());
    }

    #[test]
    fn liberal_parse_accepts_minimal() {
        // No keyword, no numbers, no declaration: still parses.
        let xml = "<NativeDicomModel><DicomAttribute tag=\"00100010\" vr=\"PN\">\
            <PersonName><Alphabetic><FamilyName>Doe</FamilyName></Alphabetic></PersonName>\
            </DicomAttribute>\
            <DicomAttribute tag=\"00280010\" vr=\"US\"><Value>512</Value></DicomAttribute>\
            </NativeDicomModel>";
        let ds = dataset_from_xml(xml).unwrap();
        assert_eq!(
            ds.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            "Doe"
        );
        assert_eq!(ds.uint16(tags::ROWS), Some(512));
    }
}
