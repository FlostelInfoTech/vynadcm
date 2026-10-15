//! DICOM JSON Model (NEMA PS3.18 Annex F).
//!
//! - Tags become 8-uppercase-hex keys (`"00100010"`).
//! - Each value object carries `"vr"` plus `"Value"` (or `"InlineBinary"`
//!   for byte VRs, base64). Encapsulated data uses the compact joined
//!   form (`"InlineBinary"` plus `"BasicOffsetTable"`).
//! - `PN` values become `{"Alphabetic": ..., "Ideographic": ...,
//!   "Phonetic": ...}` objects (each group optional per Annex F); `AT`
//!   values are 8-hex-digit strings; `DS`/`IS` accept numbers or strings
//!   on input.
//! - Non-finite floats have no Annex F spelling: `FL`/`FD`/`OF`/`OD`
//!   `NaN`/`±Infinity` encode as the strings `"NaN"`/`"Infinity"`/
//!   `"-Infinity"` (and parse back), instead of silently becoming null.

use base64::Engine;
use serde_json::{Map, Value as Json, json};
use vyna_dcm_core::{DataElement, DataSet, FragmentSequence, PersonName, Tag, Value, Vr};

use crate::{Error, Result};

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Maximum JSON nesting depth (stack-exhaustion guard).
const MAX_JSON_DEPTH: usize = 64;

/// Convert a dataset to a DICOM JSON object.
#[must_use]
pub fn dataset_to_json(ds: &DataSet) -> Json {
    let mut map = Map::new();
    for (tag, el) in ds.iter() {
        map.insert(tag_key(tag), element_to_json(el));
    }
    Json::Object(map)
}

/// Parse a DICOM JSON object back into a dataset.
pub fn dataset_from_json(v: &Json) -> Result<DataSet> {
    dataset_from_json_depth(v, 0)
}

fn dataset_from_json_depth(v: &Json, depth: usize) -> Result<DataSet> {
    if depth > MAX_JSON_DEPTH {
        return Err(Error::Json("nesting too deep".into()));
    }
    let map = v
        .as_object()
        .ok_or_else(|| Error::Json("top level must be an object".into()))?;
    let mut ds = DataSet::new();
    for (key, obj) in map {
        let tag = parse_tag_key(key)?;
        ds.insert(element_from_json_depth(tag, obj, depth)?);
    }
    Ok(ds)
}

pub(crate) fn tag_key(tag: Tag) -> String {
    format!("{:04X}{:04X}", tag.group(), tag.element())
}

pub(crate) fn parse_tag_key(key: &str) -> Result<Tag> {
    if key.len() != 8 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Json(format!("bad tag key {key:?}")));
    }
    let g = u16::from_str_radix(&key[0..4], 16).unwrap();
    let e = u16::from_str_radix(&key[4..8], 16).unwrap();
    Ok(Tag::new(g, e))
}

