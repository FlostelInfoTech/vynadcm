//! [`DataSet`]: an ordered collection of data elements.
//!
//! Elements are keyed by tag in ascending order, which is also the
//! encoding order required by NEMA PS3.5 §7.5.

use std::collections::BTreeMap;

use crate::{DataElement, Error, PersonName, Result, Tag, Value, Vr};

/// An ordered set of DICOM data elements.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataSet {
    elements: BTreeMap<Tag, DataElement>,
}

impl DataSet {
    /// Empty dataset.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Insert (or replace) an element.
    pub fn insert(&mut self, element: DataElement) {
        self.elements.insert(element.tag, element);
    }

    /// Insert and return `self` for builder-style construction.
    #[must_use]
    pub fn with(mut self, element: DataElement) -> Self {
        self.insert(element);
        self
    }

    /// Fetch an element by tag.
    #[must_use]
    pub fn get(&self, tag: Tag) -> Option<&DataElement> {
        self.elements.get(&tag)
    }

    /// Fetch an element mutably by tag.
    pub fn get_mut(&mut self, tag: Tag) -> Option<&mut DataElement> {
        self.elements.get_mut(&tag)
    }

    /// Remove an element, returning it when present.
    pub fn remove(&mut self, tag: Tag) -> Option<DataElement> {
        self.elements.remove(&tag)
    }

    /// True when the tag is present (even with an empty value).
    #[must_use]
    pub fn contains(&self, tag: Tag) -> bool {
        self.elements.contains_key(&tag)
    }

