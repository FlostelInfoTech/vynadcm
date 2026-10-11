//! DICOM JSON Model (NEMA PS3.18 Annex F).
//!
//! - Tags become 8-uppercase-hex keys (`"00100010"`).
//! - Each value object carries `"vr"` plus `"Value"` (or `"InlineBinary"`
//!   for byte VRs, base64), or `"DataFragment"` for encapsulated data.
//! - `PN` values become `{"Alphabetic": "..."}` objects; `AT` values are
//!   8-hex-digit strings; `DS`/`IS` accept numbers or strings on input.

use base64::Engine;
use serde_json::{Map, Value as Json, json};
use vyna_dcm_core::{DataElement, DataSet, PersonName, Tag, Value, Vr};

use crate::{Error, Result};

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

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
    let map = v
        .as_object()
        .ok_or_else(|| Error::Json("top level must be an object".into()))?;
    let mut ds = DataSet::new();
    for (key, obj) in map {
        let tag = parse_tag_key(key)?;
        ds.insert(element_from_json(tag, obj)?);
    }
    Ok(ds)
}

fn tag_key(tag: Tag) -> String {
    format!("{:04X}{:04X}", tag.group(), tag.element())
}

fn parse_tag_key(key: &str) -> Result<Tag> {
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
                    o.insert("Alphabetic".into(), Json::String(n.to_dicom_string()));
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
fn text_json(vr: Vr, s: &str) -> Json {
    match vr {
        Vr::Ds => s
            .trim()
            .parse::<f64>()
            .map(Json::from)
            .unwrap_or_else(|_| Json::String(s.into())),
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
    Json::Array(it.map(Json::from).collect())
}

fn element_from_json(tag: Tag, obj: &Json) -> Result<DataElement> {
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
        let bytes = B64.decode(b64).map_err(|e| Error::Base64(e.to_string()))?;
        if vr == Vr::Sq {
            return Err(Error::Json(format!("{tag}: InlineBinary with SQ")));
        }
        // Fragments cannot be distinguished from bytes in the compact form;
        // keep bytes (callers re-fragment for encapsulated syntaxes).
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
                items.push(dataset_from_json(v)?);
            }
            Value::Sequence(items)
        }
        Vr::Pn => {
            let mut names = Vec::with_capacity(values.len());
            for v in values {
                match v {
                    Json::Object(o) => {
                        let s = o.get("Alphabetic").and_then(Json::as_str).unwrap_or("");
                        names.push(PersonName::parse(s));
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
            v.as_f64()
                .ok_or_else(|| Error::Json(format!("{tag}: expected number")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::tags;

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
    fn parses_sv_values() {
        let ds = dataset_from_json(&json!({"00720082": {"vr": "SV", "Value": [-5, 7]}})).unwrap();
        match &ds.get(Tag::new(0x0072, 0x0082)).unwrap().value {
            Value::I64(v) => assert_eq!(v, &[-5, 7]),
            other => panic!("unexpected {other:?}"),
        }
    }
}
