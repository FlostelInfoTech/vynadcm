//! File-backed Query/Retrieve index + C-FIND matching (backs `findscp`).
//!
//! - [`QrIndex::scan`] walks a directory of Part-10 files, keeping one
//!   header dataset (pixel data stripped) per file. Unreadable files
//!   are skipped with a warning, never fatal.
//! - Matching follows PS3.4 C.2.2.2: universal (empty), single value,
//!   wildcard (`*`/`?`), UID lists (OR), date/time/numeric ranges, and
//!   nested sequence items. PN single values match leading `^`
//!   components (`Smith` matches `Smith^John`).
//! - Responses echo exactly the requested keys with record values
//!   (Query/Retrieve Level included); sequences return full matching
//!   items. Matches dedupe at the query level.
//! - Below-level keys are matched leniently rather than rejected,
//!   and unknown VRs compare as plain strings.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use vyna_dcm_codec::read_part10;
use vyna_dcm_core::{DataElement, DataSet, Tag, Value, Vr, tags};

use crate::dimse::status;
use crate::service::{Handlers, MoveInstance, RequestContext};

/// One indexed file: path plus its header dataset (pixels stripped).
#[derive(Clone, Debug)]
pub struct QrRecord {
    /// Source file.
    pub path: PathBuf,
    /// Header dataset.
    pub dataset: DataSet,
}

/// File-backed query index.
#[derive(Clone, Debug, Default)]
pub struct QrIndex {
    records: Vec<QrRecord>,
    /// Files skipped during the scan (unreadable / not Part-10).
    pub skipped: usize,
}

impl QrIndex {
    /// Scan `dir` recursively for Part-10 files.
    pub fn scan(dir: &Path) -> crate::Result<Self> {
        let mut index = Self::default();
        Self::walk(dir, &mut index)?;
        Ok(index)
    }

    fn walk(dir: &Path, index: &mut Self) -> crate::Result<()> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            crate::Error::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {e}", dir.display()),
            ))
        })?;
        for entry in entries {
            let Ok(entry) = entry else {
                index.skipped += 1;
                continue;
            };
            let path = entry.path();
            if path.is_dir() {
                Self::walk(&path, index)?;
            } else if path.is_file() {
                match Self::read_header(&path) {
                    Some(ds) => index.records.push(QrRecord { path, dataset: ds }),
                    None => {
                        tracing::warn!("qrdb: skipping unreadable file {}", path.display());
                        index.skipped += 1;
                    }
                }
            }
        }
        Ok(())
    }

    fn read_header(path: &Path) -> Option<DataSet> {
        let bytes = std::fs::read(path).ok()?;
        let (_, mut ds) = read_part10(&bytes).ok()?;
        ds.remove(tags::PIXEL_DATA);
        // Overlay planes can be large; they are never query keys.
        for group in 0x6000..0x6020u16 {
            ds.remove(Tag::new(group, 0x3000));
        }
        Some(ds)
    }

    /// Indexed records.
    #[must_use]
    pub fn records(&self) -> &[QrRecord] {
        &self.records
    }
}

/// C-FIND over the index: final status plus one response per match
/// group at the query's level. `mwl` selects the worklist model (no
/// level key, no grouping).
pub fn find_matches(index: &QrIndex, query: &DataSet, mwl: bool) -> (u16, Vec<DataSet>) {
    if mwl {
        let out = index
            .records
            .iter()
            .filter(|r| dataset_matches(&r.dataset, query))
            .map(|r| build_response(&r.dataset, query))
            .collect();
        return (status::SUCCESS, out);
    }
    let level = match query_level(query) {
        Ok(level) => level,
        Err(s) => return (s, Vec::new()),
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for record in &index.records {
        if !dataset_matches(&record.dataset, query) {
            continue;
        }
        let Some(key) = level_key(&record.dataset, level) else {
            continue;
        };
        if seen.insert(key) {
            out.push(build_response(&record.dataset, query));
        }
    }
    (status::SUCCESS, out)
}

/// Query/retrieve level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QrLevel {
    /// `PATIENT`, grouped by Patient ID.
    Patient,
    /// `STUDY`, grouped by Study Instance UID.
    Study,
    /// `SERIES`, grouped by Series Instance UID.
    Series,
    /// `IMAGE`, grouped by SOP Instance UID.
    Image,
}