    /// Iterate `(tag, element)` pairs in ascending tag order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (Tag, &DataElement)> + ExactSizeIterator {
        self.elements.iter().map(|(t, e)| (*t, e))
    }

    /// Iterate tags in ascending order.
    pub fn tags(&self) -> impl DoubleEndedIterator<Item = Tag> + '_ {
        self.elements.keys().copied()
    }

    /// Approximate in-RAM payload bytes of every element's
    /// [`Value`](crate::Value) (see
    /// [`approx_bytes`](crate::Value::approx_bytes) for what is and is
    /// not counted). For DoS accounting, not billing.
    #[must_use]
    pub fn approx_bytes(&self) -> u64 {
        self.elements.values().map(|e| e.value.approx_bytes()).sum()
    }

    // -- typed readers ----------------------------------------------------

    /// First text value of `tag`, or `None` when absent/not text.
    ///
    /// Decoded `PN` values are [`Value::PersonName`], not text: use
    /// [`person_names`](Self::person_names) or
    /// [`person_name_string`](Self::person_name_string) for those.
    #[must_use]
    pub fn string(&self, tag: Tag) -> Option<&str> {
        self.get(tag).and_then(|e| e.value.first_text())
    }

    /// All text values of `tag`.
    ///
    /// Blind to decoded [`Value::PersonName`] like [`string`](Self::string).
    #[must_use]
    pub fn strings(&self, tag: Tag) -> Option<&[String]> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::Text(v)) => Some(v),
            _ => None,
        }
    }

    /// Required string; [`Error::MissingAttribute`] when absent,
    /// [`Error::Value`] when present but empty or not text.
    ///
    /// Decoded `PN` values fail here (they are
    /// [`Value::PersonName`]); read those with
    /// [`person_names`](Self::person_names) or
    /// [`person_name_string`](Self::person_name_string).
    pub fn require_string(&self, tag: Tag) -> Result<&str> {
        let Some(e) = self.get(tag) else {
            return Err(Error::MissingAttribute(tag.to_string()));
        };
        match e.value.first_text() {
            Some(s) if !s.is_empty() => Ok(s),
            Some(_) => Err(Error::Value {
                tag: tag.to_string(),
                message: "empty value".into(),
            }),
            None => Err(Error::Value {
                tag: tag.to_string(),
                message: format!("expected text, found {:?}", e.vr),
            }),
        }
    }

    /// Person names of `tag`.
    #[must_use]
    pub fn person_names(&self, tag: Tag) -> Option<&[PersonName]> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::PersonName(v)) => Some(v),
            _ => None,
        }
    }

    /// First person name of `tag` rendered as a DICOM string, or `None`
    /// when absent/not `PN`. This is the `string()` equivalent for
    /// decoded person names (which [`string`](Self::string) cannot see).
    #[must_use]
    pub fn person_name_string(&self, tag: Tag) -> Option<String> {
        self.person_names(tag)?
            .first()
            .map(PersonName::to_dicom_string)
    }

    /// First `u16` of `tag` (`US`).
    #[must_use]
    pub fn uint16(&self, tag: Tag) -> Option<u16> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::U16(v)) => v.first().copied(),
            _ => None,
        }
    }

    /// All `u16` values of `tag`.
    #[must_use]
    pub fn uint16_list(&self, tag: Tag) -> Option<&[u16]> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::U16(v)) => Some(v),
            _ => None,
        }
    }

    /// First `u32` of `tag` (`UL`).
    #[must_use]
    pub fn uint32(&self, tag: Tag) -> Option<u32> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::U32(v)) => v.first().copied(),
            _ => None,
        }
    }

    /// Parse the first `DS`/`IS`/numeric value as `f64`.
    ///
    /// 64-bit integers lose precision past 2⁵³ (documented, not
    /// hidden: the `as f64` conversion rounds). Text uses DS grammar:
    /// Rust-only spellings (`inf`, `NaN`, ...) are not numbers and
    /// yield `None` (binary `FL`/`FD` infinities stay faithful — the
    /// bits really say that). Text under any other VR (a `DA` like
    /// `20261011`, an `LO` like `1.5`) is not a number either.
    #[must_use]
    pub fn float(&self, tag: Tag) -> Option<f64> {
        let e = self.get(tag)?;
        match &e.value {
            Value::Text(v) if matches!(e.vr, Vr::Ds | Vr::Is) => {
                v.first().and_then(|s| parse_finite(s))
            }
            Value::F32(v) => v.first().map(|f| *f as f64),
            Value::F64(v) => v.first().copied(),
            Value::U16(v) => v.first().map(|f| *f as f64),
            Value::U32(v) => v.first().map(|f| *f as f64),
            Value::U64(v) => v.first().map(|f| *f as f64),
            Value::I16(v) => v.first().map(|f| *f as f64),
            Value::I32(v) => v.first().map(|f| *f as f64),
            Value::I64(v) => v.first().map(|f| *f as f64),
            _ => None,
        }
    }

    /// Parse *all* `DS`/`IS`/numeric values as `f64`, skipping
    /// unparseable text ones (mirrors [`DataSet::float`]).
    #[must_use]
    pub fn float_list(&self, tag: Tag) -> Option<Vec<f64>> {
        let e = self.get(tag)?;
        match &e.value {
            Value::Text(v) if matches!(e.vr, Vr::Ds | Vr::Is) => {
                Some(v.iter().filter_map(|s| parse_finite(s)).collect())
            }
            Value::F32(v) => Some(v.iter().map(|f| *f as f64).collect()),
            Value::F64(v) => Some(v.clone()),
            Value::U16(v) => Some(v.iter().map(|f| *f as f64).collect()),
            Value::U32(v) => Some(v.iter().map(|f| *f as f64).collect()),
            Value::U64(v) => Some(v.iter().map(|f| *f as f64).collect()),
            Value::I16(v) => Some(v.iter().map(|f| *f as f64).collect()),
            Value::I32(v) => Some(v.iter().map(|f| *f as f64).collect()),
            Value::I64(v) => Some(v.iter().map(|f| *f as f64).collect()),
            _ => None,
        }
    }

    /// Sequence items of `tag`.
    #[must_use]
    pub fn sequence(&self, tag: Tag) -> Option<&[DataSet]> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::Sequence(v)) => Some(v),
            _ => None,
        }
    }

    /// Raw bytes of `tag` (`OB OW ...`).
    #[must_use]
    pub fn bytes(&self, tag: Tag) -> Option<&[u8]> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::Bytes(v)) => Some(v),
            _ => None,
        }
    }

    /// VR of `tag` when present.
    #[must_use]
    pub fn vr_of(&self, tag: Tag) -> Option<Vr> {
        self.get(tag).map(|e| e.vr)
    }

    // -- typed writers ----------------------------------------------------

    /// Put a single text value.
    pub fn put_text(&mut self, tag: Tag, vr: Vr, s: impl Into<String>) {
        self.insert(DataElement::text(tag, vr, s));
    }

    /// Put person names.
    pub fn put_person_name(&mut self, tag: Tag, names: Vec<PersonName>) {
        self.insert(DataElement::person_name(tag, names));
    }

    /// Put unsigned shorts.
    pub fn put_u16(&mut self, tag: Tag, v: Vec<u16>) {
        self.insert(DataElement::u16(tag, v));
    }

    /// Put one unsigned short.
    pub fn put_u16_single(&mut self, tag: Tag, v: u16) {
        self.insert(DataElement::u16_single(tag, v));
    }

    /// Put unsigned longs.
    pub fn put_u32(&mut self, tag: Tag, v: Vec<u32>) {
        self.insert(DataElement::u32(tag, v));
    }

    /// Put one unsigned long.
    pub fn put_u32_single(&mut self, tag: Tag, v: u32) {
        self.insert(DataElement::u32_single(tag, v));
    }

    /// Put raw bytes.
    pub fn put_bytes(&mut self, tag: Tag, vr: Vr, v: Vec<u8>) {
        self.insert(DataElement::bytes(tag, vr, v));
    }

    /// Put a sequence.
    pub fn put_sequence(&mut self, tag: Tag, items: Vec<DataSet>) {
        self.insert(DataElement::sequence(tag, items));
    }

    /// Put a UID.
    pub fn put_uid(&mut self, tag: Tag, uid: impl Into<String>) {
        self.insert(DataElement::uid(tag, uid));
    }

    /// Put an empty element.
    pub fn put_null(&mut self, tag: Tag, vr: Vr) {
        self.insert(DataElement::empty(tag, vr));
    }
}

