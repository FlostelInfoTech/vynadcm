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

    // -- typed readers ----------------------------------------------------

    /// First text value of `tag`, or `None` when absent/not text.
    #[must_use]
    pub fn string(&self, tag: Tag) -> Option<&str> {
        self.get(tag).and_then(|e| e.value.first_text())
    }

    /// All text values of `tag`.
    #[must_use]
    pub fn strings(&self, tag: Tag) -> Option<&[String]> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::Text(v)) => Some(v),
            _ => None,
        }
    }

    /// Required string; error when missing or empty.
    pub fn require_string(&self, tag: Tag) -> Result<&str> {
        self.string(tag)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::MissingAttribute(tag.to_string()))
    }

    /// Person names of `tag`.
    #[must_use]
    pub fn person_names(&self, tag: Tag) -> Option<&[PersonName]> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::PersonName(v)) => Some(v),
            _ => None,
        }
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
    #[must_use]
    pub fn float(&self, tag: Tag) -> Option<f64> {
        let e = self.get(tag)?;
        match &e.value {
            Value::Text(v) => v.first()?.trim().parse::<f64>().ok(),
            Value::F32(v) => v.first().map(|f| *f as f64),
            Value::F64(v) => v.first().copied(),
            Value::U16(v) => v.first().map(|f| *f as f64),
            Value::U32(v) => v.first().map(|f| *f as f64),
            Value::I16(v) => v.first().map(|f| *f as f64),
            Value::I32(v) => v.first().map(|f| *f as f64),
            _ => None,
        }
    }

    /// Parse *all* `DS`/`IS`/numeric values as `f64`, skipping
    /// unparseable text ones (mirrors [`DataSet::float`]).
    #[must_use]
    pub fn float_list(&self, tag: Tag) -> Option<Vec<f64>> {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::Text(v)) => Some(
                v.iter()
                    .filter_map(|s| s.trim().parse::<f64>().ok())
                    .collect(),
            ),
            Some(Value::F32(v)) => Some(v.iter().map(|f| *f as f64).collect()),
            Some(Value::F64(v)) => Some(v.clone()),
            Some(Value::U16(v)) => Some(v.iter().map(|f| *f as f64).collect()),
            Some(Value::U32(v)) => Some(v.iter().map(|f| *f as f64).collect()),
            Some(Value::I16(v)) => Some(v.iter().map(|f| *f as f64).collect()),
            Some(Value::I32(v)) => Some(v.iter().map(|f| *f as f64).collect()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tags;

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
        assert_eq!(ds.string(tags::PATIENT_ID), None);
        assert!(ds.require_string(tags::PATIENT_ID).is_err());

        // Ascending iteration order regardless of insertion order.
        let tags_seen: Vec<Tag> = ds.tags().collect();
        let mut sorted = tags_seen.clone();
        sorted.sort();
        assert_eq!(tags_seen, sorted);
    }
}