fn element_to_json(el: &DataElement) -> Json {
    let mut obj = Map::new();
    obj.insert("vr".into(), Json::String(el.vr.as_str().to_string()));
    let _: Option<Json> = match &el.value {
        Value::Empty => None,
        Value::Text(v) => {
            let arr = v.iter().map(|s| text_json(el.vr, s)).collect();
            obj.insert("Value".into(), Json::Array(arr))
        }
        Value::PersonName(names) => {
            let arr = names
                .iter()
                .map(|n| {
                    let mut o = Map::new();
                    o.insert(
                        "Alphabetic".into(),
                        Json::String(n.alphabetic.to_dicom_string()),
                    );
                    if let Some(g) = &n.ideographic {
                        o.insert("Ideographic".into(), Json::String(g.to_dicom_string()));
                    }
                    if let Some(g) = &n.phonetic {
                        o.insert("Phonetic".into(), Json::String(g.to_dicom_string()));
                    }
                    Json::Object(o)
                })
                .collect();
            obj.insert("Value".into(), Json::Array(arr))
        }
        Value::Tags(tags) => {
            let arr = tags.iter().map(|t| Json::String(tag_key(*t))).collect();
            obj.insert("Value".into(), Json::Array(arr))
        }
        Value::U16(v) => obj.insert("Value".into(), nums(v.iter().map(|x| *x as u64))),
        Value::I16(v) => obj.insert("Value".into(), inums(v.iter().map(|x| *x as i64))),
        Value::U32(v) => obj.insert("Value".into(), nums(v.iter().map(|x| *x as u64))),
        Value::I32(v) => obj.insert("Value".into(), inums(v.iter().map(|x| *x as i64))),
        Value::U64(v) => obj.insert("Value".into(), nums(v.iter().copied())),
        Value::I64(v) => obj.insert("Value".into(), inums(v.iter().copied())),
        Value::F32(v) => obj.insert("Value".into(), floats(v.iter().map(|x| *x as f64))),
        Value::F64(v) => obj.insert("Value".into(), floats(v.iter().copied())),
        Value::Bytes(b) => obj.insert("InlineBinary".into(), Json::String(B64.encode(b))),
        Value::Sequence(items) => {
            let arr = items.iter().map(dataset_to_json).collect();
            obj.insert("Value".into(), Json::Array(arr))
        }
        Value::Fragments(f) => {
            // Annex F: one InlineBinary per fragment is allowed via
            // DataFragment; we emit the compact joined form plus offsets.
            obj.insert("InlineBinary".into(), Json::String(B64.encode(f.joined())));
            if !f.basic_offset_table.is_empty() {
                obj.insert(
                    "BasicOffsetTable".into(),
                    Json::Array(f.basic_offset_table.iter().map(|o| json!(o)).collect()),
                );
            }
            None
        }
    };
    Json::Object(obj)
}

/// `DS`/`IS`/numeric-ish text becomes JSON numbers when parseable.
///
/// `DS` spellings that parse to non-finite doubles (`"NaN"`,
/// `"inf"`, ...) stay strings: `serde_json` would render them as
/// null, silently corrupting the export.
fn text_json(vr: Vr, s: &str) -> Json {
    match vr {
        Vr::Ds => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(Json::from)
            .unwrap_or_else(|| Json::String(s.into())),
        Vr::Is => s
            .trim()
            .parse::<i64>()
            .map(Json::from)
            .unwrap_or_else(|_| Json::String(s.into())),
        _ => Json::String(s.into()),
    }
}

fn nums(it: impl Iterator<Item = u64>) -> Json {
    Json::Array(it.map(Json::from).collect())
}
fn inums(it: impl Iterator<Item = i64>) -> Json {
    Json::Array(it.map(Json::from).collect())
}
fn floats(it: impl Iterator<Item = f64>) -> Json {
    // `Json::from` maps non-finite doubles to null; spell them
    // explicitly so the export round-trips (see module docs).
    Json::Array(
        it.map(|f| {
            if f.is_finite() {
                Json::from(f)
            } else if f.is_nan() {
                Json::String("NaN".into())
            } else if f.is_sign_positive() {
                Json::String("Infinity".into())
            } else {
                Json::String("-Infinity".into())
            }
        })
        .collect(),
    )
}

/// VRs allowed to carry `InlineBinary`: the byte VRs the encoder
/// accepts as [`Value::Bytes`](vyna_dcm_core::Value::Bytes), plus
/// retired `SI` (export writes `SI` bytes as `InlineBinary`, so
/// import must accept it back for round-trip symmetry). Text VRs
/// (`LT ST UT UC UR`) are refused: bytes cannot become text without
/// the dataset's character set, and the encoder rejects `Bytes`
/// under text VRs.
pub(crate) fn allows_inline_binary(vr: Vr) -> bool {
    matches!(
        vr,
        Vr::Ob | Vr::Od | Vr::Of | Vr::Ol | Vr::Ov | Vr::Ow | Vr::Si | Vr::Un
    )
}