/// Required Query/Retrieve Level key (QR models only).
fn query_level(query: &DataSet) -> Result<QrLevel, u16> {
    match query.string(tags::QUERY_RETRIEVE_LEVEL) {
        Some("PATIENT") => Ok(QrLevel::Patient),
        Some("STUDY") => Ok(QrLevel::Study),
        Some("SERIES") => Ok(QrLevel::Series),
        Some("IMAGE") => Ok(QrLevel::Image),
        _ => Err(status::DATASET_MISMATCH),
    }
}

/// Identity key for level grouping.
fn level_key(ds: &DataSet, level: QrLevel) -> Option<String> {
    let tag = match level {
        QrLevel::Patient => tags::PATIENT_ID,
        QrLevel::Study => tags::STUDY_INSTANCE_UID,
        QrLevel::Series => tags::SERIES_INSTANCE_UID,
        QrLevel::Image => tags::SOP_INSTANCE_UID,
    };
    ds.string(tag).map(str::to_string)
}

/// True when every query key matches the record.
fn dataset_matches(record: &DataSet, query: &DataSet) -> bool {
    query.iter().all(|(tag, qel)| {
        if tag == tags::QUERY_RETRIEVE_LEVEL {
            return true; // control key, not a match key
        }
        match (&qel.value, record.get(tag)) {
            (Value::Empty, _) => true, // universal match
            (_, None) => false,        // constrained key, absent tag
            (Value::Sequence(qitems), Some(rel)) => match &rel.value {
                Value::Sequence(ritems) => qitems
                    .iter()
                    .all(|qi| ritems.iter().any(|ri| dataset_matches(ri, qi))),
                _ => false,
            },
            (qv, Some(rel)) => {
                let patterns = value_strings(qv);
                let values = value_strings(&rel.value);
                patterns.iter().any(|p| value_matches(p, &values, qel.vr))
            }
        }
    })
}

/// Response: the requested keys with record values (+ echoed level).
fn build_response(record: &DataSet, query: &DataSet) -> DataSet {
    let mut out = DataSet::new();
    for (tag, qel) in query.iter() {
        if tag == tags::QUERY_RETRIEVE_LEVEL {
            out.insert(DataElement::new(tag, qel.vr, qel.value.clone()));
            continue;
        }
        let Some(rel) = record.get(tag) else {
            continue;
        };
        let value = match (&qel.value, &rel.value) {
            (Value::Sequence(qitems), Value::Sequence(ritems)) => Value::Sequence(
                ritems
                    .iter()
                    .filter(|ri| qitems.iter().any(|qi| dataset_matches(ri, qi)))
                    .cloned()
                    .collect(),
            ),
            _ => rel.value.clone(),
        };
        out.insert(DataElement::new(tag, rel.vr, value));
    }
    out
}

/// Render a value to matchable strings.
fn value_strings(value: &Value) -> Vec<String> {
    match value {
        Value::Empty => Vec::new(),
        Value::Text(v) => v.clone(),
        Value::PersonName(names) => names.iter().map(|n| n.to_dicom_string()).collect(),
        Value::Tags(tags) => tags
            .iter()
            .map(|t| format!("{:04X}{:04X}", t.group(), t.element()))
            .collect(),
        Value::U16(v) => v.iter().map(ToString::to_string).collect(),
        Value::I16(v) => v.iter().map(ToString::to_string).collect(),
        Value::U32(v) => v.iter().map(ToString::to_string).collect(),
        Value::I32(v) => v.iter().map(ToString::to_string).collect(),
        Value::U64(v) => v.iter().map(ToString::to_string).collect(),
        Value::I64(v) => v.iter().map(ToString::to_string).collect(),
        Value::F32(v) => v.iter().map(ToString::to_string).collect(),
        Value::F64(v) => v.iter().map(ToString::to_string).collect(),
        // Byte blobs and sequences never match as plain values.
        Value::Bytes(_) | Value::Fragments(_) | Value::Sequence(_) => Vec::new(),
    }
}

