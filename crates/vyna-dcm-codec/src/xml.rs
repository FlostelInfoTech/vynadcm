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
//! - `<BulkData>` references are refused by [`dataset_from_xml`];
//!   [`dataset_from_xml_with_resolver`] resolves them through a
//!   caller-supplied fetch callback instead.
//!   DCMTK's proprietary `<element>` format is out of scope.

use base64::Engine;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use vyna_dcm_core::{DataElement, DataSet, NameComponents, PersonName, Tag, Value, Vr, lookup};

use crate::json::{allows_inline_binary, parse_tag_key, tag_key};
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
        Value::F32(v) => write_numbers(out, &pad, v.iter().map(|f| xsd_float(f64::from(*f)))),
        Value::F64(v) => write_numbers(out, &pad, v.iter().map(|f| xsd_float(*f))),
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

/// XSD float lexical form: `INF`/`-INF`/`NaN` for non-finites
/// (Rust's `inf`/`-inf`/`NaN` spellings are outside the schema space).
fn xsd_float(f: f64) -> String {
    if f.is_finite() {
        f.to_string()
    } else if f.is_nan() {
        "NaN".to_string()
    } else if f.is_sign_positive() {
        "INF".to_string()
    } else {
        "-INF".to_string()
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

/// Escape text content.
///
/// Control characters illegal in XML 1.0 (NUL, form feed, and the
/// rest of C0 outside tab/LF/CR) become U+FFFD: stored text values
/// arrive from the wire and may carry anything.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\u{09}' | '\u{0A}' | '\u{0D}' => out.push(c),
            '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}' => {
                out.push(c);
            }
            _ => out.push('\u{FFFD}'),
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
    dataset_from_xml_inner(xml, None)
}

/// Parse PS3.19 XML, resolving `<BulkData uri="..."/>` elements
/// through `resolve` (called with the verbatim `uri`).
///
/// Without a resolver ([`dataset_from_xml`]) bulk references are
/// refused; with one, missing/empty `uri` attributes, resolver
/// failures, and bulk children under non-bulk VRs are errors.
pub fn dataset_from_xml_with_resolver(
    xml: &str,
    mut resolve: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<DataSet> {
    dataset_from_xml_inner(xml, Some(&mut resolve))
}

#[allow(clippy::type_complexity)] // spelled-out trait object keeps object/ref lifetimes unified for inference
fn dataset_from_xml_inner(
    xml: &str,
    resolve: Option<&mut dyn FnMut(&str) -> Result<Vec<u8>>>,
) -> Result<DataSet> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match next(&mut reader, &mut buf)? {
            Event::Start(e) if is(&e, b"NativeDicomModel") => {
                return read_attributes(&mut reader, &mut buf, b"NativeDicomModel", 0, resolve);
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

/// Maximum XML nesting depth (stack-exhaustion guard).
const MAX_XML_DEPTH: usize = 64;

/// Read `DicomAttribute` children until `</end>`.
/// Short reborrow of the bulk resolver for one recursive call
/// (`Option::as_deref_mut` keeps the original lifetime on
/// `&mut dyn` and will not reborrow inside the read loops).
// Named lifetimes and the spelled-out trait object are load-bearing: the
// elided/aliased equivalents fail E0106 / lifetime inference (rustc), so the
// matching clippy lints are silenced here rather than "fixed".
#[allow(clippy::needless_lifetimes, clippy::type_complexity)]
fn reborrow<'a, 'b>(
    resolve: &'a mut Option<&'b mut dyn FnMut(&str) -> Result<Vec<u8>>>,
) -> Option<&'a mut dyn FnMut(&str) -> Result<Vec<u8>>> {
    match resolve {
        Some(r) => Some(&mut **r),
        None => None,
    }
}

#[allow(clippy::type_complexity)] // as above: aliasing the resolver breaks lifetime inference at call sites
fn read_attributes(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    end: &[u8],
    depth: usize,
    mut resolve: Option<&mut dyn FnMut(&str) -> Result<Vec<u8>>>,
) -> Result<DataSet> {
    if depth > MAX_XML_DEPTH {
        return Err(Error::Xml("nesting too deep".into()));
    }
    let mut ds = DataSet::new();
    loop {
        match next(reader, buf)? {
            Event::Start(e) if is(&e, b"DicomAttribute") => {
                let (tag, vr) = attribute_head(&e)?;
                insert_unique(
                    &mut ds,
                    read_attribute_body(reader, buf, tag, vr, depth, reborrow(&mut resolve))?,
                )?;
            }
            Event::Empty(e) if is(&e, b"DicomAttribute") => {
                let (tag, vr) = attribute_head(&e)?;
                insert_unique(&mut ds, DataElement::new(tag, vr, Value::Empty))?;
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

/// Insert, rejecting duplicate tags (last-win would silently drop data).
fn insert_unique(ds: &mut DataSet, el: DataElement) -> Result<()> {
    if ds.contains(el.tag) {
        return Err(Error::Xml(format!("{}: duplicate attribute", el.tag)));
    }
    ds.insert(el);
    Ok(())
}

/// Required `tag` + `vr` from a `DicomAttribute` open tag.
fn attribute_head(e: &BytesStart<'_>) -> Result<(Tag, Vr)> {
    let mut tag = None;
    let mut vr = None;
    for attr in e.attributes() {
        let attr = attr.map_err(|e| Error::Xml(e.to_string()))?;
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
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
#[allow(clippy::type_complexity)] // as above
fn read_attribute_body(
    reader: &mut Reader<&[u8]>,
    buf: &mut Vec<u8>,
    tag: Tag,
    vr: Vr,
    depth: usize,
    mut resolve: Option<&mut dyn FnMut(&str) -> Result<Vec<u8>>>,
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
                items.push(read_attributes(
                    reader,
                    buf,
                    b"Item",
                    depth + 1,
                    reborrow(&mut resolve),
                )?);
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
            Event::Start(e) if is(&e, b"BulkData") => {
                let uri = bulk_data_uri(&e, tag)?;
                let text = leaf_text(reader, buf, b"BulkData", tag)?;
                if !text.chars().all(|c| c.is_whitespace()) {
                    return Err(Error::Xml(format!("{tag}: BulkData must be empty")));
                }
                binary = Some(resolve_bulk(reborrow(&mut resolve), &uri, tag)?);
                saw.push("BulkData");
            }
            Event::Empty(e) if is(&e, b"BulkData") => {
                let uri = bulk_data_uri(&e, tag)?;
                binary = Some(resolve_bulk(reborrow(&mut resolve), &uri, tag)?);
                saw.push("BulkData");
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
            Event::Text(e) => out.push_str(
                &e.xml_content(XmlVersion::Implicit1_0)
                    .map_err(|e| Error::Xml(e.to_string()))?,
            ),
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

/// Required `uri` attribute of a `<BulkData>` element.
fn bulk_data_uri(e: &quick_xml::events::BytesStart<'_>, tag: Tag) -> Result<String> {
    for attr in e.attributes() {
        let attr = attr.map_err(|e| Error::Xml(e.to_string()))?;
        if attr.key.as_ref() == b"uri" {
            let uri = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|e| Error::Xml(e.to_string()))?
                .into_owned();
            if uri.is_empty() {
                break;
            }
            return Ok(uri);
        }
    }
    Err(Error::Xml(format!("{tag}: BulkData without uri")))
}

/// Fetch one bulk reference: without a resolver this is the historic
/// refusal; with one, resolver failures propagate verbatim.
#[allow(clippy::type_complexity)] // as above
fn resolve_bulk(
    resolve: Option<&mut dyn FnMut(&str) -> Result<Vec<u8>>>,
    uri: &str,
    tag: Tag,
) -> Result<Vec<u8>> {
    match resolve {
        Some(resolve) => resolve(uri),
        None => Err(Error::Xml(format!(
            "{tag}: BulkData references are not resolved"
        ))),
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
        let kind = saw.first().copied().unwrap_or("InlineBinary");
        if saw.len() != 1 {
            return Err(bad(&format!("{kind} must be the only child")));
        }
        if !allows_inline_binary(vr) {
            return Err(bad(&format!("{kind} not allowed for this VR")));
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
        Vr::Us
        | Vr::Ss
        | Vr::Ul
        | Vr::Ol
        | Vr::Sl
        | Vr::Uv
        | Vr::Ov
        | Vr::Sv
        | Vr::Fl
        | Vr::Of
        | Vr::Fd
        | Vr::Od => {
            // Numeric VRs hold Value children only: Item/PersonName
            // children must error, not silently vanish.
            if !names.is_empty() || !items.is_empty() {
                return Err(bad("numeric VRs hold Value children only"));
            }
            match vr {
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
                _ => texts
                    .iter()
                    .map(|s| parse_float(s, tag))
                    .collect::<Result<_>>()
                    .map(Value::F64),
            }
        }
        Vr::Ob | Vr::Ow | Vr::Un => Err(bad("byte VRs need InlineBinary")),
        Vr::Si => Err(bad("retired SI unsupported in XML")),
        _ => Err(Error::Xml(format!("{tag}: unsupported VR {vr:?} in XML"))),
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

    #[test]
    fn non_finite_floats_use_xsd_spellings() {
        let mut ds = DataSet::new();
        ds.insert(DataElement::f32(
            tags::SELECTOR_FL_VALUE,
            vec![f32::INFINITY, f32::NEG_INFINITY, f32::NAN],
        ));
        let xml = dataset_to_xml(&ds);
        assert!(xml.contains(">INF<"), "{xml}");
        assert!(xml.contains(">-INF<"), "{xml}");
        assert!(xml.contains(">NaN<"), "{xml}");
        assert!(!xml.contains(">inf<"), "{xml}");
        // Import still reads them (Rust parses INF case-insensitively).
        let back = dataset_from_xml(&xml).unwrap();
        match &back.get(tags::SELECTOR_FL_VALUE).unwrap().value {
            Value::F32(v) => {
                assert_eq!(v.len(), 3);
                assert!(v[0].is_infinite() && v[0].is_sign_positive());
                assert!(v[1].is_infinite() && v[1].is_sign_negative());
                assert!(v[2].is_nan());
            }
            other => panic!("{other:?}"),
        }
    }

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
    fn retired_si_bytes_round_trip() {
        let mut ds = DataSet::new();
        ds.put_bytes(Tag::new(0x0019, 0x1001), Vr::Si, vec![7, 7, 7, 7]);
        let back = dataset_from_xml(&dataset_to_xml(&ds)).unwrap();
        assert_eq!(back, ds);
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
    fn control_chars_become_replacement_char() {
        // NUL, form feed and other C0 controls are illegal in XML 1.0
        // and must not reach the document, even when stored text
        // values carry them.
        let ds = DataSet::new().with(DataElement::text(tags::PATIENT_ID, Vr::Lo, "A\0B\x0C\x01"));
        let xml = dataset_to_xml(&ds);
        assert!(!xml.contains('\0'), "{xml}");
        assert!(!xml.contains('\x0C'), "{xml}");
        assert!(!xml.contains('\x01'), "{xml}");
        assert!(xml.contains('\u{FFFD}'), "{xml}");
        // The replacement output still parses.
        let back = dataset_from_xml(&xml).unwrap();
        assert_eq!(
            back.string(tags::PATIENT_ID),
            Some("A\u{FFFD}B\u{FFFD}\u{FFFD}")
        );
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
        // Item/PersonName children under a numeric VR are refused
        // (not silently dropped).
        let bad = wrap(
            r#"<DicomAttribute tag="00280010" vr="US"><Value number="1">1</Value><Item number="1"/></DicomAttribute>"#,
        );
        assert!(dataset_from_xml(&bad).is_err());
        let bad = wrap(r#"<DicomAttribute tag="00280010" vr="US"><PersonName/></DicomAttribute>"#);
        assert!(dataset_from_xml(&bad).is_err());
        // InlineBinary under a non-bulk VR is refused.
        let bad = wrap(
            r#"<DicomAttribute tag="00280010" vr="US"><InlineBinary>AA==</InlineBinary></DicomAttribute>"#,
        );
        assert!(dataset_from_xml(&bad).is_err());
        // Duplicate attributes are refused (last-win would drop data).
        let bad = wrap(concat!(
            r#"<DicomAttribute tag="00280010" vr="US"><Value number="1">1</Value></DicomAttribute>"#,
            r#"<DicomAttribute tag="00280010" vr="US"><Value number="1">2</Value></DicomAttribute>"#,
        ));
        let err = dataset_from_xml(&bad).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn bulk_data_resolves_through_callback() {
        use std::collections::HashMap;
        let wrap = |body: &str| format!("<NativeDicomModel>{body}</NativeDicomModel>");
        let mut store = HashMap::new();
        store.insert("px.bin".to_string(), vec![9u8; 16]);
        store.insert("nested/frame.bin".to_string(), vec![7u8; 4]);
        let mut resolve = |uri: &str| {
            store
                .get(uri)
                .cloned()
                .ok_or_else(|| crate::Error::Xml(format!("no such bulk object: {uri}")))
        };
        // Top-level bulk attribute lands as bytes.
        let xml = wrap(
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData uri="px.bin"/></DicomAttribute>"#,
        );
        let ds = dataset_from_xml_with_resolver(&xml, &mut resolve).unwrap();
        assert_eq!(
            ds.get(vyna_dcm_core::tags::PIXEL_DATA).unwrap().value,
            vyna_dcm_core::Value::Bytes(vec![9u8; 16])
        );
        // Bulk inside a sequence item resolves too (threading check).
        let xml = wrap(concat!(
            r#"<DicomAttribute tag="00081140" vr="SQ"><Item number="1">"#,
            r#"<DicomAttribute tag="7FE00010" vr="OB"><BulkData uri="nested/frame.bin"></BulkData></DicomAttribute>"#,
            r#"</Item></DicomAttribute>"#,
        ));
        let ds = dataset_from_xml_with_resolver(&xml, &mut resolve).unwrap();
        let items = match &ds
            .get(vyna_dcm_core::tags::REFERENCED_IMAGE_SEQUENCE)
            .unwrap()
            .value
        {
            vyna_dcm_core::Value::Sequence(items) => items.clone(),
            other => panic!("expected sequence, got {other:?}"),
        };
        assert_eq!(
            items[0].get(vyna_dcm_core::tags::PIXEL_DATA).unwrap().value,
            vyna_dcm_core::Value::Bytes(vec![7u8; 4])
        );
        // Unknown uri: resolver failure propagates.
        let xml = wrap(
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData uri="missing.bin"/></DicomAttribute>"#,
        );
        let err = dataset_from_xml_with_resolver(&xml, &mut resolve).unwrap_err();
        assert!(err.to_string().contains("no such bulk object"), "{err}");
        // Missing/empty uri never reaches the resolver.
        for body in [
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData/></DicomAttribute>"#,
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData uri=""/></DicomAttribute>"#,
        ] {
            let err = dataset_from_xml_with_resolver(&wrap(body), &mut resolve).unwrap_err();
            assert!(err.to_string().contains("without uri"), "{err}");
        }
        // Bulk under a non-bulk VR is refused, like InlineBinary.
        let xml = wrap(
            r#"<DicomAttribute tag="00280010" vr="US"><BulkData uri="px.bin"/></DicomAttribute>"#,
        );
        let err = dataset_from_xml_with_resolver(&xml, &mut resolve).unwrap_err();
        assert!(err.to_string().contains("not allowed for this VR"), "{err}");
        // Bulk with a Value sibling is refused (single-child rule).
        let xml = wrap(
            r#"<DicomAttribute tag="7FE00010" vr="OW"><Value number="1">1</Value><BulkData uri="px.bin"/></DicomAttribute>"#,
        );
        let err = dataset_from_xml_with_resolver(&xml, &mut resolve).unwrap_err();
        assert!(err.to_string().contains("must be the only child"), "{err}");
        // Non-empty Start/End form is refused.
        let xml = wrap(
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData uri="px.bin">junk</BulkData></DicomAttribute>"#,
        );
        let err = dataset_from_xml_with_resolver(&xml, &mut resolve).unwrap_err();
        assert!(err.to_string().contains("must be empty"), "{err}");
        // Without a resolver the historic refusal stands.
        let xml = wrap(
            r#"<DicomAttribute tag="7FE00010" vr="OW"><BulkData uri="px.bin"/></DicomAttribute>"#,
        );
        let err = dataset_from_xml(&xml).unwrap_err();
        assert!(err.to_string().contains("not resolved"), "{err}");
    }

    #[test]
    fn rejects_deeply_nested_items() {
        let mut body = String::from(
            r#"<DicomAttribute tag="00100020" vr="LO"><Value number="1">x</Value></DicomAttribute>"#,
        );
        for n in 0..(MAX_XML_DEPTH + 8) {
            body = format!(
                r#"<DicomAttribute tag="00081140" vr="SQ"><Item number="{n}">{body}</Item></DicomAttribute>"#
            );
        }
        let xml = format!("<NativeDicomModel>{body}</NativeDicomModel>");
        assert!(dataset_from_xml(&xml).is_err());
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