/// Re-split joined `InlineBinary` bytes into per-frame fragments using
/// the `BasicOffsetTable` the exporter writes. The table must start at
/// 0, increase strictly, and stay within the decoded bytes; anything
/// else errors (no silent fallback to framing-less bytes).
fn split_fragments(tag: Tag, bytes: &[u8], table: &Json) -> Result<Value> {
    let bad = |m: &str| Error::Json(format!("{tag}: bad BasicOffsetTable ({m})"));
    let arr = table.as_array().ok_or_else(|| bad("not an array"))?;
    if arr.is_empty() {
        return Err(bad("empty"));
    }
    let mut offsets = Vec::with_capacity(arr.len());
    for o in arr {
        let n = o.as_u64().ok_or_else(|| bad("not uints"))?;
        let n = usize::try_from(n).map_err(|_| bad("offset out of range"))?;
        if n > bytes.len() {
            return Err(bad("offset past end"));
        }
        if offsets.last().is_some_and(|prev| n <= *prev) {
            return Err(bad("not strictly increasing"));
        }
        offsets.push(n);
    }
    if offsets[0] != 0 {
        return Err(bad("must start at 0"));
    }
    let mut fragments = Vec::with_capacity(offsets.len());
    for (i, start) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).copied().unwrap_or(bytes.len());
        fragments.push(bytes[*start..end].to_vec());
    }
    let mut table = Vec::with_capacity(offsets.len());
    for o in &offsets {
        table.push(u32::try_from(*o).map_err(|_| bad("offset out of range"))?);
    }
    Ok(Value::Fragments(FragmentSequence {
        basic_offset_table: table,
        fragments,
    }))
}

fn element_from_json_depth(tag: Tag, obj: &Json, depth: usize) -> Result<DataElement> {
    let obj = obj
        .as_object()
        .ok_or_else(|| Error::Json(format!("{tag}: not an object")))?;
    let vr: Vr = obj
        .get("vr")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::Json(format!("{tag}: missing vr")))?
        .parse()
        .map_err(|_| Error::Json(format!("{tag}: bad vr")))?;

    if let Some(b64) = obj.get("InlineBinary").and_then(Json::as_str) {
        // Annex F carries one form per value object (mirrors the XML rule).
        if obj.contains_key("Value") {
            return Err(Error::Json(format!(
                "{tag}: InlineBinary and Value are mutually exclusive"
            )));
        }
        let bytes = B64.decode(b64).map_err(|e| Error::Base64(e.to_string()))?;
        if !allows_inline_binary(vr) {
            return Err(Error::Json(format!(
                "{tag}: InlineBinary not allowed for {vr}"
            )));
        }
        // Fragment framing round-trips: the exporter writes joined bytes
        // plus the table, so a present table re-splits them into frames.
        // Other VRs ignore the member (bytes, as before).
        if matches!(vr, Vr::Ob | Vr::Ow)
            && let Some(table) = obj.get("BasicOffsetTable")
        {
            let fragments = split_fragments(tag, &bytes, table)?;
            return Ok(DataElement::new(tag, vr, fragments));
        }
        return Ok(DataElement::new(tag, vr, Value::Bytes(bytes)));
    }

    let values = match obj.get("Value") {
        None => return Ok(DataElement::new(tag, vr, Value::Empty)),
        Some(Json::Array(a)) => a,
        Some(_) => return Err(Error::Json(format!("{tag}: Value must be an array"))),
    };

    let bad = |m: &str| Error::Json(format!("{tag}: {m}"));
    let value = match vr {
        Vr::Sq => {
            let mut items = Vec::with_capacity(values.len());
            for v in values {
                items.push(dataset_from_json_depth(v, depth + 1)?);
            }
            Value::Sequence(items)
        }
        Vr::Pn => {
            let mut names = Vec::with_capacity(values.len());
            for v in values {
                match v {
                    Json::Object(o) => {
                        // Annex F: Alphabetic/Ideographic/Phonetic are
                        // each optional; rebuild the `=`-joined form.
                        let alpha = o.get("Alphabetic").and_then(Json::as_str).unwrap_or("");
                        let ideo = o.get("Ideographic").and_then(Json::as_str);
                        let phon = o.get("Phonetic").and_then(Json::as_str);
                        let mut s = alpha.to_string();
                        if ideo.is_some() || phon.is_some() {
                            s.push('=');
                            if let Some(g) = ideo {
                                s.push_str(g);
                            }
                        }
                        if let Some(g) = phon {
                            s.push('=');
                            s.push_str(g);
                        }
                        names.push(PersonName::parse(&s));
                    }
                    Json::String(s) => names.push(PersonName::parse(s)),
                    _ => return Err(bad("bad PN value")),
                }
            }
            Value::PersonName(names)
        }
        Vr::At => {
            let mut tags = Vec::with_capacity(values.len());
            for v in values {
                let s = v.as_str().ok_or_else(|| bad("AT values must be strings"))?;
                tags.push(parse_tag_key(s)?);
            }
            Value::Tags(tags)
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
            let mut out = Vec::with_capacity(values.len());
            for item in values {
                match item {
                    Json::String(s) => out.push(s.clone()),
                    Json::Number(n) => out.push(n.to_string()),
                    Json::Null => {}
                    _ => return Err(bad("bad text value")),
                }
            }
            Value::Text(out)
        }
        Vr::Us => Value::U16(ints(values, tag, u16::try_from)?),
        Vr::Ss => Value::I16(sints(values, tag, i16::try_from)?),
        Vr::Ul | Vr::Ol => Value::U32(ints(values, tag, u32::try_from)?),
        Vr::Sl => Value::I32(sints(values, tag, i32::try_from)?),
        Vr::Uv | Vr::Ov => Value::U64(ints(values, tag, Ok)?),
        Vr::Sv => Value::I64(sints(values, tag, Ok)?),
        Vr::Fl | Vr::Of => Value::F32(json_floats(values, tag)?),
        Vr::Fd | Vr::Od => Value::F64(json_doubles(values, tag)?),
        Vr::Ob | Vr::Ow | Vr::Un => return Err(bad("byte VRs need InlineBinary")),
        Vr::Si => return Err(bad("retired SI unsupported in JSON")),
        _ => {
            return Err(Error::Json(format!("{tag}: unsupported VR {vr:?} in JSON")));
        }
    };
    Ok(DataElement::new(tag, vr, value))
}