/// One query pattern against the record's value strings.
fn value_matches(pattern: &str, values: &[String], vr: Vr) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        return values.iter().any(|v| wildcard_match(pattern, v));
    }
    if matches!(vr, Vr::Da | Vr::Tm | Vr::Dt | Vr::Ds | Vr::Is) && pattern.contains('-') {
        return range_matches(pattern, values, vr);
    }
    if vr == Vr::Pn {
        // Leading-component match: "Smith" matches "Smith^John".
        return values.iter().any(|v| {
            let (mut p, mut t) = (pattern.split('^'), v.split('^'));
            loop {
                match (p.next(), t.next()) {
                    (None, _) => return true,
                    (Some(a), Some(b)) => {
                        if a != b {
                            return false;
                        }
                    }
                    (Some(_), None) => return false,
                }
            }
        });
    }
    values.iter().any(|v| v == pattern)
}

/// Inclusive `lo-hi` range (either side may be empty = open).
fn range_matches(pattern: &str, values: &[String], vr: Vr) -> bool {
    // Datetimes never start with '-'; numerics might ("-5--1").
    let dash = if matches!(vr, Vr::Ds | Vr::Is) {
        pattern
            .get(1..)
            .and_then(|rest| rest.find('-').map(|i| i + 1))
    } else {
        pattern.find('-')
    };
    let Some(d) = dash else {
        return values.iter().any(|v| v == pattern);
    };
    let (lo, hi) = (&pattern[..d], &pattern[d + 1..]);
    if matches!(vr, Vr::Ds | Vr::Is) {
        let bound = |s: &str, open: f64| {
            if s.is_empty() {
                Some(open)
            } else {
                s.parse().ok()
            }
        };
        let (Some(lo), Some(hi)) = (bound(lo, f64::NEG_INFINITY), bound(hi, f64::INFINITY)) else {
            return false;
        };
        return values
            .iter()
            .any(|v| v.parse::<f64>().is_ok_and(|v| v >= lo && v <= hi));
    }
    values
        .iter()
        .any(|v| (lo.is_empty() || v.as_str() >= lo) && (hi.is_empty() || v.as_str() <= hi))
}

/// Glob match with `*` (any run) and `?` (one byte).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

// --- Handlers ---------------------------------------------------------------

use vyna_dcm_core::sop_class;

/// QR Find/Move/Get SCP over a [`QrIndex`].
#[derive(Clone)]
pub struct QrScp {
    index: std::sync::Arc<QrIndex>,
}

impl QrScp {
    /// Serve FIND/MOVE/GET from `index`.
    #[must_use]
    pub fn new(index: QrIndex) -> Self {
        Self {
            index: std::sync::Arc::new(index),
        }
    }
}

/// Modality Worklist FIND SCP over a [`QrIndex`].
#[derive(Clone)]
pub struct MwlScp {
    index: std::sync::Arc<QrIndex>,
}

impl MwlScp {
    /// Serve worklist FIND from `index`.
    #[must_use]
    pub fn new(index: QrIndex) -> Self {
        Self {
            index: std::sync::Arc::new(index),
        }
    }
}

fn is_qr_find(sop: &str) -> bool {
    matches!(
        sop,
        sop_class::PATIENT_ROOT_FIND
            | sop_class::STUDY_ROOT_FIND
            | sop_class::PATIENT_STUDY_ONLY_FIND
    )
}

fn is_qr_move(sop: &str) -> bool {
    matches!(
        sop,
        sop_class::PATIENT_ROOT_MOVE
            | sop_class::STUDY_ROOT_MOVE
            | sop_class::PATIENT_STUDY_ONLY_MOVE
    )
}

fn is_qr_get(sop: &str) -> bool {
    matches!(sop, sop_class::PATIENT_ROOT_GET | sop_class::STUDY_ROOT_GET)
}

// NOTE: Patient/Study Only has no C-GET SOP Class (PS3.4 K.6).

impl Handlers for QrScp {
    async fn on_find(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, Vec<DataSet>) {
        if !is_qr_find(&ctx.context.abstract_syntax) {
            return (status::SOP_CLASS_NOT_SUPPORTED, Vec::new());
        }
        if ctx.context.abstract_syntax == sop_class::PATIENT_STUDY_ONLY_FIND
            && matches!(
                query.string(tags::QUERY_RETRIEVE_LEVEL),
                Some("SERIES" | "IMAGE")
            )
        {
            return (status::DATASET_MISMATCH, Vec::new());
        }
        find_matches(&self.index, query, false)
    }

