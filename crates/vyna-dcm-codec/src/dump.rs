//! Human-readable dataset dump (`dcmdump` style, after dcm4che/pydicom).

use std::fmt::Write;
use vyna_dcm_core::{DataSet, Value, lookup};

/// Format a dataset as indented `(GGGG,EEEE) VR [value] # keyword` lines.
#[must_use]
pub fn dump_dataset(ds: &DataSet) -> String {
    let mut out = String::new();
    dump_into(ds, 0, &mut out);
    out
}

fn dump_into(ds: &DataSet, depth: usize, out: &mut String) {
    let pad: String = "  ".repeat(depth);
    for (tag, el) in ds.iter() {
        let keyword = lookup(tag).map(|e| e.keyword).unwrap_or("PrivateOrUnknown");
        match &el.value {
            Value::Sequence(items) => {
                let _ = writeln!(
                    out,
                    "{pad}{tag} {} (Sequence, {} items) # {keyword}",
                    el.vr,
                    items.len()
                );
                for (i, item) in items.iter().enumerate() {
                    let _ = writeln!(out, "{pad}  [Item {i}]");
                    dump_into(item, depth + 2, out);
                }
            }
            Value::Fragments(f) => {
                let _ = writeln!(
                    out,
                    "{pad}{tag} {} ({} fragments, {} bytes) # {keyword}",
                    el.vr,
                    f.fragments.len(),
                    f.payload_len()
                );
            }
            Value::Bytes(b) => {
                let preview: String = b.iter().take(16).map(|x| format!("{x:02X} ")).collect();
                let _ = writeln!(
                    out,
                    "{pad}{tag} {} [{} bytes: {preview}{}] # {keyword}",
                    el.vr,
                    b.len(),
                    if b.len() > 16 { "..." } else { "" }
                );
            }
            Value::Empty => {
                let _ = writeln!(out, "{pad}{tag} {} (empty) # {keyword}", el.vr);
            }
            v => {
                let _ = writeln!(out, "{pad}{tag} {} [{}] # {keyword}", el.vr, short_value(v));
            }
        }
    }
}

fn short_value(v: &Value) -> String {
    const MAX: usize = 120;
    let mut s = match v {
        Value::Text(t) => t.join("\\"),
        Value::PersonName(n) => n
            .iter()
            .map(|p| p.to_dicom_string())
            .collect::<Vec<_>>()
            .join("\\"),
        Value::Tags(t) => t
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join("\\"),
        Value::U16(x) => join_nums(x),
        Value::I16(x) => join_nums(x),
        Value::U32(x) => join_nums(x),
        Value::I32(x) => join_nums(x),
        Value::U64(x) => join_nums(x),
        Value::I64(x) => join_nums(x),
        Value::F32(x) => join_nums(x),
        Value::F64(x) => join_nums(x),
        Value::Empty | Value::Bytes(_) | Value::Sequence(_) | Value::Fragments(_) => String::new(),
    };
    if s.len() > MAX {
        s.truncate(MAX);
        s.push_str("...");
    }
    s.replace('\n', " ")
}

fn join_nums<T: ToString>(v: &[T]) -> String {
    v.iter()
        .take(16)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_core::{Vr, tags};

    #[test]
    fn dump_contains_expected_lines() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe^Jane");
        ds.put_u16_single(tags::ROWS, 512);
        let text = dump_dataset(&ds);
        assert!(text.contains("(0010,0010)"), "{text}");
        assert!(text.contains("PatientName"), "{text}");
        assert!(text.contains("512"), "{text}");
    }
}