fn ints<T, F>(values: &[Json], tag: Tag, conv: F) -> Result<Vec<T>>
where
    F: Fn(u64) -> std::result::Result<T, std::num::TryFromIntError>,
{
    values
        .iter()
        .map(|v| {
            let n = v
                .as_u64()
                .ok_or_else(|| Error::Json(format!("{tag}: expected uint")))?;
            conv(n).map_err(|_| Error::Json(format!("{tag}: uint out of range")))
        })
        .collect()
}

fn sints<T, F>(values: &[Json], tag: Tag, conv: F) -> Result<Vec<T>>
where
    F: Fn(i64) -> std::result::Result<T, std::num::TryFromIntError>,
{
    values
        .iter()
        .map(|v| {
            let n = v
                .as_i64()
                .ok_or_else(|| Error::Json(format!("{tag}: expected int")))?;
            conv(n).map_err(|_| Error::Json(format!("{tag}: int out of range")))
        })
        .collect()
}

fn json_floats(values: &[Json], tag: Tag) -> Result<Vec<f32>> {
    values
        .iter()
        .map(|v| {
            if let Some(f) = nonfinite_token(v) {
                return Ok(f as f32);
            }
            v.as_f64()
                .ok_or_else(|| Error::Json(format!("{tag}: expected number")))
                .map(|f| f as f32)
        })
        .collect()
}

fn json_doubles(values: &[Json], tag: Tag) -> Result<Vec<f64>> {
    values
        .iter()
        .map(|v| {
            if let Some(f) = nonfinite_token(v) {
                return Ok(f);
            }
            v.as_f64()
                .ok_or_else(|| Error::Json(format!("{tag}: expected number")))
        })
        .collect()
}