    async fn on_move(
        &self,
        ctx: &RequestContext<'_>,
        query: &DataSet,
        _destination: &str,
    ) -> (u16, Vec<MoveInstance>) {
        if !is_qr_move(&ctx.context.abstract_syntax) {
            return (status::SOP_CLASS_NOT_SUPPORTED, Vec::new());
        }
        retrieve_matches(&self.index, query).await
    }

    async fn on_get(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, Vec<MoveInstance>) {
        if !is_qr_get(&ctx.context.abstract_syntax) {
            return (status::SOP_CLASS_NOT_SUPPORTED, Vec::new());
        }
        retrieve_matches(&self.index, query).await
    }
}

/// Files matching a MOVE/GET query, each read in full. Level is still
/// required; matches dedupe by SOP Instance UID.
async fn retrieve_matches(index: &QrIndex, query: &DataSet) -> (u16, Vec<MoveInstance>) {
    if query_level(query).is_err() {
        return (status::DATASET_MISMATCH, Vec::new());
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for record in &index.records {
        if !dataset_matches(&record.dataset, query) {
            continue;
        }
        match level_key(&record.dataset, QrLevel::Image) {
            Some(key) if !seen.contains(&key) => {
                seen.insert(key);
            }
            _ => continue,
        }
        let bytes = match tokio::fs::read(&record.path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("qrdb: cannot re-read {}: {e}", record.path.display());
                return (status::PROCESSING_FAILURE, Vec::new());
            }
        };
        let Ok((_, ds)) = read_part10(&bytes) else {
            tracing::warn!("qrdb: {} changed on disk", record.path.display());
            return (status::PROCESSING_FAILURE, Vec::new());
        };
        let (Some(class), Some(inst)) = (
            ds.string(tags::SOP_CLASS_UID).map(str::to_string),
            ds.string(tags::SOP_INSTANCE_UID).map(str::to_string),
        ) else {
            return (status::PROCESSING_FAILURE, Vec::new());
        };
        out.push(MoveInstance {
            sop_class_uid: class,
            sop_instance_uid: inst,
            dataset: ds,
        });
    }
    (status::SUCCESS, out)
}

impl Handlers for MwlScp {
    async fn on_find(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, Vec<DataSet>) {
        if ctx.context.abstract_syntax != sop_class::MODALITY_WORKLIST_FIND {
            return (status::SOP_CLASS_NOT_SUPPORTED, Vec::new());
        }
        find_matches(&self.index, query, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(patient: &str, study: &str, series: &str, instance: &str, date: &str) -> QrRecord {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, patient);
        ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe^John");
        ds.put_text(tags::STUDY_INSTANCE_UID, Vr::Ui, study);
        ds.put_text(tags::STUDY_DATE, Vr::Da, date);
        ds.put_text(tags::SERIES_INSTANCE_UID, Vr::Ui, series);
        ds.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, instance);
        ds.put_text(tags::MODALITY, Vr::Cs, "CT");
        QrRecord {
            path: PathBuf::from(format!("{instance}.dcm")),
            dataset: ds,
        }
    }

    fn index() -> QrIndex {
        QrIndex {
            records: vec![
                record("P1", "1.2.1", "1.2.1.1", "1.2.1.1.1", "20240115"),
                record("P1", "1.2.1", "1.2.1.1", "1.2.1.1.2", "20240115"),
                record("P2", "1.2.2", "1.2.2.1", "1.2.2.1.1", "20240320"),
            ],
            skipped: 0,
        }
    }

