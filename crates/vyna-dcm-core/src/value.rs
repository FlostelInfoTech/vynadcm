//! Data elements and their values.
//!
//! A [`DataElement`] is the `(tag, VR, value)` triple of NEMA PS3.5 §7.1.
//! [`Value`] keeps text VRs as strings, numeric VRs as typed vectors, byte
//! VRs (`OB OW OD OF OL OV UN`) as raw bytes, `SQ` as nested datasets, and
//! encapsulated data as a fragment sequence.

use crate::{DataSet, PersonName, Tag, Vr};

/// Basic Offset Table plus data fragments of one encapsulated element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FragmentSequence {
    /// Offset (bytes) of each frame within the fragment stream.
    /// Empty means "no basic offset table" (offsets unknown).
    pub basic_offset_table: Vec<u32>,
    /// Data fragments in order (each ≤ 64 KiB on the wire).
    pub fragments: Vec<Vec<u8>>,
}

impl FragmentSequence {
    /// Concatenate all fragments (BOT excluded).
    #[must_use]
    pub fn joined(&self) -> Vec<u8> {
        let len: usize = self.fragments.iter().map(Vec::len).sum();
        let mut out = Vec::with_capacity(len);
        for f in &self.fragments {
            out.extend_from_slice(f);
        }
        out
    }

    /// Total payload bytes across fragments.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        self.fragments.iter().map(Vec::len).sum()
    }
}

/// The value of one data element.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Empty (zero-length) value, any VR.
    Empty,
    /// Text VRs (`AE AS CS DA DS DT IS LO LT SH ST TM UC UI UR UT`):
    /// one entry per backslash-separated component.
    Text(Vec<String>),
    /// Person names (`PN`), one per backslash-separated value.
    PersonName(Vec<PersonName>),
    /// Attribute tags (`AT`).
    Tags(Vec<Tag>),
    /// Unsigned short (`US`).
    U16(Vec<u16>),
    /// Signed short (`SS`).
    I16(Vec<i16>),
    /// Unsigned long (`UL`) and selector `UV` low halves are *not* merged:
    /// `UV` uses [`Value::U64`].
    U32(Vec<u32>),
    /// Signed long (`SL`).
    I32(Vec<i32>),
    /// Unsigned very long (`UV`), signed long (`SV` retired → `I64`).
    U64(Vec<u64>),
    /// Signed 64-bit.
    I64(Vec<i64>),
    /// Floating point single (`FL`).
    F32(Vec<f32>),
    /// Floating point double (`FD`).
    F64(Vec<f64>),
    /// Raw bytes for `OB OW OD OF OL OV UN` (native element order preserved
    /// by the codec; see typed accessors for endian-aware reads).
    Bytes(Vec<u8>),
    /// Sequence of items (`SQ`).
    Sequence(Vec<DataSet>),
    /// Encapsulated fragments (undefined-length `OB/OW` pixel or waveform data).
    Fragments(FragmentSequence),
}