/// Explicit non-finite spellings emitted by [`floats`].
fn nonfinite_token(v: &Json) -> Option<f64> {
    match v.as_str()? {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::tags;

    #[test]
    fn inline_binary_and_value_are_mutually_exclusive() {
        let v: Json =
            serde_json::from_str(r#"{"7FE00010":{"vr":"OW","InlineBinary":"AQI=","Value":[1,2]}}"#)
                .unwrap();
        let err = dataset_from_json(&v).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn basic_offset_table_resplits_fragments() {
        // Export writes joined bytes + table; import must restore framing.
        let mut ds = DataSet::new();
        ds.insert(DataElement::new(
            tags::PIXEL_DATA,
            Vr::Ow,
            Value::Fragments(FragmentSequence {
                basic_offset_table: vec![0, 4],
                fragments: vec![vec![1u8, 2, 3, 4], vec![5u8, 6]],
            }),
        ));
        let json = dataset_to_json(&ds);
        let back = dataset_from_json(&json).unwrap();
        match &back.get(tags::PIXEL_DATA).unwrap().value {
            Value::Fragments(f) => {
                assert_eq!(f.basic_offset_table, vec![0, 4]);
                assert_eq!(f.fragments, vec![vec![1u8, 2, 3, 4], vec![5u8, 6]]);
            }
            other => panic!("{other:?}"),
        }
        // Malformed tables error rather than silently dropping framing.
        for table in ["[]", "[4]", "[0,0]", "[0,99]", "[\"x\"]", "{}"] {
            let v: Json = serde_json::from_str(&format!(
                "{{\"7FE00010\":{{\"vr\":\"OW\",\"InlineBinary\":\"AQI=\",\"BasicOffsetTable\":{table}}}}}"
            ))
            .unwrap();
            assert!(dataset_from_json(&v).is_err(), "{table}");
        }
        // Other InlineBinary-capable VRs ignore the member (bytes).
        let v: Json = serde_json::from_str(
            r#"{"7FE00008":{"vr":"OD","InlineBinary":"AQI=","BasicOffsetTable":[0,1]}}"#,
        )
        .unwrap();
        let back = dataset_from_json(&v).unwrap();
        assert!(matches!(
            back.get(Tag::new(0x7FE0, 0x0008)).unwrap().value,
            Value::Bytes(_)
        ));
    }

    #[test]
    fn pn_three_groups_roundtrip() {
        // Annex F: each group is its own member, all optional.
        let mut ds = DataSet::new();
        ds.put_person_name(
            tags::PATIENT_NAME,
            vec![PersonName::parse("Doe^Jane=山田^太郎=やまだ^たろう")],
        );
        let j = dataset_to_json(&ds);
        let v = &j["00100010"]["Value"][0];
        assert_eq!(v["Alphabetic"], json!("Doe^Jane"));
        assert_eq!(v["Ideographic"], json!("山田^太郎"));
        assert_eq!(v["Phonetic"], json!("やまだ^たろう"));
        let back = dataset_from_json(&j).unwrap();
        assert_eq!(
            back.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            "Doe^Jane=山田^太郎=やまだ^たろう"
        );
        // Missing groups stay missing (no phantom `=` joins).
        let mut ds = DataSet::new();
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("Doe^Jane")]);
        let j = dataset_to_json(&ds);
        let v = &j["00100010"]["Value"][0];
        assert_eq!(v["Alphabetic"], json!("Doe^Jane"));
        assert!(v.get("Ideographic").is_none());
        assert!(v.get("Phonetic").is_none());
        let back = dataset_from_json(&j).unwrap();
        let name = &back.person_names(tags::PATIENT_NAME).unwrap()[0];
        assert_eq!(name.to_dicom_string(), "Doe^Jane");
        assert!(name.ideographic.is_none() && name.phonetic.is_none());
        // Phonetic without ideographic keeps its slot.
        let j: Json = serde_json::from_str(
            r#"{"00100010":{"vr":"PN","Value":[{"Alphabetic":"A","Phonetic":"C"}]}}"#,
        )
        .unwrap();
        let back = dataset_from_json(&j).unwrap();
        assert_eq!(
            back.person_names(tags::PATIENT_NAME).unwrap()[0].to_dicom_string(),
            "A==C"
        );
    }

    #[test]
    fn retired_si_bytes_round_trip() {
        let mut ds = DataSet::new();
        ds.put_bytes(Tag::new(0x0019, 0x1001), Vr::Si, vec![7, 7, 7, 7]);
        let back = dataset_from_json(&dataset_to_json(&ds)).unwrap();
        assert_eq!(back, ds);
    }

    #[test]
    fn round_trips_json_model() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("Doe^Jane")]);
        ds.put_u16_single(tags::ROWS, 512);
        // NOTE: the JSON model carries DS as numbers, so "500" normalises
        // to "500.0" through JSON (same as pydicom/dcm4che).
        ds.put_text(tags::SAMPLING_FREQUENCY, Vr::Ds, "500.0");
        ds.put_text(tags::SERIES_NUMBER, Vr::Is, "3");
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, vec![1, 2, 3, 4]);
        ds.put_sequence(
            tags::REFERENCED_IMAGE_SEQUENCE,
            vec![{
                let mut item = DataSet::new();
                item.put_uid(tags::REFERENCED_SOP_CLASS_UID, "1.2.3");
                item
            }],
        );

        let j = dataset_to_json(&ds);
        assert_eq!(j["00100020"]["Value"][0], json!("E123"));
        assert_eq!(j["00280010"]["Value"][0], json!(512));
        assert_eq!(j["003A001A"]["Value"][0], json!(500.0));
        assert_eq!(j["00100010"]["Value"][0]["Alphabetic"], json!("Doe^Jane"));
        assert!(j["7FE00010"]["InlineBinary"].is_string());

        let back = dataset_from_json(&j).unwrap();
        assert_eq!(back, ds);
    }

    #[test]
    fn rejects_bad_json() {
        assert!(dataset_from_json(&json!([1, 2])).is_err());
        assert!(dataset_from_json(&json!({"ZZZZ": {}})).is_err());
        assert!(dataset_from_json(&json!({"00100010": {"vr": "PN", "Value": [42]}})).is_err());
    }

    #[test]
    fn rejects_deeply_nested_sequences() {
        let mut v = json!({"00100020": {"vr": "LO", "Value": ["x"]}});
        for _ in 0..(MAX_JSON_DEPTH + 8) {
            v = json!({"00081140": {"vr": "SQ", "Value": [v]}});
        }
        assert!(dataset_from_json(&v).is_err());
    }

    #[test]
    fn nonfinite_floats_round_trip() {
        let tag = Tag::new(0x0019, 0x1001);
        let mut ds = DataSet::new();
        ds.insert(DataElement::new(
            tag,
            Vr::Fd,
            Value::F64(vec![1.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY]),
        ));
        let j = dataset_to_json(&ds);
        let vals = j["00191001"]["Value"].as_array().unwrap();
        assert_eq!(vals[0], json!(1.5));
        assert_eq!(vals[1], json!("NaN"));
        assert_eq!(vals[2], json!("Infinity"));
        assert_eq!(vals[3], json!("-Infinity"));
        let back = dataset_from_json(&j).unwrap();
        match &back.get(tag).unwrap().value {
            Value::F64(v) => {
                assert_eq!(v[0], 1.5);
                assert!(v[1].is_nan());
                assert_eq!(v[2], f64::INFINITY);
                assert_eq!(v[3], f64::NEG_INFINITY);
            }
            other => panic!("unexpected {other:?}"),
        }
        // DS text spelling "NaN" stays a string (not null).
        let mut ds2 = DataSet::new();
        ds2.put_text(tags::SAMPLING_FREQUENCY, Vr::Ds, "NaN");
        let j2 = dataset_to_json(&ds2);
        assert_eq!(j2["003A001A"]["Value"][0], json!("NaN"));
        let back2 = dataset_from_json(&j2).unwrap();
        assert_eq!(back2.string(tags::SAMPLING_FREQUENCY), Some("NaN"));
    }

    #[test]
    fn inline_binary_rejects_non_bulk_vrs() {
        let bulk = json!({"7FE00010": {"vr": "OW", "InlineBinary": "AA=="}});
        assert!(dataset_from_json(&bulk).is_ok());
        for vr in ["US", "SS", "FL", "FD", "PN", "SQ", "AT", "UI"] {
            let v = json!({"00280010": {"vr": vr, "InlineBinary": "AA=="}});
            assert!(dataset_from_json(&v).is_err(), "{vr}");
        }
        // Text VRs are refused too: the encoder rejects Bytes under
        // text VRs, so admitting them here would build datasets that
        // can never encode.
        for vr in ["LT", "ST", "UT", "UC", "UR"] {
            let v = json!({"00280010": {"vr": vr, "InlineBinary": "AA=="}});
            assert!(dataset_from_json(&v).is_err(), "{vr}");
        }
    }

    #[test]
    fn parses_sv_values() {
        let ds = dataset_from_json(&json!({"00720082": {"vr": "SV", "Value": [-5, 7]}})).unwrap();
        match &ds.get(Tag::new(0x0072, 0x0082)).unwrap().value {
            Value::I64(v) => assert_eq!(v, &[-5, 7]),
            other => panic!("unexpected {other:?}"),
        }
    }
}