    fn study_query() -> DataSet {
        let mut q = DataSet::new();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "STUDY");
        q.put_null(tags::STUDY_INSTANCE_UID, Vr::Ui);
        q
    }

    #[test]
    fn wildcard_and_range_match() {
        let idx = index();
        // Wildcard patient.
        let mut q = study_query();
        q.put_text(tags::PATIENT_ID, Vr::Lo, "P*");
        assert_eq!(find_matches(&idx, &q, false).1.len(), 2);
        let mut q = study_query();
        q.put_text(tags::PATIENT_ID, Vr::Lo, "P?");
        assert_eq!(find_matches(&idx, &q, false).1.len(), 2);
        let mut q = study_query();
        q.put_text(tags::PATIENT_ID, Vr::Lo, "P2");
        let (s, m) = find_matches(&idx, &q, false);
        assert_eq!((s, m.len()), (status::SUCCESS, 1));
        assert_eq!(m[0].string(tags::STUDY_INSTANCE_UID), Some("1.2.2"));
        // Date range (open low end).
        let mut q = study_query();
        q.put_text(tags::STUDY_DATE, Vr::Da, "-20240201");
        let (s, m) = find_matches(&idx, &q, false);
        assert_eq!((s, m.len()), (status::SUCCESS, 1));
        assert_eq!(m[0].string(tags::STUDY_INSTANCE_UID), Some("1.2.1"));
        // UID list (OR).
        let mut q = study_query();
        q.insert(DataElement::texts(
            tags::STUDY_INSTANCE_UID,
            Vr::Ui,
            vec!["1.2.1".to_string(), "9.9.9".to_string()],
        ));
        assert_eq!(find_matches(&idx, &q, false).1.len(), 1);
    }

    #[test]
    fn pn_matches_leading_components() {
        let idx = index();
        let mut q = study_query();
        q.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe");
        assert_eq!(find_matches(&idx, &q, false).1.len(), 2);
        let mut q = study_query();
        q.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe^Jane");
        assert_eq!(find_matches(&idx, &q, false).1.len(), 0);
    }

    #[test]
    fn level_grouping_dedupes() {
        let idx = index();
        // STUDY: two files collapse to one response.
        let (s, m) = find_matches(&idx, &study_query(), false);
        assert_eq!((s, m.len()), (status::SUCCESS, 2));
        // IMAGE: every file answers.
        let mut q = DataSet::new();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
        q.put_null(tags::SOP_INSTANCE_UID, Vr::Ui);
        assert_eq!(find_matches(&idx, &q, false).1.len(), 3);
        // Missing level fails.
        let q = DataSet::new();
        assert_eq!(find_matches(&idx, &q, false).0, status::DATASET_MISMATCH);
    }

    #[test]
    fn responses_echo_requested_keys_only() {
        let idx = index();
        let mut q = study_query();
        q.put_null(tags::PATIENT_ID, Vr::Lo);
        let (_, m) = find_matches(&idx, &q, false);
        assert_eq!(m.len(), 2);
        // Requested keys present with record values...
        assert_eq!(m[0].string(tags::PATIENT_ID), Some("P1"));
        assert_eq!(m[0].string(tags::QUERY_RETRIEVE_LEVEL), Some("STUDY"));
        // ...unrequested keys absent.
        assert!(!m[0].contains(tags::MODALITY));
    }

    #[test]
    fn sequence_keys_match_nested() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "W1");
        let mut sps = DataSet::new();
        sps.put_text(tags::SCHEDULED_STATION_AE_TITLE, Vr::Ae, "CT01");
        sps.put_text(tags::MODALITY, Vr::Cs, "CT");
        ds.put_sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE, vec![sps]);
        let idx = QrIndex {
            records: vec![QrRecord {
                path: PathBuf::from("w1.dcm"),
                dataset: ds,
            }],
            skipped: 0,
        };
        // MWL query: station match inside the sequence.
        let mut item = DataSet::new();
        item.put_text(tags::SCHEDULED_STATION_AE_TITLE, Vr::Ae, "CT01");
        item.put_null(tags::MODALITY, Vr::Cs);
        let mut q = DataSet::new();
        q.put_null(tags::PATIENT_ID, Vr::Lo);
        q.put_sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE, vec![item]);
        let (s, m) = find_matches(&idx, &q, true);
        assert_eq!((s, m.len()), (status::SUCCESS, 1));
        let items = m[0]
            .sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE)
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].string(tags::MODALITY), Some("CT"));
        // Wrong station: no match.
        let mut item = DataSet::new();
        item.put_text(tags::SCHEDULED_STATION_AE_TITLE, Vr::Ae, "MR01");
        let mut q = DataSet::new();
        q.put_sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE, vec![item]);
        assert_eq!(find_matches(&idx, &q, true).1.len(), 0);
    }
}