impl Value {
    /// True for [`Value::Empty`] or any zero-length container.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Empty => true,
            Value::Text(v) => v.is_empty() || v.iter().all(String::is_empty),
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
            Value::Fragments(f) => f.fragments.is_empty(),
        }
    }

    /// Value multiplicity (number of values).
    #[must_use]
    pub fn multiplicity(&self) -> usize {
        match self {
            Value::Empty => 0,
            Value::Text(v) => v.len(),
            Value::PersonName(v) => v.len(),
            Value::Tags(v) => v.len(),
            Value::U16(v) => v.len(),
            Value::I16(v) => v.len(),
            Value::U32(v) => v.len(),
            Value::I32(v) => v.len(),
            Value::U64(v) => v.len(),
            Value::I64(v) => v.len(),
            Value::F32(v) => v.len(),
            Value::F64(v) => v.len(),
            Value::Bytes(v) => {
                if v.is_empty() {
                    0
                } else {
                    1
                }
            }
            Value::Sequence(v) => v.len(),
            Value::Fragments(f) => f.fragments.len(),
        }
    }

    /// First text value, if any.
    #[must_use]
    pub fn first_text(&self) -> Option<&str> {
        match self {
            Value::Text(v) => v.first().map(String::as_str),
            _ => None,
        }
    }

    /// Interpret raw `OW/OB` bytes as little-endian `u16` words.
    /// A trailing odd byte is dropped.
    #[must_use]
    pub fn bytes_as_u16_le(&self) -> Option<Vec<u16>> {
        match self {
            Value::Bytes(b) => Some(
                b.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect(),
            ),
            Value::U16(v) => Some(v.clone()),
            _ => None,
        }
    }

    /// Interpret raw `OW/OB` bytes as big-endian `u16` words.
    #[must_use]
    pub fn bytes_as_u16_be(&self) -> Option<Vec<u16>> {
        match self {
            Value::Bytes(b) => Some(
                b.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Interpret raw bytes as little-endian `i16` words (waveform data).
    #[must_use]
    pub fn bytes_as_i16_le(&self) -> Option<Vec<i16>> {
        match self {
            Value::Bytes(b) => Some(
                b.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| i16::from_le_bytes(*c))
                    .collect(),
            ),
            Value::I16(v) => Some(v.clone()),
            _ => None,
        }
    }
}

/// One `(tag, VR, value)` data element.
#[derive(Clone, Debug, PartialEq)]
pub struct DataElement {
    /// Attribute tag.
    pub tag: Tag,
    /// Value representation.
    pub vr: Vr,
    /// Decoded value.
    pub value: Value,
}

impl DataElement {
    /// Build an element directly.
    #[must_use]
    pub fn new(tag: Tag, vr: Vr, value: Value) -> Self {
        Self { tag, vr, value }
    }

    /// Empty element.
    #[must_use]
    pub fn empty(tag: Tag, vr: Vr) -> Self {
        Self::new(tag, vr, Value::Empty)
    }

    /// Single-valued text element (`LO SH CS DA TM DT AS UI UR ...`).
    pub fn text(tag: Tag, vr: Vr, s: impl Into<String>) -> Self {
        Self::new(tag, vr, Value::Text(vec![s.into()]))
    }

    /// Multi-valued text element.
    pub fn texts(tag: Tag, vr: Vr, v: Vec<String>) -> Self {
        Self::new(tag, vr, Value::Text(v))
    }

    /// Person name element.
    pub fn person_name(tag: Tag, names: Vec<PersonName>) -> Self {
        Self::new(tag, Vr::Pn, Value::PersonName(names))
    }

    /// Tag list (`AT`).
    pub fn tags(tag: Tag, tags: Vec<Tag>) -> Self {
        Self::new(tag, Vr::At, Value::Tags(tags))
    }

    /// Unsigned shorts (`US`).
    pub fn u16(tag: Tag, v: Vec<u16>) -> Self {
        Self::new(tag, Vr::Us, Value::U16(v))
    }

    /// Single unsigned short.
    pub fn u16_single(tag: Tag, v: u16) -> Self {
        Self::u16(tag, vec![v])
    }

    /// Unsigned longs (`UL`).
    pub fn u32(tag: Tag, v: Vec<u32>) -> Self {
        Self::new(tag, Vr::Ul, Value::U32(v))
    }

    /// Single unsigned long.
    pub fn u32_single(tag: Tag, v: u32) -> Self {
        Self::u32(tag, vec![v])
    }

    /// Single-precision floats (`FL`).
    pub fn f32(tag: Tag, v: Vec<f32>) -> Self {
        Self::new(tag, Vr::Fl, Value::F32(v))
    }

    /// Double-precision floats (`FD`).
    pub fn f64(tag: Tag, v: Vec<f64>) -> Self {
        Self::new(tag, Vr::Fd, Value::F64(v))
    }

    /// Raw bytes (`OB OW OD OF OL OV UN`).
    pub fn bytes(tag: Tag, vr: Vr, v: Vec<u8>) -> Self {
        Self::new(tag, vr, Value::Bytes(v))
    }

    /// Sequence (`SQ`).
    pub fn sequence(tag: Tag, items: Vec<DataSet>) -> Self {
        Self::new(tag, Vr::Sq, Value::Sequence(items))
    }

    /// UID element (`UI`).
    pub fn uid(tag: Tag, uid: impl Into<String>) -> Self {
        Self::text(tag, Vr::Ui, uid.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_helpers() {
        assert!(Value::Empty.is_empty());
        assert!(Value::Text(vec![]).is_empty());
        assert_eq!(Value::U16(vec![1, 2]).multiplicity(), 2);
        assert_eq!(Value::Text(vec!["a".into()]).first_text(), Some("a"));
        assert_eq!(Value::U16(vec![1]).first_text(), None);
    }

    #[test]
    fn byte_word_views() {
        let v = Value::Bytes(vec![0x01, 0x02, 0x03]);
        assert_eq!(v.bytes_as_u16_le(), Some(vec![0x0201]));
        assert_eq!(v.bytes_as_u16_be(), Some(vec![0x0102]));
        assert_eq!(
            Value::Bytes(vec![0xFF, 0xFF]).bytes_as_i16_le(),
            Some(vec![-1])
        );
    }

    #[test]
    fn fragments_join() {
        let f = FragmentSequence {
            basic_offset_table: vec![0],
            fragments: vec![vec![1, 2], vec![3]],
        };
        assert_eq!(f.joined(), vec![1, 2, 3]);
        assert_eq!(f.payload_len(), 3);
    }
}
