//! File-backed Query/Retrieve index + C-FIND matching (backs `findscp`).
//!
//! - [`QrIndex::scan`] walks a directory of Part-10 files, keeping one
//!   header dataset (pixel data never decoded) per file. Unreadable
//!   files are skipped with a warning, never fatal.
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
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use vyna_dcm_codec::{read_part10, read_part10_header_complete};
use vyna_dcm_core::{DataElement, DataSet, Tag, Value, Vr, tags};

use crate::dimse::status;
use crate::service::{
    Handlers, MoveInstance, RequestContext, RetrieveLoad, RetrieveLoader, RetrievePlan, ScpFollowUp,
};
use crate::worklist::{COMMITMENT_ACTION_REQUEST, COMMITMENT_EVENT_TYPE, STORAGE_COMMITMENT_PUSH};

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
    ///
    /// Symlinks are never followed (a looped link would recurse
    /// forever, and any link escapes the scanned root). Depth is
    /// capped at `MAX_SCAN_DEPTH` (64) levels.
    pub fn scan(dir: &Path) -> crate::Result<Self> {
        let mut index = Self::default();
        Self::walk(dir, &mut index, 0)?;
        Ok(index)
    }

    fn walk(dir: &Path, index: &mut Self, depth: usize) -> crate::Result<()> {
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
            // Never follow symlinks: `Path::is_dir` follows them, so a
            // looped link would recurse forever (and any link escapes
            // the scanned root).
            let Ok(ft) = entry.file_type() else {
                index.skipped += 1;
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                if depth >= MAX_SCAN_DEPTH {
                    tracing::warn!(
                        "qrdb: directory {} past max scan depth, skipped",
                        path.display()
                    );
                    index.skipped += 1;
                    continue;
                }
                Self::walk(&path, index, depth + 1)?;
            } else if ft.is_file() {
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
        Self::read_header_capped(path, SCAN_FIRST_READ, SCAN_MAX_HEADER)
    }

    /// Header-only read through a growing prefix: typical headers land
    /// in the first read, large ones grow toward `max`, and headers
    /// past `max` skip the file instead of spiking RAM on multi-GB
    /// pixel payloads (which are never even read, let alone decoded).
    /// A prefix that decodes without reaching the Pixel Data stop is
    /// grown, never indexed partial (a boundary-aligned truncation
    /// would silently drop query keys).
    fn read_header_capped(path: &Path, first: u64, max: u64) -> Option<DataSet> {
        use std::io::Read as _;
        let mut file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        let mut want = first.max(1);
        loop {
            let before = bytes.len() as u64;
            let chunk = want.saturating_sub(before);
            if chunk > 0 {
                std::io::Read::by_ref(&mut file)
                    .take(chunk)
                    .read_to_end(&mut bytes)
                    .ok()?;
            }
            let eof = (bytes.len() as u64) - before < chunk;
            match read_part10_header_complete(&bytes) {
                Ok((_, mut ds, complete)) if complete || eof => {
                    // Overlay planes can be large; they are never keys.
                    for group in 0x6000..0x6020u16 {
                        ds.remove(Tag::new(group, 0x3000));
                    }
                    return Some(ds);
                }
                _ if !eof && (bytes.len() as u64) < max => {
                    want = want.saturating_mul(4).min(max).max(bytes.len() as u64 + 1);
                }
                _ => return None,
            }
        }
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
/// Maximum matches collected for one C-FIND/C-MOVE/C-GET: past this
/// the SCP fails out-of-resources instead of loading the archive
/// into RAM (C-FIND: `A700`; MOVE/GET: `A701`, unable to calculate
/// the number of matches).
const MAX_QR_MATCHES: usize = 10_000;

/// Maximum directory depth [`QrIndex::scan`] descends: pathological
/// local nesting must not overflow the stack.
const MAX_SCAN_DEPTH: usize = 64;

/// First header-prefix probe: typical headers (units of KiB) land in
/// one read; pixel payloads past the headers are never touched.
const SCAN_FIRST_READ: u64 = 256 * 1024;

/// Headers past this are not indexed (the file is skipped with a
/// warning): bounds the per-file transient no matter how large the
/// pixel payload behind them.
const SCAN_MAX_HEADER: u64 = 64 * 1024 * 1024;

/// Maximum query cost units (one per key, one per sequence item,
/// one per match value, at every nesting level): a crafted FIND
/// identifier must fail `A700`/`A701`, not burn CPU/RAM per record.
/// Legit queries carry dozens of keys and single-item sequences.
const MAX_QUERY_VALUES: usize = 10_000;

/// Maximum Storage Commitment references per N-ACTION: past this the
/// SCP fails out-of-resources instead of amplifying peer-controlled
/// items ~2-3x into RAM. Real requests carry dozens of refs.
const MAX_COMMIT_REFS: usize = 10_000;

/// Run a C-FIND query (`mwl` selects worklist matching): status plus
/// one response dataset per match, capped at `MAX_QR_MATCHES` (10,000).
pub fn find_matches(index: &QrIndex, query: &DataSet, mwl: bool) -> (u16, Vec<DataSet>) {
    let Some(compiled) = compile_query(query) else {
        return (status::OUT_OF_RESOURCES, Vec::new());
    };
    if mwl {
        let mut out = Vec::new();
        for r in index
            .records
            .iter()
            .filter(|r| dataset_matches(&r.dataset, &compiled))
        {
            if out.len() >= MAX_QR_MATCHES {
                return (status::OUT_OF_RESOURCES, Vec::new());
            }
            out.push(build_response(&r.dataset, query, &compiled));
        }
        return (status::SUCCESS, out);
    }
    let level = match query_level(query) {
        Ok(level) => level,
        Err(s) => return (s, Vec::new()),
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for record in &index.records {
        if !dataset_matches(&record.dataset, &compiled) {
            continue;
        }
        let Some(key) = level_key(&record.dataset, level) else {
            continue;
        };
        if seen.insert(key) {
            if out.len() >= MAX_QR_MATCHES {
                return (status::OUT_OF_RESOURCES, Vec::new());
            }
            out.push(build_response(&record.dataset, query, &compiled));
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

/// One compiled match key: the query side rendered once, not per
/// record, with the total value count capped at compile time.
#[derive(Clone, Debug)]
enum CompiledKey {
    /// Empty query value: matches anything present or absent.
    Universal,
    /// Pre-rendered match patterns plus the query VR.
    Values { vr: Vr, patterns: Vec<String> },
    /// Nested sequence items, compiled recursively.
    Sequence(Vec<CompiledQuery>),
}

/// A query compiled for matching (level key excluded: control, not
/// match, key).
#[derive(Clone, Debug, Default)]
struct CompiledQuery(Vec<(Tag, CompiledKey)>);

/// Render the query side once; `None` when the query carries more
/// than [`MAX_QUERY_VALUES`] match values (callers fail out-of-resources).
fn compile_query(query: &DataSet) -> Option<CompiledQuery> {
    let mut budget = MAX_QUERY_VALUES;
    compile_level(query, &mut budget)
}

fn compile_level(query: &DataSet, budget: &mut usize) -> Option<CompiledQuery> {
    let mut out = Vec::new();
    for (tag, qel) in query.iter() {
        if tag == tags::QUERY_RETRIEVE_LEVEL {
            continue;
        }
        // Every key costs budget: even universal keys loop once per
        // record in the matcher.
        *budget = budget.checked_sub(1)?;
        match &qel.value {
            Value::Empty => out.push((tag, CompiledKey::Universal)),
            Value::Sequence(qitems) => {
                // Every item multiplies the per-record matcher
                // (query items x record items), so items cost too.
                *budget = budget.checked_sub(qitems.len())?;
                let mut items = Vec::with_capacity(qitems.len());
                for qi in qitems {
                    items.push(compile_level(qi, budget)?);
                }
                out.push((tag, CompiledKey::Sequence(items)));
            }
            qv => {
                let patterns = value_strings(qv);
                *budget = budget.checked_sub(patterns.len())?;
                out.push((
                    tag,
                    CompiledKey::Values {
                        vr: qel.vr,
                        patterns,
                    },
                ));
            }
        }
    }
    Some(CompiledQuery(out))
}

/// True when every query key matches the record.
fn dataset_matches(record: &DataSet, query: &CompiledQuery) -> bool {
    query.0.iter().all(|(tag, key)| {
        match (key, record.get(*tag)) {
            (CompiledKey::Universal, _) => true, // universal match
            (_, None) => false,                  // constrained key, absent tag
            (CompiledKey::Sequence(qitems), Some(rel)) => match &rel.value {
                Value::Sequence(ritems) => qitems
                    .iter()
                    .all(|qi| ritems.iter().any(|ri| dataset_matches(ri, qi))),
                _ => false,
            },
            (CompiledKey::Values { vr, patterns }, Some(rel)) => {
                let values = value_strings(&rel.value);
                patterns.iter().any(|p| value_matches(p, &values, *vr))
            }
        }
    })
}

/// Response: the requested keys with record values (+ echoed level).
fn build_response(record: &DataSet, query: &DataSet, compiled: &CompiledQuery) -> DataSet {
    let mut out = DataSet::new();
    if let Some(qel) = query.get(tags::QUERY_RETRIEVE_LEVEL) {
        out.insert(DataElement::new(
            tags::QUERY_RETRIEVE_LEVEL,
            qel.vr,
            qel.value.clone(),
        ));
    }
    for (tag, key) in &compiled.0 {
        let Some(rel) = record.get(*tag) else {
            continue;
        };
        let value = match (key, &rel.value) {
            (CompiledKey::Sequence(qitems), Value::Sequence(ritems)) => Value::Sequence(
                ritems
                    .iter()
                    .filter(|ri| qitems.iter().any(|qi| dataset_matches(ri, qi)))
                    .cloned()
                    .collect(),
            ),
            _ => rel.value.clone(),
        };
        out.insert(DataElement::new(*tag, rel.vr, value));
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

/// Storage Commitment (Push) SCP over a [`QrIndex`]: instances the
/// index holds commit, the rest fail with 0x0112. The verdict ships
/// as an N-EVENT-REPORT follow-up on the same association.
#[derive(Clone)]
pub struct StgCommitScp {
    index: std::sync::Arc<QrIndex>,
}

impl StgCommitScp {
    /// Serve commitment requests against `index` holdings.
    #[must_use]
    pub fn new(index: QrIndex) -> Self {
        Self {
            index: std::sync::Arc::new(index),
        }
    }

    /// Study UID for an indexed instance (bin audits).
    #[must_use]
    pub fn study_uid_for(&self, sop_instance_uid: &str) -> Option<String> {
        self.index.records.iter().find_map(|r| {
            (r.dataset.string(tags::SOP_INSTANCE_UID) == Some(sop_instance_uid))
                .then(|| r.dataset.string(tags::STUDY_INSTANCE_UID))
                .flatten()
                .map(str::to_string)
        })
    }

    /// N-ACTION core: status + optional RSP dataset + the report
    /// follow-up. `requested_uid` is the commitment request UID.
    pub fn commit(
        &self,
        requested_uid: Option<&str>,
        action_type: Option<u16>,
        sop_class: &str,
        dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>, Vec<ScpFollowUp>) {
        if sop_class != STORAGE_COMMITMENT_PUSH {
            return (status::SOP_CLASS_NOT_SUPPORTED, None, Vec::new());
        }
        if action_type != Some(COMMITMENT_ACTION_REQUEST) {
            return (status::UNRECOGNIZED_OPERATION, None, Vec::new());
        }
        let Some(uid) = requested_uid.filter(|u| vyna_dcm_core::is_valid_uid(u)) else {
            return (status::MISTYPED_ARGUMENT, None, Vec::new());
        };
        let Some(req) = dataset else {
            return (status::DATASET_MISMATCH, None, Vec::new());
        };
        let Some(txn) = req.string(tags::TRANSACTION_UID) else {
            return (status::DATASET_MISMATCH, None, Vec::new());
        };
        // Count before collecting: each ref amplifies ~2-3x into owned
        // strings plus a report item, so an uncapped N-ACTION is a RAM
        // multiplier on peer-controlled input.
        let items = req.sequence(tags::REFERENCED_SOP_SEQUENCE);
        if items.map(|v| v.len()).unwrap_or(0) > MAX_COMMIT_REFS {
            return (status::OUT_OF_RESOURCES, None, Vec::new());
        }
        let refs: Vec<(String, String)> = items
            .map(|items| {
                items
                    .iter()
                    .filter_map(|it| {
                        Some((
                            it.string(tags::REFERENCED_SOP_CLASS_UID)?.to_string(),
                            it.string(tags::REFERENCED_SOP_INSTANCE_UID)?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Held-set built once: per-ref linear scans would be
        // O(refs x records) on peer-controlled N-ACTION input.
        let held: HashSet<(&str, &str)> = self
            .index
            .records
            .iter()
            .filter_map(|r| {
                Some((
                    r.dataset.string(tags::SOP_CLASS_UID)?,
                    r.dataset.string(tags::SOP_INSTANCE_UID)?,
                ))
            })
            .collect();
        let (mut ok, mut failed) = (Vec::new(), Vec::new());
        for (class, inst) in refs {
            let is_held = held.contains(&(class.as_str(), inst.as_str()));
            if is_held {
                let mut item = DataSet::new();
                item.put_uid(tags::REFERENCED_SOP_CLASS_UID, class);
                item.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, inst);
                ok.push(item);
            } else {
                let mut item = DataSet::new();
                item.put_uid(tags::REFERENCED_SOP_CLASS_UID, class);
                item.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, inst);
                item.put_u16_single(tags::FAILURE_REASON, status::NO_SUCH_SOP_INSTANCE);
                failed.push(item);
            }
        }
        let mut report = DataSet::new();
        report.put_uid(tags::TRANSACTION_UID, txn);
        report.put_sequence(tags::REFERENCED_SOP_SEQUENCE, ok);
        report.put_sequence(tags::FAILED_SOP_SEQUENCE, failed);
        let follow_up = ScpFollowUp::EventReport {
            sop_class_uid: STORAGE_COMMITMENT_PUSH.to_string(),
            affected_sop_instance_uid: uid.to_string(),
            event_type_id: COMMITMENT_EVENT_TYPE,
            dataset: Some(report),
        };
        (status::SUCCESS, None, vec![follow_up])
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

/// Patient/Study Only (PS3.4 K.6.1.2) serves PATIENT/STUDY levels
/// only: SERIES/IMAGE under its FIND/MOVE SOPs is an identifier
/// mismatch. Shared by FIND/MOVE/GET so the three cannot drift
/// (the GET arm never trips — no such SOP exists — but stays for
/// symmetry if one is ever added).
fn patient_study_only_level_violation(sop: &str, query: &DataSet) -> bool {
    matches!(
        sop,
        sop_class::PATIENT_STUDY_ONLY_FIND | sop_class::PATIENT_STUDY_ONLY_MOVE
    ) && matches!(
        query.string(tags::QUERY_RETRIEVE_LEVEL),
        Some("SERIES" | "IMAGE")
    )
}

impl Handlers for QrScp {
    async fn on_find(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, Vec<DataSet>) {
        if !is_qr_find(&ctx.context.abstract_syntax) {
            return (status::SOP_CLASS_NOT_SUPPORTED, Vec::new());
        }
        if patient_study_only_level_violation(&ctx.context.abstract_syntax, query) {
            return (status::DATASET_MISMATCH, Vec::new());
        }
        find_matches(&self.index, query, false)
    }

    async fn on_move_plan(
        &self,
        ctx: &RequestContext<'_>,
        query: &DataSet,
        _destination: &str,
    ) -> (u16, RetrievePlan) {
        if !is_qr_move(&ctx.context.abstract_syntax) {
            return (
                status::SOP_CLASS_NOT_SUPPORTED,
                RetrievePlan::buffered(Vec::new()),
            );
        }
        if patient_study_only_level_violation(&ctx.context.abstract_syntax, query) {
            return (status::DATASET_MISMATCH, RetrievePlan::buffered(Vec::new()));
        }
        retrieve_plan(&self.index, query)
    }

    async fn on_get_plan(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, RetrievePlan) {
        if !is_qr_get(&ctx.context.abstract_syntax) {
            return (
                status::SOP_CLASS_NOT_SUPPORTED,
                RetrievePlan::buffered(Vec::new()),
            );
        }
        if patient_study_only_level_violation(&ctx.context.abstract_syntax, query) {
            return (status::DATASET_MISMATCH, RetrievePlan::buffered(Vec::new()));
        }
        retrieve_plan(&self.index, query)
    }
}

/// Plan a MOVE/GET retrieve from header matches only: no instance
/// data is loaded here. Level is still required; matches dedupe by
/// SOP Instance UID. The framework then streams load-and-send through
/// [`FileLoader`], so a 10k-match retrieve holds ~one dataset in RAM.
/// A file that fails to load counts one failed sub-operation while
/// the rest continue (previously the whole retrieve failed).
fn retrieve_plan(index: &QrIndex, query: &DataSet) -> (u16, RetrievePlan) {
    if query_level(query).is_err() {
        return (status::DATASET_MISMATCH, RetrievePlan::buffered(Vec::new()));
    }
    let Some(compiled) = compile_query(query) else {
        return (
            status::UNABLE_TO_CALCULATE_MATCHES,
            RetrievePlan::buffered(Vec::new()),
        );
    };
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    let mut classes: Vec<String> = Vec::new();
    for record in &index.records {
        if !dataset_matches(&record.dataset, &compiled) {
            continue;
        }
        let Some(key) = level_key(&record.dataset, QrLevel::Image) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        // Checked before queueing: the (MAX+1)-th match fails without
        // planning anything more.
        if paths.len() >= MAX_QR_MATCHES {
            return (
                status::UNABLE_TO_CALCULATE_MATCHES,
                RetrievePlan::buffered(Vec::new()),
            );
        }
        if let Some(class) = record.dataset.string(tags::SOP_CLASS_UID) {
            let class = class.to_string();
            if !classes.contains(&class) {
                classes.push(class);
            }
        }
        paths.push(record.path.clone());
    }
    let total = paths.len();
    (
        status::SUCCESS,
        RetrievePlan::new(total, classes, FileLoader { paths }),
    )
}

/// Loads one planned file just before its C-STORE.
struct FileLoader {
    paths: Vec<PathBuf>,
}

impl RetrieveLoader for FileLoader {
    fn load(&self, index: usize) -> Pin<Box<dyn Future<Output = RetrieveLoad> + Send + '_>> {
        let path = self.paths.get(index).cloned();
        Box::pin(async move {
            let Some(path) = path else {
                return RetrieveLoad::Failed;
            };
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!("qrdb: cannot re-read {}: {e}", path.display());
                    return RetrieveLoad::Failed;
                }
            };
            let Ok((_, ds)) = read_part10(&bytes) else {
                tracing::warn!("qrdb: {} changed on disk", path.display());
                return RetrieveLoad::Failed;
            };
            let (Some(class), Some(inst)) = (
                ds.string(tags::SOP_CLASS_UID).map(str::to_string),
                ds.string(tags::SOP_INSTANCE_UID).map(str::to_string),
            ) else {
                return RetrieveLoad::Failed;
            };
            RetrieveLoad::Ready(MoveInstance {
                sop_class_uid: class,
                sop_instance_uid: inst,
                dataset: ds,
            })
        })
    }
}

impl Handlers for MwlScp {
    async fn on_find(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, Vec<DataSet>) {
        if ctx.context.abstract_syntax != sop_class::MODALITY_WORKLIST_FIND {
            return (status::SOP_CLASS_NOT_SUPPORTED, Vec::new());
        }
        find_matches(&self.index, query, true)
    }
}

impl Handlers for StgCommitScp {
    async fn on_n_action(
        &self,
        ctx: &RequestContext<'_>,
        dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>, Vec<ScpFollowUp>) {
        // N-ACTION-RQ carries *requested* (not affected) UIDs.
        self.commit(
            ctx.command.requested_sop_instance_uid.as_deref(),
            ctx.command.action_type_id,
            ctx.command.requested_sop_class_uid.as_deref().unwrap_or(""),
            dataset,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_only_guard_fires_for_find_and_move() {
        for sop in [
            sop_class::PATIENT_STUDY_ONLY_FIND,
            sop_class::PATIENT_STUDY_ONLY_MOVE,
        ] {
            for level in ["SERIES", "IMAGE"] {
                let mut q = DataSet::new();
                q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, level);
                assert!(
                    patient_study_only_level_violation(sop, &q),
                    "{sop} {level} must trip"
                );
            }
            for level in ["PATIENT", "STUDY"] {
                let mut q = DataSet::new();
                q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, level);
                assert!(
                    !patient_study_only_level_violation(sop, &q),
                    "{sop} {level} must pass"
                );
            }
        }
        // Other models never trip, at any level.
        for sop in [
            sop_class::PATIENT_ROOT_FIND,
            sop_class::STUDY_ROOT_MOVE,
            sop_class::STUDY_ROOT_GET,
        ] {
            let mut q = DataSet::new();
            q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
            assert!(!patient_study_only_level_violation(sop, &q), "{sop}");
        }
    }

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
    fn match_flood_fails_out_of_resources() {
        let mut records = Vec::new();
        for i in 0..=MAX_QR_MATCHES {
            records.push(record(
                "P",
                &format!("1.2.{i}"),
                &format!("1.2.{i}.1"),
                &format!("1.2.{i}.1.1"),
                "20240115",
            ));
        }
        let idx = QrIndex {
            records,
            skipped: 0,
        };
        // C-FIND refuses with A700 instead of building 10k responses.
        assert_eq!(
            find_matches(&idx, &study_query(), false).0,
            status::OUT_OF_RESOURCES
        );
        assert_eq!(
            find_matches(&idx, &study_query(), true).0,
            status::OUT_OF_RESOURCES
        );
    }

    fn uid_list_query(n: usize) -> DataSet {
        let mut q = DataSet::new();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
        let uids: Vec<String> = (0..n).map(|i| format!("1.9.{i}")).collect();
        q.insert(DataElement::texts(tags::SOP_INSTANCE_UID, Vr::Ui, uids));
        q
    }

    #[test]
    fn query_value_flood_fails_out_of_resources() {
        let idx = index();
        // One value over budget: A700, no responses (the key itself
        // costs one unit, so 10000 values already exceed it).
        let (s, m) = find_matches(&idx, &uid_list_query(MAX_QUERY_VALUES), false);
        assert_eq!(s, status::OUT_OF_RESOURCES);
        assert!(m.is_empty());
        // Exactly at budget (9999 values + 1 key): evaluated normally.
        let (s, _) = find_matches(&idx, &uid_list_query(MAX_QUERY_VALUES - 1), false);
        assert_eq!(s, status::SUCCESS);
        // MOVE/GET planning fails A701 on the same flood.
        let (s, _) = retrieve_plan(&idx, &uid_list_query(MAX_QUERY_VALUES));
        assert_eq!(s, status::UNABLE_TO_CALCULATE_MATCHES);
    }

    #[test]
    fn universal_key_flood_fails_out_of_resources() {
        let idx = index();
        // Empty keys cost one unit each: 10001 distinct universal keys
        // exceed the budget even with no values at all.
        let mut q = DataSet::new();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "STUDY");
        for i in 0..MAX_QUERY_VALUES + 1 {
            q.put_null(Tag::new(0x0009, i as u16), Vr::Lo);
        }
        let (s, m) = find_matches(&idx, &q, false);
        assert_eq!(s, status::OUT_OF_RESOURCES);
        assert!(m.is_empty());
    }

    #[test]
    fn commitment_ref_flood_fails_out_of_resources() {
        let scp = StgCommitScp::new(index());
        let request = |n: usize| {
            let mut req = DataSet::new();
            req.put_uid(tags::TRANSACTION_UID, "1.2.9.9");
            req.put_sequence(
                tags::REFERENCED_SOP_SEQUENCE,
                (0..n)
                    .map(|i| {
                        let mut it = DataSet::new();
                        it.put_uid(tags::REFERENCED_SOP_CLASS_UID, "1.2.840.10008.5.1.4.1.1.2");
                        it.put_uid(tags::REFERENCED_SOP_INSTANCE_UID, format!("1.9.9.{i}"));
                        it
                    })
                    .collect(),
            );
            req
        };
        // One over cap: A700, no follow-ups.
        let (s, ds, follow) = scp.commit(
            Some("1.2.9.8"),
            Some(COMMITMENT_ACTION_REQUEST),
            STORAGE_COMMITMENT_PUSH,
            Some(request(MAX_COMMIT_REFS + 1)),
        );
        assert_eq!(s, status::OUT_OF_RESOURCES);
        assert!(ds.is_none());
        assert!(follow.is_empty());
        // Exactly at cap: evaluated normally.
        let (s, _, follow) = scp.commit(
            Some("1.2.9.8"),
            Some(COMMITMENT_ACTION_REQUEST),
            STORAGE_COMMITMENT_PUSH,
            Some(request(MAX_COMMIT_REFS)),
        );
        assert_eq!(s, status::SUCCESS);
        assert_eq!(follow.len(), 1);
    }

    #[test]
    fn sequence_item_flood_fails_out_of_resources() {
        let idx = index();
        // One sequence key with 10000 empty items: the items alone
        // (plus the key) exceed the budget.
        let mut q = DataSet::new();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "STUDY");
        q.put_sequence(
            tags::REFERENCED_SERIES_SEQUENCE,
            vec![DataSet::new(); MAX_QUERY_VALUES],
        );
        let (s, m) = find_matches(&idx, &q, false);
        assert_eq!(s, status::OUT_OF_RESOURCES);
        assert!(m.is_empty());
    }

    #[test]
    fn query_value_budget_spans_nesting() {
        let idx = index();
        // 6000 values in a sequence item plus 4001 at top level share
        // one budget: 10001 total fails.
        let mut q = DataSet::new();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "STUDY");
        let mut item = DataSet::new();
        item.insert(DataElement::texts(
            tags::SOP_INSTANCE_UID,
            Vr::Ui,
            (0..6000).map(|i| format!("1.8.{i}")).collect(),
        ));
        q.put_sequence(tags::REFERENCED_SERIES_SEQUENCE, vec![item]);
        q.insert(DataElement::texts(
            tags::SERIES_INSTANCE_UID,
            Vr::Ui,
            (0..4001).map(|i| format!("1.7.{i}")).collect(),
        ));
        assert_eq!(find_matches(&idx, &q, false).0, status::OUT_OF_RESOURCES);
    }

    #[tokio::test]
    async fn retrieve_flood_fails_at_plan_time() {
        // One shared file keeps the fixture cheap; distinct index keys
        // still drive the match count past the cap.
        let dir = std::env::temp_dir().join(format!("qrdb-flood-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shared.dcm");
        let mut ds = DataSet::new();
        ds.put_text(
            vyna_dcm_core::tags::SOP_CLASS_UID,
            Vr::Ui,
            "1.2.840.10008.5.1.4.1.1.2",
        );
        ds.put_text(vyna_dcm_core::tags::SOP_INSTANCE_UID, Vr::Ui, "1.2.9");
        let meta = vyna_dcm_codec::FileMeta::new(
            "1.2.840.10008.5.1.4.1.1.2",
            "1.2.9",
            vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
        );
        std::fs::write(&path, vyna_dcm_codec::write_part10(&meta, &ds).unwrap()).unwrap();
        let mut records = Vec::new();
        for i in 0..=MAX_QR_MATCHES {
            let mut r = record(
                "P",
                &format!("1.2.{i}"),
                &format!("1.2.{i}.1"),
                &format!("1.2.{i}.1.1"),
                "20240115",
            );
            r.path = path.clone();
            records.push(r);
        }
        let idx = QrIndex {
            records,
            skipped: 0,
        };
        let mut q = study_query();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
        // Plan-time cap: no instance data is touched at all.
        let (s, plan) = retrieve_plan(&idx, &q);
        assert_eq!(s, status::UNABLE_TO_CALCULATE_MATCHES);
        assert_eq!(plan.total, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn retrieve_plan_streams_one_file_at_a_time() {
        // Two matches over real files: the plan carries paths only,
        // and each load() reads exactly one file on demand.
        let dir = std::env::temp_dir().join(format!("qrdb-stream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut records = Vec::new();
        for i in 0..2 {
            let path = dir.join(format!("img{i}.dcm"));
            let mut ds = DataSet::new();
            ds.put_uid(
                vyna_dcm_core::tags::SOP_CLASS_UID,
                "1.2.840.10008.5.1.4.1.1.2",
            );
            ds.put_uid(vyna_dcm_core::tags::SOP_INSTANCE_UID, format!("1.2.7.{i}"));
            let meta = vyna_dcm_codec::FileMeta::new(
                "1.2.840.10008.5.1.4.1.1.2",
                format!("1.2.7.{i}"),
                vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
            );
            std::fs::write(&path, vyna_dcm_codec::write_part10(&meta, &ds).unwrap()).unwrap();
            let mut r = record("P", "1.2.7", "1.2.7.1", &format!("1.2.7.{i}"), "20240115");
            // Header carries the class for the plan (as scan would).
            r.dataset.put_uid(
                vyna_dcm_core::tags::SOP_CLASS_UID,
                "1.2.840.10008.5.1.4.1.1.2",
            );
            r.path = path;
            records.push(r);
        }
        let idx = QrIndex {
            records,
            skipped: 0,
        };
        let mut q = study_query();
        q.put_text(tags::QUERY_RETRIEVE_LEVEL, Vr::Cs, "IMAGE");
        let (s, plan) = retrieve_plan(&idx, &q);
        assert_eq!(s, status::SUCCESS);
        assert_eq!(plan.total, 2);
        assert_eq!(
            plan.storage_classes,
            vec!["1.2.840.10008.5.1.4.1.1.2".to_string()]
        );
        for (i, want) in ["1.2.7.0", "1.2.7.1"].iter().enumerate() {
            match plan.load(i).await {
                RetrieveLoad::Ready(inst) => {
                    assert_eq!(&inst.sop_instance_uid, want);
                    assert_eq!(
                        inst.dataset.string(vyna_dcm_core::tags::SOP_INSTANCE_UID),
                        Some(*want)
                    );
                }
                RetrieveLoad::Failed => panic!("load({i}) unexpectedly failed"),
            }
        }
        // Out of range loads fail rather than panic.
        assert!(matches!(plan.load(2).await, RetrieveLoad::Failed));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn header_prefix_grows_skips_and_never_partials() {
        use vyna_dcm_core::TransferSyntax as Ts;
        let dir = std::env::temp_dir().join(format!("qrdb-prefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, ds: &DataSet| {
            let meta = vyna_dcm_codec::FileMeta::new(
                "1.2.840.10008.5.1.4.1.1.2",
                ds.string(vyna_dcm_core::tags::SOP_INSTANCE_UID).unwrap(),
                Ts::ExplicitVrLittleEndian,
            );
            let path = dir.join(name);
            let bytes = vyna_dcm_codec::write_part10(&meta, ds).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            (path, bytes)
        };
        let mut small = DataSet::new();
        small.put_uid(
            vyna_dcm_core::tags::SOP_CLASS_UID,
            "1.2.840.10008.5.1.4.1.1.2",
        );
        small.put_uid(vyna_dcm_core::tags::SOP_INSTANCE_UID, "1.2.9.1");
        small.put_text(tags::PATIENT_ID, Vr::Lo, "P1");
        // 300 KB of pixels past a 1 KB first probe: indexed without
        // ever reading the payload (results, not RAM, are asserted —
        // the bound itself is structural).
        let mut big_pix = small.clone();
        big_pix.insert(DataElement::new(
            tags::PIXEL_DATA,
            Vr::Ow,
            Value::Bytes(vec![7u8; 300_000]),
        ));
        let (path, _) = write("bigpix.dcm", &big_pix);
        let got = QrIndex::read_header_capped(&path, 1024, 4096).unwrap();
        assert_eq!(got.string(tags::PATIENT_ID), Some("P1"));
        assert!(got.get(tags::PIXEL_DATA).is_none());
        // 2 KB private blob before the keys, first probe 512 B: the
        // prefix must grow (all keys still recovered).
        let mut wide = small.clone();
        wide.put_text(tags::PATIENT_ID, Vr::Lo, "P2");
        wide.insert(DataElement::new(
            Tag::new(0x0009, 0x1001),
            Vr::Ob,
            Value::Bytes(vec![8u8; 2000]),
        ));
        let (path, _) = write("wide.dcm", &wide);
        let got = QrIndex::read_header_capped(&path, 512, 8192).unwrap();
        assert_eq!(got.string(tags::PATIENT_ID), Some("P2"));
        // Same file, max below the headers: skipped, not partial.
        assert!(QrIndex::read_header_capped(&path, 512, 1024).is_none());
        // Razor case: first probe ends exactly at the pixel tag, so it
        // decodes but the stop is unreached — must grow, not index.
        let (_, pix_bytes) = write("razor.dcm", &big_pix);
        let (_, head_bytes) = write(
            "razor-head.dcm",
            &DataSet::new()
                .with(DataElement::new(
                    vyna_dcm_core::tags::SOP_CLASS_UID,
                    Vr::Ui,
                    Value::Text(vec!["1.2.840.10008.5.1.4.1.1.2".into()]),
                ))
                .with(DataElement::new(
                    vyna_dcm_core::tags::SOP_INSTANCE_UID,
                    Vr::Ui,
                    Value::Text(vec!["1.2.9.1".into()]),
                ))
                .with(DataElement::new(
                    tags::PATIENT_ID,
                    Vr::Lo,
                    Value::Text(vec!["P1".into()]),
                )),
        );
        // Sanity: the head-only file really is a prefix of the pixel one.
        assert!(pix_bytes.starts_with(&head_bytes));
        let got =
            QrIndex::read_header_capped(&dir.join("razor.dcm"), head_bytes.len() as u64, 1 << 20)
                .unwrap();
        assert_eq!(got.string(tags::PATIENT_ID), Some("P1"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn scan_skips_symlink_loops() {
        // A symlink pointing back at an ancestor must not recurse
        // forever (nor escape the scanned root).
        let dir = std::env::temp_dir().join(format!("qrdb-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::os::unix::fs::symlink(dir.join("sub"), dir.join("sub").join("loop")).unwrap();
        std::os::unix::fs::symlink("/nonexistent-target", dir.join("dangling")).unwrap();
        let index = QrIndex::scan(&dir).unwrap();
        assert!(index.records.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_reads_headers_only() {
        let dir = std::env::temp_dir().join(format!("qrdb-header-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut ds = DataSet::new();
        ds.put_text(tags::SOP_CLASS_UID, Vr::Ui, sop_class::CT_IMAGE);
        ds.put_text(tags::SOP_INSTANCE_UID, Vr::Ui, "1.2.9.1");
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "PH");
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, vec![0xABu8; 1024]);
        let meta = vyna_dcm_codec::FileMeta::new(
            sop_class::CT_IMAGE,
            "1.2.9.1",
            vyna_dcm_core::TransferSyntax::ExplicitVrLittleEndian,
        );
        let bytes = vyna_dcm_codec::write_part10(&meta, &ds).unwrap();
        std::fs::write(dir.join("px.dcm"), bytes).unwrap();
        let index = QrIndex::scan(&dir).unwrap();
        assert_eq!(index.records.len(), 1);
        let got = &index.records[0].dataset;
        // Header keys indexed, pixel bytes never decoded.
        assert_eq!(got.string(tags::PATIENT_ID), Some("PH"));
        assert!(got.get(tags::PIXEL_DATA).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
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

    fn commit_index() -> QrIndex {
        let mut held = DataSet::new();
        held.put_uid(tags::SOP_CLASS_UID, vyna_dcm_core::sop_class::CT_IMAGE);
        held.put_uid(tags::SOP_INSTANCE_UID, "1.2.1.1.1");
        held.put_uid(tags::STUDY_INSTANCE_UID, "1.2.1");
        QrIndex {
            records: vec![QrRecord {
                path: PathBuf::from("1.2.1.1.1.dcm"),
                dataset: held,
            }],
            skipped: 0,
        }
    }

    #[test]
    fn commitment_holds_partition() {
        use crate::worklist::{parse_commitment_result, storage_commitment_request};
        let scp = StgCommitScp::new(commit_index());
        let ct = vyna_dcm_core::sop_class::CT_IMAGE;
        let (req, txn) = storage_commitment_request(&[(ct, "1.2.1.1.1"), (ct, "9.9.9")]);
        let (s, _, follow_ups) = scp.commit(
            Some("1.9.1"),
            Some(1),
            crate::worklist::STORAGE_COMMITMENT_PUSH,
            Some(req),
        );
        assert_eq!(s, status::SUCCESS);
        assert_eq!(follow_ups.len(), 1);
        let crate::service::ScpFollowUp::EventReport {
            sop_class_uid,
            affected_sop_instance_uid,
            event_type_id,
            dataset,
        } = &follow_ups[0];
        assert_eq!(sop_class_uid, crate::worklist::STORAGE_COMMITMENT_PUSH);
        assert_eq!(affected_sop_instance_uid, "1.9.1");
        assert_eq!(*event_type_id, 1);
        let report = dataset.as_ref().unwrap();
        let (t, ok, failed) = parse_commitment_result(report);
        assert_eq!(t, Some(txn));
        assert_eq!(ok, vec![(ct.to_string(), "1.2.1.1.1".to_string())]);
        assert_eq!(failed, vec![(ct.to_string(), "9.9.9".to_string())]);
        let ng = &report.sequence(tags::FAILED_SOP_SEQUENCE).unwrap()[0];
        assert_eq!(ng.uint16(tags::FAILURE_REASON), Some(0x0112));
        assert_eq!(scp.study_uid_for("1.2.1.1.1"), Some("1.2.1".to_string()));
        assert_eq!(scp.study_uid_for("9.9.9"), None);
    }

    #[test]
    fn commitment_rejects_bad_requests() {
        use crate::worklist::storage_commitment_request;
        let scp = StgCommitScp::new(commit_index());
        let ct = vyna_dcm_core::sop_class::CT_IMAGE;
        let (req, _) = storage_commitment_request(&[(ct, "1.2.1.1.1")]);
        assert_eq!(
            scp.commit(Some("1.9.1"), Some(1), "1.2.9.9", Some(req.clone()))
                .0,
            status::SOP_CLASS_NOT_SUPPORTED
        );
        assert_eq!(
            scp.commit(
                Some("1.9.1"),
                Some(2),
                crate::worklist::STORAGE_COMMITMENT_PUSH,
                Some(req.clone())
            )
            .0,
            status::UNRECOGNIZED_OPERATION
        );
        assert_eq!(
            scp.commit(
                Some("../x"),
                Some(1),
                crate::worklist::STORAGE_COMMITMENT_PUSH,
                Some(req.clone())
            )
            .0,
            status::MISTYPED_ARGUMENT
        );
        assert_eq!(
            scp.commit(
                None,
                Some(1),
                crate::worklist::STORAGE_COMMITMENT_PUSH,
                Some(req.clone())
            )
            .0,
            status::MISTYPED_ARGUMENT
        );
        assert_eq!(
            scp.commit(
                Some("1.9.1"),
                Some(1),
                crate::worklist::STORAGE_COMMITMENT_PUSH,
                None
            )
            .0,
            status::DATASET_MISMATCH
        );
        assert_eq!(
            scp.commit(
                Some("1.9.1"),
                Some(1),
                crate::worklist::STORAGE_COMMITMENT_PUSH,
                Some(DataSet::new())
            )
            .0,
            status::DATASET_MISMATCH
        );
    }
}