/// Parse `DS`/`IS` text as `f64`: the spelling must match the DS
/// grammar (PS3.5 §6.2) and the value must be finite (an exponent
/// like `1e999` parses but overflows to infinity).
fn parse_finite(s: &str) -> Option<f64> {
    let t = s.trim();
    if !is_ds_spelling(t) {
        return None;
    }
    let f: f64 = t.parse().ok()?;
    f.is_finite().then_some(f)
}

/// True when `t` (already trimmed) matches the DS grammar: an
/// optional sign, digits with an optional fraction (a `.` needs
/// digits after it), and an optional exponent. Rejects the
/// Rust-only `f64::from_str` spellings (`1_0`, `12.`, `inf`,
/// `NaN`, ...) that are not DS.
fn is_ds_spelling(t: &str) -> bool {
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - frac_start;
        if frac_digits == 0 {
            return false;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return false;
        }
    }
    i == b.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags;

    #[test]
    fn require_string_distinguishes_absence_from_bad_value() {
        let mut ds = DataSet::new();
        assert!(matches!(
            ds.require_string(tags::MODALITY),
            Err(crate::Error::MissingAttribute(_))
        ));
        ds.put_text(tags::MODALITY, Vr::Cs, "");
        assert!(matches!(
            ds.require_string(tags::MODALITY),
            Err(crate::Error::Value { .. })
        ));
        ds.insert(DataElement::new(
            tags::MODALITY,
            Vr::Us,
            Value::U16(vec![1]),
        ));
        let err = ds.require_string(tags::MODALITY).unwrap_err();
        assert!(matches!(err, crate::Error::Value { .. }), "{err:?}");
        assert!(err.to_string().contains("Us"), "{err}");
        ds.put_text(tags::MODALITY, Vr::Cs, "ECG");
        assert_eq!(ds.require_string(tags::MODALITY).unwrap(), "ECG");
    }

    #[test]
    fn float_rejects_nonfinite_text_keeps_binary() {
        let mut ds = DataSet::new();
        for bad in ["inf", "-inf", "Infinity", "NaN", "nan", "INF"] {
            ds.put_text(tags::RESCALE_SLOPE, Vr::Ds, bad);
            assert_eq!(ds.float(tags::RESCALE_SLOPE), None, "{bad}");
            assert_eq!(
                ds.float_list(tags::RESCALE_SLOPE).unwrap(),
                Vec::<f64>::new(),
                "{bad}"
            );
        }
        ds.put_text(tags::RESCALE_SLOPE, Vr::Ds, "1.25");
        assert_eq!(ds.float(tags::RESCALE_SLOPE), Some(1.25));
        // Binary infinities are faithful (the bits really say that).
        ds.insert(DataElement::new(
            tags::RESCALE_SLOPE,
            Vr::Fl,
            Value::F32(vec![f32::INFINITY]),
        ));
        assert_eq!(ds.float(tags::RESCALE_SLOPE), Some(f64::INFINITY));
    }

    #[test]
    fn float_rejects_rust_only_ds_spellings() {
        // `f64::from_str` accepts these, but the DS grammar (PS3.5
        // §6.2) does not: underscores, a bare trailing point, a bare
        // exponent, and embedded whitespace.
        for bad in ["1_0", "12.", "1e", "e5", ".", "+", "1 2", "0x10"] {
            assert_eq!(parse_finite(bad), None, "{bad:?}");
        }
        for (good, want) in [
            ("1", 1.0),
            ("-0.5", -0.5),
            ("+2e3", 2000.0),
            (".5", 0.5),
            ("1E-3", 0.001),
            (" 12.5 ", 12.5),
        ] {
            assert_eq!(parse_finite(good), Some(want), "{good:?}");
        }
        // Finite grammar, infinite value: exponent overflow stays out.
        assert_eq!(parse_finite("1e999"), None);
    }

    #[test]
    fn decoded_person_names_need_pn_accessors() {
        // Decoded PN data is Value::PersonName, invisible to string().
        let mut ds = DataSet::new();
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("Doe^Jane")]);
        assert_eq!(ds.string(tags::PATIENT_NAME), None);
        assert!(ds.require_string(tags::PATIENT_NAME).is_err());
        assert_eq!(
            ds.person_name_string(tags::PATIENT_NAME),
            Some("Doe^Jane".to_string())
        );
        assert_eq!(ds.person_names(tags::PATIENT_NAME).unwrap().len(), 1);
        // Absent / wrong-type still None.
        assert_eq!(ds.person_name_string(tags::PATIENT_ID), None);
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "E123");
        assert_eq!(ds.person_name_string(tags::PATIENT_ID), None);
    }

    #[test]
    fn float_ignores_non_ds_is_text() {
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20261011");
        ds.put_text(tags::SERIES_DESCRIPTION, Vr::Lo, "1.5");
        assert_eq!(ds.float(tags::STUDY_DATE), None);
        assert_eq!(ds.float(tags::SERIES_DESCRIPTION), None);
        assert_eq!(ds.float_list(tags::STUDY_DATE), None);
        ds.put_text(tags::RESCALE_SLOPE, Vr::Ds, "1.25");
        assert_eq!(ds.float(tags::RESCALE_SLOPE), Some(1.25));
    }

    #[test]
    fn typed_accessors() {
        let mut ds = DataSet::new();
        assert!(ds.is_empty());
        ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe^Jane");
        ds.put_u16_single(tags::COMMAND_FIELD, 0x0030);
        ds.put_text(tags::SAMPLING_FREQUENCY, Vr::Ds, "500");

        assert_eq!(ds.len(), 3);
        assert_eq!(ds.string(tags::PATIENT_NAME), Some("Doe^Jane"));
        assert_eq!(ds.uint16(tags::COMMAND_FIELD), Some(0x0030));
        assert_eq!(ds.float_list(tags::SAMPLING_FREQUENCY), Some(vec![500.0]));
        // float_list covers the same numeric VRs as float.
        ds.insert(DataElement::new(tags::ROWS, Vr::Us, Value::U16(vec![4, 8])));
        assert_eq!(ds.float_list(tags::ROWS), Some(vec![4.0, 8.0]));
        ds.insert(DataElement::new(
            tags::SELECTOR_SS_VALUE,
            Vr::Ss,
            Value::I16(vec![-2]),
        ));
        assert_eq!(ds.float_list(tags::SELECTOR_SS_VALUE), Some(vec![-2.0]));
        // 64-bit integers convert (with f64 rounding past 2^53).
        ds.insert(DataElement::new(
            tags::SELECTOR_UV_VALUE,
            Vr::Uv,
            Value::U64(vec![7, 1 << 60]),
        ));
        assert_eq!(ds.float(tags::SELECTOR_UV_VALUE), Some(7.0));
        assert_eq!(
            ds.float_list(tags::SELECTOR_UV_VALUE),
            Some(vec![7.0, (1u64 << 60) as f64])
        );
        ds.insert(DataElement::new(
            tags::SELECTOR_SV_VALUE,
            Vr::Sv,
            Value::I64(vec![-9]),
        ));
        assert_eq!(ds.float(tags::SELECTOR_SV_VALUE), Some(-9.0));
        assert_eq!(ds.float_list(tags::SELECTOR_SV_VALUE), Some(vec![-9.0]));
        assert_eq!(ds.string(tags::PATIENT_ID), None);
        assert!(ds.require_string(tags::PATIENT_ID).is_err());

        // Ascending iteration order regardless of insertion order.
        let tags_seen: Vec<Tag> = ds.tags().collect();
        let mut sorted = tags_seen.clone();
        sorted.sort();
        assert_eq!(tags_seen, sorted);
    }
}
