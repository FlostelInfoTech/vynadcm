//! DICOMweb server core: framework-agnostic dispatch + in-memory store.
//!
//! [`dispatch`] routes an [`HttpRequest`] to an [`HttpResponse`] against any
//! [`DicomWebStore`]. The `server` feature adds a hyper HTTP/1.1 front end
//! ([`crate::hyper_server`]); embedders can also adapt `dispatch` to their
//! own HTTP stack (as VynaLink does).

use bytes::Bytes;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use vyna_dcm_codec::{
    FileMeta, dataset_to_json, decode_pixels, read_part10, read_part10_header, write_part10,
};
use vyna_dcm_core::{DataSet, Tag, TransferSyntax, Value, Vr, lookup, lookup_keyword, tags};

use crate::Error;
use crate::multipart::{
    Part, decode_multipart, encode_multipart, is_dicom_media_type, new_boundary, parse_content_type,
};

/// Tags treated as bulk data (stripped from QIDO, `BulkDataURI` in metadata).
pub const BULK_TAGS: [Tag; 5] = [
    tags::PIXEL_DATA,
    tags::FLOAT_PIXEL_DATA,
    tags::DOUBLE_FLOAT_PIXEL_DATA,
    tags::WAVEFORM_DATA,
    tags::ENCAPSULATED_DOCUMENT,
];

/// Framework-agnostic HTTP request.
#[derive(Clone)]
pub struct HttpRequest {
    /// Uppercase method (`GET`, `POST`, ...).
    pub method: String,
    /// Path without query (`/studies/...`).
    pub path: String,
    /// Raw query string (no leading `?`).
    pub query: String,
    /// Headers with lowercased names.
    pub headers: HashMap<String, String>,
    /// Body bytes (shared buffer: front ends hand over their bytes
    /// without copying, e.g. multi-hundred-MB STOW uploads).
    pub body: Bytes,
}

// Manual `Debug`: embedders log requests, so credentials stay out
// (`Authorization`, session cookies) and the (possibly
// PHI-carrying, possibly huge) body shows as a length only.
impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: HashMap<&str, &str> = self
            .headers
            .iter()
            .map(|(k, v)| {
                let v = if matches!(
                    k.as_str(),
                    "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
                ) {
                    "<redacted>"
                } else {
                    v.as_str()
                };
                (k.as_str(), v)
            })
            .collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("query", &self.query)
            .field("headers", &headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Framework-agnostic HTTP response.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Body bytes (shared: bulk/instance payloads are not copied
    /// per response).
    pub body: Bytes,
}

impl HttpResponse {
    /// JSON response.
    #[must_use]
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/dicom+json".into())],
            body: Bytes::from(serde_json::to_vec(value).unwrap_or_default()),
        }
    }

    /// Plain-text response.
    #[must_use]
    pub fn text(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: Bytes::from(message.into().into_bytes()),
        }
    }

    /// `multipart/related` response.
    ///
    /// Fails with [`Error::Multipart`] when a part header carries
    /// CR/LF (framing injection) — embedder-supplied parts are not
    /// trusted.
    pub fn multipart(parts: &[Part], root_type: &str) -> crate::Result<Self> {
        let (content_type, body) = encode_multipart(parts, &new_boundary(), root_type)
            .map_err(|e| Error::Multipart(e.to_string()))?;
        Ok(Self {
            status: 200,
            headers: vec![("content-type".into(), content_type)],
            body: Bytes::from(body),
        })
    }

    /// `405 Method Not Allowed` with the `Allow` header RFC 9110
    /// §15.5.5 wants (this API speaks GET and POST only).
    #[must_use]
    pub fn method_not_allowed() -> Self {
        Self {
            status: 405,
            headers: vec![
                ("content-type".into(), "text/plain".into()),
                ("allow".into(), "GET, POST".into()),
            ],
            body: Bytes::from_static(b"method not allowed"),
        }
    }

    /// Raw-bytes response.
    #[must_use]
    pub fn bytes(status: u16, content_type: impl Into<String>, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body: body.into(),
        }
    }
}

/// QIDO search scope resolved from the request path.
#[derive(Clone, Debug)]
pub struct QidoScope {
    /// Response level: `STUDY`, `SERIES` or `IMAGE`.
    pub level: &'static str,
    /// Study filter from the path, when nested.
    pub study: Option<String>,
    /// Series filter from the path, when nested.
    pub series: Option<String>,
}

/// Extracted bulk blob awaiting a `BulkDataURI` id.
#[derive(Clone, Debug)]
pub struct BulkBlob {
    /// Source tag.
    pub tag: Tag,
    /// Source VR.
    pub vr: Vr,
    /// Blob bytes.
    pub bytes: Vec<u8>,
    /// Blob content type.
    pub content_type: String,
}

/// Stored instance ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredIds {
    /// Study Instance UID.
    pub study: String,
    /// Series Instance UID.
    pub series: String,
    /// SOP Instance UID.
    pub instance: String,
    /// SOP Class UID (dataset, else file meta): feeds the STOW
    /// response `ReferencedSOPSequence`.
    pub sop_class: String,
}

/// DICOMweb storage backend.
pub trait DicomWebStore: Send + Sync {
    /// QIDO search with `(tag, match_value)` filters; empty match values
    /// are return keys. Applies limit/offset.
    fn search(
        &self,
        scope: &QidoScope,
        filters: &[(Tag, String)],
        limit: usize,
        offset: usize,
    ) -> Vec<DataSet>;

    /// Full Part-10 bytes of an instance (shared: retrieving must
    /// not copy a multi-hundred-MB file per GET).
    fn instance_file(&self, study: &str, series: &str, instance: &str) -> Option<Bytes>;

    /// All instance files of a study in store order (RetrieveStudy,
    /// PS3.18 §6.5.3); empty when the study is unknown.
    fn study_files(&self, study: &str) -> Vec<Bytes>;

    /// All instance files of one series in store order
    /// (RetrieveSeries, PS3.18 §6.5.4); empty when unknown.
    fn series_files(&self, study: &str, series: &str) -> Vec<Bytes>;

    /// Metadata dataset (bulk elements removed) plus extracted blobs.
    fn instance_metadata(
        &self,
        study: &str,
        series: &str,
        instance: &str,
    ) -> Option<(DataSet, Vec<BulkBlob>)>;

    /// Raw native-order frame bytes for 1-based frame numbers.
    ///
    /// `Ok(None)` means the instance (or a requested frame number) is
    /// absent; `Err` means the instance exists but is undecodable
    /// (no pixels, garbage frame count, length mismatch) — the
    /// dispatcher maps those to 404 and 500 respectively.
    fn instance_frames(
        &self,
        study: &str,
        series: &str,
        instance: &str,
        frames: &[u32],
    ) -> std::result::Result<Option<Vec<Vec<u8>>>, String>;

    /// Store one Part-10 file; `study_hint` (STOW URL) must match the file.
    ///
    /// Takes the file by value as shared [`Bytes`]: STOW moves each
    /// decoded part straight into the store instead of copying the
    /// upload a second time.
    fn store_instance(
        &self,
        file: Bytes,
        study_hint: Option<&str>,
    ) -> std::result::Result<StoredIds, String>;

    /// Register blobs, returning one id per blob for `BulkDataURI`s.
    /// Implementations MUST return exactly `blobs.len()` ids:
    /// anything else makes the metadata endpoint answer 500.
    fn put_bulk(&self, blobs: Vec<(Bytes, String)>) -> Vec<String>;

    /// Resolve a bulk id to `(bytes, content_type)`.
    fn bulk_data(&self, id: &str) -> Option<(Bytes, String)>;
}

/// Thread-safe in-memory [`DicomWebStore`] (tests, tools, small archives).
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    instances: VecDeque<StoredInstance>,
    /// Total instance file bytes currently held.
    instance_bytes: usize,
    bulk: HashMap<String, (Bytes, String)>,
    /// Insertion order for oldest-first bulk eviction.
    bulk_order: VecDeque<String>,
    /// Total bulk bytes currently held.
    bulk_bytes: usize,
}

/// Oldest bulk blobs are evicted past these bounds (every metadata
/// GET registers fresh blobs, so an unbounded store is a DoS).
const MAX_BULK_BLOBS: usize = 10_000;
/// See [`MAX_BULK_BLOBS`].
const MAX_BULK_BYTES: usize = 1024 * 1024 * 1024;

/// Oldest instances are evicted past these bounds (STOW is
/// unauthenticated by default, so an unbounded store is a DoS).
const MAX_INSTANCES: usize = 10_000;
/// See [`MAX_INSTANCES`] (counts stored file bytes).
const MAX_INSTANCE_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
struct StoredInstance {
    study: String,
    series: String,
    instance: String,
    file: Bytes,
    dataset: DataSet,
    transfer_syntax: TransferSyntax,
}

impl MemoryStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the store, recovering from a poisoned mutex instead of
    /// panicking: one panicked request handler must not wedge the
    /// whole server.
    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of stored instances.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().instances.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl DicomWebStore for MemoryStore {
    fn search(
        &self,
        scope: &QidoScope,
        filters: &[(Tag, String)],
        limit: usize,
        offset: usize,
    ) -> Vec<DataSet> {
        if limit == 0 {
            return Vec::new();
        }
        let inner = self.lock();
        let mut out = Vec::new();
        // Study-level responses de-duplicate by Study Instance UID.
        let mut seen_studies = std::collections::HashSet::new();
        let mut seen_series = std::collections::HashSet::new();
        // NOTE: offset applies to *matched* rows below, never to the raw
        // store order — skipping here would page unfiltered instances.
        // Skipped rows are counted, never cloned: a huge offset must
        // not clone the whole match set into a transient.
        let mut seen = 0usize;
        for inst in inner.instances.iter() {
            if let Some(s) = &scope.study
                && inst.study != *s
            {
                continue;
            }
            if let Some(s) = &scope.series
                && inst.series != *s
            {
                continue;
            }
            if !filters
                .iter()
                .all(|(t, m)| match_attribute(&inst.dataset, *t, m))
            {
                continue;
            }
            // Dedup AFTER the match: an unmatched first instance must
            // not claim its study/series and hide a later match. Skipped
            // rows still claim, so paging windows are stable.
            if scope.level == "STUDY" && !seen_studies.insert(inst.study.clone()) {
                continue;
            }
            if scope.level == "SERIES"
                && !seen_series.insert((inst.study.clone(), inst.series.clone()))
            {
                continue;
            }
            if seen < offset {
                seen += 1;
                continue;
            }
            if out.len() >= limit {
                break;
            }
            // Bulk never clones: strip-before-clone, so a paged QIDO
            // over big-pixel instances holds no pixel transient.
            out.push(clone_without_bulk(&inst.dataset));
            seen += 1;
        }
        out
    }

    fn instance_file(&self, study: &str, series: &str, instance: &str) -> Option<Bytes> {
        let inner = self.lock();
        inner
            .instances
            .iter()
            .find(|i| i.study == study && i.series == series && i.instance == instance)
            .map(|i| i.file.clone())
    }

    fn study_files(&self, study: &str) -> Vec<Bytes> {
        self.lock()
            .instances
            .iter()
            .filter(|i| i.study == study)
            .map(|i| i.file.clone())
            .collect()
    }

    fn series_files(&self, study: &str, series: &str) -> Vec<Bytes> {
        self.lock()
            .instances
            .iter()
            .filter(|i| i.study == study && i.series == series)
            .map(|i| i.file.clone())
            .collect()
    }

    fn instance_metadata(
        &self,
        study: &str,
        series: &str,
        instance: &str,
    ) -> Option<(DataSet, Vec<BulkBlob>)> {
        let inner = self.lock();
        let inst = inner
            .instances
            .iter()
            .find(|i| i.study == study && i.series == series && i.instance == instance)?;
        let mut ds = inst.dataset.clone();
        let blobs = extract_bulk(&mut ds);
        Some((ds, blobs))
    }

    fn instance_frames(
        &self,
        study: &str,
        series: &str,
        instance: &str,
        frames: &[u32],
    ) -> std::result::Result<Option<Vec<Vec<u8>>>, String> {
        // Snapshot under the lock, decode after it: pixel decode is
        // CPU-bound and must not serialize the whole store.
        let snap = {
            let inner = self.lock();
            inner
                .instances
                .iter()
                .find(|i| i.study == study && i.series == series && i.instance == instance)
                .map(|i| (i.dataset.clone(), i.transfer_syntax))
        };
        let Some((dataset, transfer_syntax)) = snap else {
            return Ok(None);
        };
        let pixels = decode_pixels(&dataset, transfer_syntax)
            .map_err(|e| format!("cannot decode pixels: {e}"))?;
        // Absent frame count means single-frame; present-but-garbage
        // is undecodable (500), not a license to serve all frames.
        let count: usize = match dataset.string(tags::NUMBER_OF_FRAMES) {
            None => 1,
            Some(s) => match s.parse() {
                Ok(n) if n >= 1 => n,
                _ => {
                    return Err(format!("cannot decode frames: bad NumberOfFrames {s:?}"));
                }
            },
        };
        if pixels.len() % count != 0 {
            return Err(format!(
                "cannot decode frames: {} pixel bytes not divisible by {count} frames",
                pixels.len()
            ));
        }
        let frame_len = pixels.len() / count;
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            if *f == 0 || *f as usize > count {
                return Ok(None);
            }
            let start = (*f as usize - 1) * frame_len;
            out.push(pixels[start..start + frame_len].to_vec());
        }
        Ok(Some(out))
    }

    fn store_instance(
        &self,
        file: Bytes,
        study_hint: Option<&str>,
    ) -> std::result::Result<StoredIds, String> {
        // A single file over the byte cap would evict the whole store
        // (including itself) and still report success; refuse it.
        // This also bounds bulk blobs: blobs are extracted from stored
        // instances, so none can exceed the cap either.
        if file.len() > MAX_INSTANCE_BYTES {
            return Err(format!(
                "instance ({} bytes) exceeds the store cap of {MAX_INSTANCE_BYTES} bytes",
                file.len()
            ));
        }
        let (meta, dataset) = read_part10(&file).map_err(|e| e.to_string())?;
        let study = dataset
            .string(tags::STUDY_INSTANCE_UID)
            .ok_or("missing Study Instance UID")?
            .to_string();
        let series = dataset
            .string(tags::SERIES_INSTANCE_UID)
            .ok_or("missing Series Instance UID")?
            .to_string();
        let instance = dataset
            .string(tags::SOP_INSTANCE_UID)
            .ok_or("missing SOP Instance UID")?
            .to_string();
        // File meta always carries the class (`read_part10` requires
        // it), so the STOW response refs never go without.
        let sop_class = dataset
            .string(tags::SOP_CLASS_UID)
            .map(str::to_string)
            .unwrap_or_else(|| meta.media_storage_sop_class_uid.clone());
        if let Some(hint) = study_hint
            && hint != study
        {
            return Err(format!(
                "STOW study {hint} does not match file study {study}"
            ));
        }
        let ts = meta.transfer_syntax().map_err(|e| e.to_string())?;
        let mut inner = self.lock();
        // Replace on re-store (same SOP Instance UID).
        if let Some(pos) = inner.instances.iter().position(|i| i.instance == instance)
            && let Some(old) = inner.instances.remove(pos)
        {
            inner.instance_bytes = inner.instance_bytes.saturating_sub(old.file.len());
        }
        // Saturating: plain `+=` wraps (release) or panics (debug) on
        // 32-bit / huge direct-dispatch inputs, corrupting eviction.
        inner.instance_bytes = inner.instance_bytes.saturating_add(file.len());
        inner.instances.push_back(StoredInstance {
            study: study.clone(),
            series: series.clone(),
            instance: instance.clone(),
            file,
            dataset,
            transfer_syntax: ts,
        });
        while inner.instances.len() > MAX_INSTANCES || inner.instance_bytes > MAX_INSTANCE_BYTES {
            let Some(oldest) = inner.instances.pop_front() else {
                break;
            };
            inner.instance_bytes = inner.instance_bytes.saturating_sub(oldest.file.len());
        }
        Ok(StoredIds {
            study,
            series,
            instance,
            sop_class,
        })
    }

    fn put_bulk(&self, blobs: Vec<(Bytes, String)>) -> Vec<String> {
        let mut inner = self.lock();
        blobs
            .into_iter()
            .map(|(bytes, ct)| {
                // Unpredictable ids: sequential hex ids let anyone
                // enumerate every stored blob.
                let id = uuid::Uuid::new_v4().as_simple().to_string();
                // Saturating (see `store_instance`): never wrap eviction.
                inner.bulk_bytes = inner.bulk_bytes.saturating_add(bytes.len());
                inner.bulk_order.push_back(id.clone());
                inner.bulk.insert(id.clone(), (bytes, ct));
                while inner.bulk.len() > MAX_BULK_BLOBS || inner.bulk_bytes > MAX_BULK_BYTES {
                    let Some(oldest) = inner.bulk_order.pop_front() else {
                        break;
                    };
                    if let Some((b, _)) = inner.bulk.remove(&oldest) {
                        inner.bulk_bytes = inner.bulk_bytes.saturating_sub(b.len());
                    }
                }
                id
            })
            .collect()
    }

    fn bulk_data(&self, id: &str) -> Option<(Bytes, String)> {
        self.lock().bulk.get(id).cloned()
    }
}

/// Remove bulk elements, returning the extracted blobs.
pub fn extract_bulk(ds: &mut DataSet) -> Vec<BulkBlob> {
    let mut blobs = Vec::new();
    for tag in BULK_TAGS {
        if let Some(el) = ds.remove(tag) {
            let bytes = match &el.value {
                Value::Bytes(b) => Some(b.clone()),
                Value::Fragments(f) => Some(f.joined()),
                _ => None,
            };
            if let Some(bytes) = bytes {
                let content_type = if tag == tags::ENCAPSULATED_DOCUMENT {
                    // MIME Type of Encapsulated Document names CDA/XML
                    // payloads; absent or malformed means legacy PDF.
                    ds.string(tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT)
                        .map(str::trim)
                        .filter(|m| {
                            !m.is_empty()
                                && m.contains('/')
                                && !m.chars().any(|c| {
                                    c.is_whitespace() || c.is_control() || c == ';' || c == ','
                                })
                        })
                        .unwrap_or("application/pdf")
                        .to_string()
                } else {
                    "application/octet-stream".to_string()
                };
                blobs.push(BulkBlob {
                    tag,
                    vr: el.vr,
                    bytes,
                    content_type,
                });
            } else {
                ds.insert(el); // unexpected shape; leave in place
            }
        }
    }
    blobs
}

/// Remove bulk elements, discarding the bytes (QIDO responses).
pub fn strip_bulk(ds: &mut DataSet) {
    for tag in BULK_TAGS {
        ds.remove(tag);
    }
}

/// Clone a dataset minus bulk elements (QIDO responses): cloning
/// first and stripping after would transiently duplicate megabytes
/// of pixel data per match (a whole-store transient under paging).
fn clone_without_bulk(ds: &DataSet) -> DataSet {
    let mut out = DataSet::new();
    for (tag, el) in ds.iter() {
        if BULK_TAGS.contains(&tag) {
            continue;
        }
        out.insert(el.clone());
    }
    out
}

/// Maximum bytes of one stored value scanned by a single QIDO
/// comparison. Stored values are attacker-shaped (1 GiB STOW
/// uploads), so an uncapped `value` would turn every `?<Tag>=*`
/// query into a multi-GB `Vec<char>` per instance (OOM-abort). Past
/// the budget the value is a non-match; 64 KiB is orders of
/// magnitude past real key values.
const MAX_MATCH_VALUE_BYTES: usize = 64 * 1024;

/// Maximum pattern bytes scanned by one wildcard comparison (the
/// whole QIDO query string is already capped at
/// [`MAX_QIDO_QUERY_BYTES`]; this bounds one comma-separated
/// alternative defensively).
const MAX_MATCH_PATTERN_BYTES: usize = 64 * 1024;

/// Maximum matcher steps per wildcard comparison. Backtracking is
/// quadratic in the worst case, so a star-heavy pattern against a
/// long value bails out (non-match) instead of burning CPU; real
/// key matches finish in hundreds of steps.
const MAX_WILDCARD_STEPS: u64 = 1_000_000;

/// Maximum total value bytes scanned per element per pattern
/// alternative: a huge-VM element must not burn unbounded CPU per
/// query. Past the budget the remaining values are non-matches.
const MAX_MATCH_SCAN_BYTES: usize = 256 * 1024;

/// Decode the char at byte index `i` (which must sit on a char
/// boundary) plus the next boundary.
fn char_at(s: &str, i: usize) -> (char, usize) {
    let c = s[i..].chars().next().unwrap_or('\0');
    (c, i + c.len_utf8())
}

/// PS3.4 wildcard match: `*` spans any run (empty included),
/// `?` matches exactly one character. Matching runs on `char`s so
/// multibyte values stay on character boundaries, but without
/// building `Vec<char>` copies: byte indices walk the borrowed
/// strings. Oversized inputs and step-budget exhaustion report
/// non-match (see the `MAX_MATCH_*` budgets).
fn wildcard_match(value: &str, pattern: &str, case_insensitive: bool) -> bool {
    if value.len() > MAX_MATCH_VALUE_BYTES || pattern.len() > MAX_MATCH_PATTERN_BYTES {
        return false;
    }
    let fold = |c: char| {
        if case_insensitive {
            c.to_ascii_lowercase()
        } else {
            c
        }
    };
    let (mut vi, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    let mut steps = 0u64;
    while vi < value.len() {
        steps += 1;
        if steps > MAX_WILDCARD_STEPS {
            return false;
        }
        let (vc, vnext) = char_at(value, vi);
        if pi < pattern.len() {
            let (pc, pnext) = char_at(pattern, pi);
            let pc = fold(pc);
            if pc == '?' || pc == fold(vc) {
                vi = vnext;
                pi = pnext;
                continue;
            }
            if pc == '*' {
                // Collapse the run: consecutive `*` backtrack
                // identically to one, so a star-heavy pattern costs
                // one state instead of one backtrack level per star.
                let mut q = pnext;
                while q < pattern.len() && pattern.as_bytes()[q] == b'*' {
                    q += 1;
                }
                star = Some(q);
                mark = vi;
                pi = q;
                continue;
            }
        }
        if let Some(s) = star {
            pi = s;
            let (_, mnext) = char_at(value, mark);
            mark = mnext;
            vi = mark;
        } else {
            return false;
        }
    }
    // Trailing stars (a lone run may remain past the loop).
    while pi < pattern.len() {
        steps += 1;
        if steps > MAX_WILDCARD_STEPS {
            return false;
        }
        let (pc, pnext) = char_at(pattern, pi);
        if pc != '*' {
            return false;
        }
        pi = pnext;
    }
    true
}

/// Visit each string form of `value` until `f` hits or the scan
/// budgets exhaust. Text borrows straight out of the dataset;
/// names, tags, and numerics format on demand one at a time — the
/// old code cloned the whole `Vec<String>` (megabytes per instance)
/// before comparing. Oversized values are non-matches; past
/// [`MAX_MATCH_SCAN_BYTES`] the rest are too.
fn any_value_str(value: &Value, mut f: impl FnMut(&str) -> bool) -> bool {
    let mut scanned = 0usize;
    // `Ok(hit)` keeps or stops the scan; `Err(())` means the total
    // budget is out (stop, report no hit).
    let mut visit = |s: &str, scanned: &mut usize| -> std::result::Result<bool, ()> {
        if s.len() > MAX_MATCH_VALUE_BYTES {
            return Ok(false);
        }
        *scanned = scanned.saturating_add(s.len());
        if *scanned > MAX_MATCH_SCAN_BYTES {
            return Err(());
        }
        Ok(f(s))
    };
    macro_rules! scan_borrowed {
        ($vec:expr) => {
            for s in $vec {
                match visit(s, &mut scanned) {
                    Ok(true) => return true,
                    Ok(false) => {}
                    Err(()) => return false,
                }
            }
        };
    }
    macro_rules! scan_owned {
        ($vec:expr, |$x:ident| $fmt:expr) => {
            for $x in $vec {
                let owned = $fmt;
                match visit(&owned, &mut scanned) {
                    Ok(true) => return true,
                    Ok(false) => {}
                    Err(()) => return false,
                }
            }
        };
    }
    match value {
        Value::Text(v) => scan_borrowed!(v),
        Value::PersonName(n) => scan_owned!(n, |p| p.to_dicom_string()),
        Value::U16(v) => scan_owned!(v, |x| x.to_string()),
        Value::U32(v) => scan_owned!(v, |x| x.to_string()),
        Value::I16(v) => scan_owned!(v, |x| x.to_string()),
        Value::I32(v) => scan_owned!(v, |x| x.to_string()),
        Value::U64(v) => scan_owned!(v, |x| x.to_string()),
        Value::I64(v) => scan_owned!(v, |x| x.to_string()),
        Value::F32(v) => scan_owned!(v, |x| x.to_string()),
        Value::F64(v) => scan_owned!(v, |x| x.to_string()),
        Value::Tags(v) => scan_owned!(v, |t| format!("{:04X}{:04X}", t.group(), t.element())),
        _ => return false,
    }
    false
}

/// Case-insensitive keyword/tag matching for one QIDO filter.
///
/// Values are visited borrowed-and-lazy ([`any_value_str`]): no
/// whole-element clone before comparing.
fn match_attribute(ds: &DataSet, tag: Tag, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true; // universal match / return key
    }
    let Some(el) = ds.get(tag) else { return false };
    // Comma-separated list = OR; for DA/TM/DT/DS/IS, `a-b` is an
    // inclusive range (numeric for DS/IS, lexicographic for the
    // fixed-width datetimes); `*`/`?` anywhere = wildcard match;
    // otherwise exact (case-insensitive for PN). A `-` in any other
    // VR is a literal character, not a range (mirrors C-FIND matching).
    let range_vr = matches!(el.vr, Vr::Da | Vr::Tm | Vr::Dt | Vr::Ds | Vr::Is);
    for alt in pattern.split(',') {
        let alt = alt.trim();
        if alt.is_empty() {
            continue;
        }
        if range_vr && alt.contains('-') {
            // Datetimes never start with '-'; numerics might ("-5--1").
            let dash = if matches!(el.vr, Vr::Ds | Vr::Is) {
                alt.get(1..).and_then(|rest| rest.find('-').map(|i| i + 1))
            } else {
                alt.find('-')
            };
            if let Some(d) = dash {
                // d is a byte index of ASCII '-', so both slices land
                // on char boundaries.
                let (lo, hi) = (&alt[..d], &alt[d + 1..]);
                if matches!(el.vr, Vr::Ds | Vr::Is) {
                    let bound = |s: &str, open: f64| {
                        if s.is_empty() {
                            Some(open)
                        } else {
                            s.parse().ok()
                        }
                    };
                    if let (Some(lo), Some(hi)) =
                        (bound(lo, f64::NEG_INFINITY), bound(hi, f64::INFINITY))
                    {
                        if any_value_str(&el.value, |v| {
                            v.parse::<f64>().is_ok_and(|v| v >= lo && v <= hi)
                        }) {
                            return true;
                        }
                        continue;
                    }
                    // Unparsable bound ("1e-3", "abc-def"): not a
                    // range — fall through to literal/wildcard below
                    // so exact queries still match.
                } else if any_value_str(&el.value, |v| {
                    (lo.is_empty() || v >= lo) && (hi.is_empty() || v <= hi)
                }) {
                    return true;
                } else {
                    continue;
                }
            }
            // Lone leading '-' (a negative literal), or an unparsable
            // DS/IS bound: fall through to wildcard/exact below.
        }
        // PS3.4 wildcards: `*` matches any run (empty included) and
        // `?` any single character, anywhere in the pattern — not just
        // a trailing `*`. PN matching stays case-insensitive.
        if alt.contains(['*', '?']) {
            let ci = el.vr == Vr::Pn;
            if any_value_str(&el.value, |v| wildcard_match(v, alt, ci)) {
                return true;
            }
            continue;
        }
        if el.vr == Vr::Pn {
            if any_value_str(&el.value, |v| v.eq_ignore_ascii_case(alt)) {
                return true;
            }
        } else if any_value_str(&el.value, |v| v == alt) {
            return true;
        }
    }
    false
}

/// True when the request's `Accept` header allows one of `kinds`.
/// Missing header means anything goes; matching is case-insensitive
/// over bare media types (other `;` parameters ignored) with `*/*`
/// and `type/*` wildcards honored. A media range with `q=0` (RFC
/// 7231 §5.3.2: "not acceptable") never allows, even when the type
/// matches.
fn accepts(req: &HttpRequest, kinds: &[&str]) -> bool {
    let Some(accept) = req.headers.get("accept") else {
        return true;
    };
    accept.split(',').any(|item| {
        let mut parts = item.split(';');
        let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let refused = parts.any(|p| {
            let p = p.trim();
            let Some(q) = p
                .strip_prefix("q=")
                .or_else(|| p.strip_prefix("Q="))
                .map(str::trim)
            else {
                return false;
            };
            // RFC 7231 weight: `0`, `0.0`, `0.00`, `0.000` all mean
            // "not acceptable". Anything else (including garbage)
            // leaves the range acceptable.
            q.bytes().all(|b| b == b'0' || b == b'.')
                && !q.is_empty()
                && q.parse::<f32>().is_ok_and(|w| w == 0.0)
        });
        if refused {
            return false;
        }
        if mime == "*/*" {
            return true;
        }
        kinds.iter().any(|k| {
            mime == *k
                || mime
                    .strip_suffix("/*")
                    .is_some_and(|t| k.split('/').next() == Some(t))
        })
    })
}

/// Maximum request-path bytes routed by [`dispatch`]. The longest
/// legitimate path is a 10k-entry WADO frame list (~50 KiB); past
/// this the request is `414` without further parsing. Direct
/// `dispatch` embedders have no URI cap of their own, so this is
/// enforced here rather than at the HTTP front end.
const MAX_PATH_BYTES: usize = 256 * 1024;

/// Maximum path segments collected by [`dispatch`]. The deepest
/// route (`.../instances/{i}/frames/{list}`) has 8; past this the
/// path cannot match any route, so it is `404` without building the
/// segment vector (a million-slash path must not allocate a
/// million-entry `Vec`).
const MAX_PATH_SEGMENTS: usize = 32;

/// Strip the deployment path prefix carried by `public_base` (e.g.
/// `https://host/dicomweb`) from `path`, on a segment boundary only
/// (so `/dicomweb2/studies` never collapses onto `/studies`).
/// Shared by [`dispatch`] and the hyper front end's pre-body route
/// check so the two can never drift apart.
pub(crate) fn strip_base_prefix<'a>(public_base: &str, path: &'a str) -> &'a str {
    let base = public_base.trim_end_matches('/');
    let prefix = base
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..]))
        .unwrap_or("");
    if prefix.is_empty() {
        path
    } else if path == prefix {
        "/"
    } else if let Some(rest) = path.strip_prefix(prefix)
        && rest.starts_with('/')
    {
        rest
    } else {
        path
    }
}

/// Route one request against a store. `public_base` is the externally
/// visible base URL used for `BulkDataURI`s and RetrieveURLs; when it
/// carries a path prefix (e.g. `https://host/dicomweb`), that prefix is
/// stripped from request paths before routing (bare-root deployments
/// pass through unchanged).
///
/// QIDO matching is exact plus PS3.4 wildcards (`*`/`?` anywhere in
/// the pattern), ranges, and OR lists: `includefield` and
/// `fuzzymatching` parameters are accepted but ignored — responses
/// always carry full key sets and matching never fuzzes.
///
/// WADO-RS honors `Accept` media types (`406` when nothing offered is
/// servable) but ignores `type`/`transfer-syntax` parameters: frames
/// serve decoded bytes and instances serve stored bytes — no
/// transcoding is negotiated. QIDO always serves
/// `application/dicom+json`.
pub fn dispatch<S: DicomWebStore>(store: &S, public_base: &str, req: &HttpRequest) -> HttpResponse {
    let base = public_base.trim_end_matches('/');
    if req.path.len() > MAX_PATH_BYTES {
        return HttpResponse::text(414, "URI too long");
    }
    let path = strip_base_prefix(public_base, &req.path);
    // Count-then-collect: paths past MAX_PATH_SEGMENTS 404 without
    // ever building the oversized vector.
    let mut segs: Vec<&str> = Vec::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if segs.len() >= MAX_PATH_SEGMENTS {
            return HttpResponse::text(404, "unknown DICOMweb resource");
        }
        segs.push(seg);
    }
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["studies"]) => qido(
            store,
            req,
            QidoScope {
                level: "STUDY",
                study: None,
                series: None,
            },
            &req.query,
        ),
        ("GET", ["series"]) => qido(
            store,
            req,
            QidoScope {
                level: "SERIES",
                study: None,
                series: None,
            },
            &req.query,
        ),
        ("GET", ["instances"]) => qido(
            store,
            req,
            QidoScope {
                level: "IMAGE",
                study: None,
                series: None,
            },
            &req.query,
        ),
        ("GET", ["studies", study, "series"]) => qido(
            store,
            req,
            QidoScope {
                level: "SERIES",
                study: Some((*study).to_string()),
                series: None,
            },
            &req.query,
        ),
        ("GET", ["studies", study, "series", series, "instances"]) => qido(
            store,
            req,
            QidoScope {
                level: "IMAGE",
                study: Some((*study).to_string()),
                series: Some((*series).to_string()),
            },
            &req.query,
        ),
        ("GET", ["studies", study]) => {
            if !accepts(req, &["application/dicom", "multipart/related"]) {
                return HttpResponse::text(406, "study retrieve needs Accept: application/dicom");
            }
            retrieve_files(store.study_files(study), "study")
        }
        ("GET", ["studies", study, "series", series]) => {
            if !accepts(req, &["application/dicom", "multipart/related"]) {
                return HttpResponse::text(406, "series retrieve needs Accept: application/dicom");
            }
            retrieve_files(store.series_files(study, series), "series")
        }
        ("GET", ["studies", study, "series", series, "instances", instance]) => {
            if !accepts(req, &["application/dicom", "multipart/related"]) {
                return HttpResponse::text(406, "instance needs Accept: application/dicom");
            }
            match store.instance_file(study, series, instance) {
                Some(file) => HttpResponse::multipart(
                    &[Part::new("application/dicom", file)],
                    "application/dicom",
                )
                .unwrap_or_else(|e| HttpResponse::text(500, e.to_string())),
                None => HttpResponse::text(404, "instance not found"),
            }
        }
        (
            "GET",
            [
                "studies",
                study,
                "series",
                series,
                "instances",
                instance,
                "metadata",
            ],
        ) => {
            if !accepts(req, &["application/dicom+json"]) {
                return HttpResponse::text(406, "metadata needs Accept: application/dicom+json");
            }
            match store.instance_metadata(study, series, instance) {
                Some((ds, blobs)) => {
                    // Snapshot the small JSON keys first so the blob bytes
                    // below can move into the store instead of cloning.
                    let keys: Vec<(String, String)> = blobs
                        .iter()
                        .map(|b| {
                            (
                                format!("{:04X}{:04X}", b.tag.group(), b.tag.element()),
                                b.vr.as_str().to_string(),
                            )
                        })
                        .collect();
                    let ids = store.put_bulk(
                        blobs
                            .into_iter()
                            .map(|b| (Bytes::from(b.bytes), b.content_type))
                            .collect(),
                    );
                    // A short id list would silently drop BulkDataURIs
                    // via the zip below; fail loudly instead (custom
                    // store broke the one-id-per-blob contract).
                    if ids.len() != keys.len() {
                        return HttpResponse::text(500, "bulk store returned mismatched ids");
                    }
                    let mut obj = dataset_to_json(&ds);
                    let map = obj.as_object_mut().unwrap();
                    for ((key, vr), id) in keys.iter().zip(ids) {
                        map.insert(
                            key.clone(),
                            serde_json::json!({
                                "vr": vr,
                                "BulkDataURI": format!("{base}/bulk/{id}")
                            }),
                        );
                    }
                    HttpResponse::json(200, &serde_json::Value::Array(vec![obj]))
                }
                None => HttpResponse::text(404, "instance not found"),
            }
        }
        (
            "GET",
            [
                "studies",
                study,
                "series",
                series,
                "instances",
                instance,
                "frames",
                list,
            ],
        ) => {
            if !accepts(req, &["application/octet-stream", "multipart/related"]) {
                return HttpResponse::text(406, "frames need Accept: application/octet-stream");
            }
            // Every addressed frame must be a positive integer: bad
            // entries are a 400 (not silently dropped, and frame 0 is
            // invalid — frames are 1-based). Duplicates are also a 400:
            // each entry embeds a full frame, so `1,1,...` would turn a
            // KB URL into a GB response (direct `dispatch` embedders
            // have no URI cap of their own). The count cap trips
            // *during* the parse (before the duplicate check), so a
            // million-entry list never builds a million-entry `Vec` +
            // `HashSet` first.
            let mut frames = Vec::new();
            let mut seen = HashSet::new();
            for f in list.split(',') {
                match f.parse::<u32>() {
                    Ok(n) if n >= 1 => {
                        if frames.len() >= MAX_WADO_FRAMES {
                            return HttpResponse::text(400, "too many frames");
                        }
                        if !seen.insert(n) {
                            return HttpResponse::text(400, "duplicate frame number");
                        }
                        frames.push(n);
                    }
                    _ => return HttpResponse::text(400, "bad frame list"),
                }
            }
            if frames.is_empty() {
                return HttpResponse::text(400, "bad frame list");
            }
            match store.instance_frames(study, series, instance, &frames) {
                Ok(Some(raw)) => {
                    let parts: Vec<Part> = raw
                        .into_iter()
                        .map(|b| Part::new("application/octet-stream", b))
                        .collect();
                    HttpResponse::multipart(&parts, "application/octet-stream")
                        .unwrap_or_else(|e| HttpResponse::text(500, e.to_string()))
                }
                Ok(None) => HttpResponse::text(404, "instance or frames not found"),
                Err(e) => HttpResponse::text(500, e),
            }
        }
        ("GET", ["bulk", id]) => match store.bulk_data(id) {
            Some((bytes, ct)) => HttpResponse::bytes(200, ct, bytes),
            None => HttpResponse::text(404, "bulk data not found"),
        },
        ("POST", ["studies"]) => stow(store, base, None, req),
        ("POST", ["studies", study]) => stow(store, base, Some(study), req),
        ("GET", _) => HttpResponse::text(404, "unknown DICOMweb resource"),
        _ => HttpResponse::method_not_allowed(),
    }
}

/// Maximum QIDO query parameters: each unknown key scans the whole
/// tag dictionary, so an unbounded list is a CPU DoS.
const MAX_QIDO_PARAMS: usize = 100;

/// Maximum total QIDO query-string bytes (the param count alone does
/// not bound it: one 100 MB filter value would clone through matching
/// against every instance). 64 KiB is orders of magnitude past real
/// queries; direct-`dispatch` embedders have no URI cap of their own,
/// so this is enforced here rather than at the HTTP front end.
const MAX_QIDO_QUERY_BYTES: usize = 64 * 1024;

/// Maximum frames per WADO-RS `/frames/` request. Duplicates are
/// rejected outright (see above); this caps the parse/lookup work of
/// distinct entries. 10,000 covers any real multiframe object.
const MAX_WADO_FRAMES: usize = 10_000;

/// RetrieveStudy/RetrieveSeries body: every instance file as one
/// `application/dicom` part; empty means the study/series is unknown.
fn retrieve_files(files: Vec<Bytes>, what: &str) -> HttpResponse {
    if files.is_empty() {
        return HttpResponse::text(404, format!("{what} not found"));
    }
    let parts: Vec<Part> = files
        .into_iter()
        .map(|f| Part::new("application/dicom", f))
        .collect();
    HttpResponse::multipart(&parts, "application/dicom")
        .unwrap_or_else(|e| HttpResponse::text(500, e.to_string()))
}

fn qido<S: DicomWebStore>(
    store: &S,
    req: &HttpRequest,
    scope: QidoScope,
    query: &str,
) -> HttpResponse {
    // This server answers QIDO in DICOM JSON only, like the WADO
    // paths 406 anything else.
    if !accepts(req, &["application/dicom+json"]) {
        return HttpResponse::text(406, "QIDO needs Accept: application/dicom+json");
    }
    if query.len() > MAX_QIDO_QUERY_BYTES {
        return HttpResponse::text(400, "query too long");
    }
    let params = parse_query(query);
    if params.len() > MAX_QIDO_PARAMS {
        return HttpResponse::text(400, "too many query parameters");
    }
    let mut limit = 100usize;
    let mut offset = 0usize;
    let mut filters = Vec::new();
    for (k, v) in &params {
        match k.to_ascii_lowercase().as_str() {
            "limit" => limit = v.parse().unwrap_or(100).min(1000),
            "offset" => offset = v.parse().unwrap_or(0),
            "includefield" | "fuzzymatching" => {} // accepted, matching stays exact+wildcard (never fuzzy)
            _ => {
                if let Some(tag) = qido_key_to_tag(k) {
                    filters.push((tag, v.clone()));
                }
            }
        }
    }
    let matches = store.search(&scope, &filters, limit, offset);
    let arr: Vec<serde_json::Value> = matches.iter().map(dataset_to_json).collect();
    HttpResponse::json(200, &serde_json::Value::Array(arr))
}

fn qido_key_to_tag(key: &str) -> Option<Tag> {
    if let Some(e) = lookup_keyword(key) {
        return Some(e.tag);
    }
    // Case-insensitive keyword fallback.
    if let Some(e) = vyna_dcm_core::ENTRIES
        .iter()
        .find(|e| e.keyword.eq_ignore_ascii_case(key))
    {
        return Some(e.tag);
    }
    // 8-hex-digit tag in canonical dressings only (`GGGGEEEE`,
    // `GGGG,EEEE`, `(GGGG,EEEE)`): stripping all non-hex first would
    // fabricate tags from arbitrary unknown keys. The tag must also
    // be in the dictionary — unknown spellings are ignored, not
    // treated as filters.
    let t = key.trim().trim_matches(['(', ')']).trim();
    if !t
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ',' || c == ' ')
    {
        return None;
    }
    let hex: String = t.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() == 8 {
        let g = u16::from_str_radix(&hex[0..4], 16).ok()?;
        let e = u16::from_str_radix(&hex[4..8], 16).ok()?;
        let tag = Tag::new(g, e);
        if lookup(tag).is_some() {
            return Some(tag);
        }
    }
    None
}

/// Percent-decode a query string into pairs.
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(p), String::new()),
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn stow<S: DicomWebStore>(
    store: &S,
    base: &str,
    study_hint: Option<&str>,
    req: &HttpRequest,
) -> HttpResponse {
    // This server answers STOW in DICOM JSON only (success and
    // failure bodies alike): 406 before touching the upload.
    if !accepts(req, &["application/dicom+json"]) {
        return HttpResponse::text(406, "STOW needs Accept: application/dicom+json");
    }
    let ct = req
        .headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("");
    let (mime, params) = parse_content_type(ct);
    if mime != "multipart/related" {
        return stow_total_failure(&[]);
    }
    let Some(boundary) = params.get("boundary") else {
        return stow_total_failure(&[]);
    };
    if boundary.is_empty() {
        return stow_total_failure(&[]);
    }
    let parts = match decode_multipart(&req.body, boundary) {
        Ok(p) => p,
        Err(_) => return stow_total_failure(&[]),
    };
    let mut stored: Vec<StoredIds> = Vec::new();
    let mut failures: Vec<(Option<String>, Option<String>, String)> = Vec::new();
    // Media-type match, not string equality: compliant senders add
    // parameters such as `transfer-syntax` (PS3.18 §8.6).
    // The clone is a refcount bump, not a copy: each part body (a
    // zero-copy slice of the request buffer) moves into the store,
    // while the failure path below can still read it for UIDs.
    for part in parts
        .iter()
        .filter(|p| is_dicom_media_type(&p.content_type))
    {
        match store.store_instance(part.body.clone(), study_hint) {
            Ok(ids) => stored.push(ids),
            Err(reason) => {
                let (class, inst) = sop_uids_of(&part.body);
                failures.push((class, inst, reason));
            }
        }
    }
    if stored.is_empty() {
        // Total failure (or zero DICOM parts, when `failures` is
        // empty): 400 in DICOM JSON like every other STOW body —
        // never a joined free-text reason list (unbounded growth:
        // 100k parts × long reasons).
        return stow_total_failure(&failures);
    }
    if !failures.is_empty() {
        // Partial failure: 409 with one FailedSOPSequence item per
        // failed instance (UIDs best-effort: an unparseable part has
        // none to report).
        return HttpResponse::json(
            409,
            &serde_json::json!({
                "00081198": {"vr": "SQ", "Value": failed_sop_items(&failures)},
                "00081199": {"vr": "SQ", "Value": referenced_sop_items(&stored)},
                "00081190": {"vr": "UR", "Value": stow_retrieve_urls(base, &stored)},
            }),
        );
    }
    HttpResponse::json(
        200,
        &serde_json::json!({
            "00081199": {"vr": "SQ", "Value": referenced_sop_items(&stored)},
            "00081190": {"vr": "UR", "Value": stow_retrieve_urls(base, &stored)}
        }),
    )
}

/// STOW total failure / malformed upload: 400 in DICOM JSON (the
/// `:1160` contract covers failure bodies alike), carrying the
/// `FailedSOPSequence` refs when any part yielded UIDs. The per-part
/// reason strings never render: only UIDs plus the fixed
/// `STOW_FAILURE_PROCESSING` code, so the body stays bounded.
fn stow_total_failure(failures: &[(Option<String>, Option<String>, String)]) -> HttpResponse {
    HttpResponse::json(
        400,
        &serde_json::json!({
            "00081198": {"vr": "SQ", "Value": failed_sop_items(failures)},
        }),
    )
}

/// One `FailedSOPSequence` item per failed instance (PS3.18 Store
/// Instances Response): best-effort class/instance refs plus the
/// failure-reason code.
fn failed_sop_items(
    failures: &[(Option<String>, Option<String>, String)],
) -> Vec<serde_json::Value> {
    failures
        .iter()
        .map(|(class, inst, _)| {
            let mut item = serde_json::Map::new();
            if let Some(uid) = class {
                item.insert(
                    "00081150".to_string(),
                    serde_json::json!({"vr": "UI", "Value": [uid]}),
                );
            }
            if let Some(uid) = inst {
                item.insert(
                    "00081155".to_string(),
                    serde_json::json!({"vr": "UI", "Value": [uid]}),
                );
            }
            item.insert(
                "00081197".to_string(),
                serde_json::json!({"vr": "US", "Value": [STOW_FAILURE_PROCESSING]}),
            );
            serde_json::Value::Object(item)
        })
        .collect()
}

/// One RetrieveURL per distinct stored study, first-seen order: a
/// multi-study STOW must reference every study it stored, not just
/// the first part's.
fn stow_retrieve_urls(base: &str, stored: &[StoredIds]) -> Vec<serde_json::Value> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for ids in stored {
        if seen.insert(ids.study.clone()) {
            urls.push(serde_json::json!(format!("{base}/studies/{}", ids.study)));
        }
    }
    urls
}

/// One `ReferencedSOPSequence` item per stored instance (PS3.18
/// Store Instances Response: class + instance refs for what landed).
fn referenced_sop_items(stored: &[StoredIds]) -> Vec<serde_json::Value> {
    stored
        .iter()
        .map(|ids| {
            serde_json::json!({
                "00081150": {"vr": "UI", "Value": [ids.sop_class]},
                "00081155": {"vr": "UI", "Value": [ids.instance]},
            })
        })
        .collect()
}

/// STOW-RS Failure Reason for a part that could not be stored
/// (PS3.18 processing failure).
const STOW_FAILURE_PROCESSING: u16 = 0x0110;

/// Best-effort SOP Class/Instance UIDs for a failed STOW part
/// (header-only: the body may be truncated or garbage).
fn sop_uids_of(body: &[u8]) -> (Option<String>, Option<String>) {
    let Ok((_, ds)) = read_part10_header(body) else {
        return (None, None);
    };
    (
        ds.string(tags::SOP_CLASS_UID).map(str::to_string),
        ds.string(tags::SOP_INSTANCE_UID).map(str::to_string),
    )
}

/// Maximum test-image pixels per side ([`try_test_file`]): `u16`
/// dims multiply to ~8 GiB of pixel bytes, which a test helper must
/// never allocate.
const MAX_TEST_FILE_SIDE: usize = 1024;

/// Maximum test-image pixel bytes ([`try_test_file`]).
const MAX_TEST_FILE_PIXELS: usize = 8 * 1024 * 1024;

/// Build a Part-10 test file, refusing dims that would allocate
/// more than [`MAX_TEST_FILE_PIXELS`] bytes instead of OOMing the
/// test runner.
#[doc(hidden)]
pub fn try_test_file(
    study: &str,
    series: &str,
    instance: &str,
    rows: u16,
    cols: u16,
) -> std::result::Result<Vec<u8>, String> {
    let (rows_n, cols_n) = (rows as usize, cols as usize);
    let len = rows_n
        .checked_mul(cols_n)
        .and_then(|n| n.checked_mul(2))
        .ok_or_else(|| "test_file dims overflow address space".to_string())?;
    if rows_n > MAX_TEST_FILE_SIDE || cols_n > MAX_TEST_FILE_SIDE || len > MAX_TEST_FILE_PIXELS {
        return Err(format!(
            "test_file dims {rows}x{cols} exceed the test cap \
             ({MAX_TEST_FILE_SIDE}x{MAX_TEST_FILE_SIDE}, {MAX_TEST_FILE_PIXELS} pixel bytes)"
        ));
    }
    let mut ds = DataSet::new();
    ds.put_uid(tags::SOP_CLASS_UID, vyna_dcm_core::sop_class::CT_IMAGE);
    ds.put_uid(tags::SOP_INSTANCE_UID, instance);
    ds.put_uid(tags::STUDY_INSTANCE_UID, study);
    ds.put_uid(tags::SERIES_INSTANCE_UID, series);
    ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Web^Test");
    ds.put_text(tags::PATIENT_ID, Vr::Lo, "W123");
    ds.put_text(tags::MODALITY, Vr::Cs, "CT");
    ds.put_u16_single(tags::ROWS, rows);
    ds.put_u16_single(tags::COLUMNS, cols);
    ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 1);
    ds.put_u16_single(tags::BITS_ALLOCATED, 16);
    ds.put_u16_single(tags::BITS_STORED, 12);
    ds.put_u16_single(tags::HIGH_BIT, 11);
    ds.put_u16_single(tags::PIXEL_REPRESENTATION, 0);
    ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, vec![0u8; len]);
    let meta = FileMeta::new(
        vyna_dcm_core::sop_class::CT_IMAGE,
        instance,
        TransferSyntax::ExplicitVrLittleEndian,
    );
    write_part10(&meta, &ds).map_err(|e| e.to_string())
}

/// Build a Part-10 test file (also used by doc examples).
///
/// Panics on oversize dims or encode failure; fallible callers want
/// [`try_test_file`].
#[doc(hidden)]
pub fn test_file(study: &str, series: &str, instance: &str, rows: u16, cols: u16) -> Vec<u8> {
    try_test_file(study, series, instance, rows, cols).expect("test_file dims within test cap")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_codec::dataset_from_json;

    fn stored() -> MemoryStore {
        let store = MemoryStore::new();
        store
            .store_instance(
                Bytes::from(test_file("1.2.3", "1.2.3.1", "1.2.3.1.1", 4, 4)),
                None,
            )
            .unwrap();
        store
            .store_instance(
                Bytes::from(test_file("1.2.3", "1.2.3.2", "1.2.3.2.1", 2, 2)),
                None,
            )
            .unwrap();
        store
    }

    fn get(path: &str) -> HttpRequest {
        let (path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.to_string(), String::new()),
        };
        HttpRequest {
            method: "GET".into(),
            path,
            query,
            headers: HashMap::new(),
            body: Bytes::new(),
        }
    }

    #[test]
    fn qido_paging_windows_are_stable() {
        let store = stored();
        let rows = |path: &str| {
            let r = dispatch(&store, "http://x/wado", &get(path));
            assert_eq!(r.status, 200, "{path}");
            serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()
        };
        let uid = |v: &serde_json::Value, i: usize| {
            v.as_array().unwrap()[i]["0020000E"]["Value"][0]
                .as_str()
                .unwrap()
                .to_string()
        };
        // Two series; page through them one at a time.
        let v = rows("/studies/1.2.3/series?limit=1&offset=0");
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(uid(&v, 0), "1.2.3.1");
        let v = rows("/studies/1.2.3/series?limit=1&offset=1");
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(uid(&v, 0), "1.2.3.2");
        // Past the end (and absurd offsets) return nothing.
        assert!(
            rows("/studies/1.2.3/series?offset=99")
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Absurd but 32-bit-parseable (larger values fall back to 0).
        assert!(
            rows("/studies/1.2.3/series?offset=4000000000")
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(rows("/studies?offset=1").as_array().unwrap().is_empty());
    }

    #[test]
    fn qido_rejects_overlong_query() {
        let store = stored();
        let big = format!("PatientID={}", "P".repeat(MAX_QIDO_QUERY_BYTES + 1));
        let r = dispatch(&store, "http://x/wado", &get(&format!("/studies?{big}")));
        assert_eq!(r.status, 400);
        let ok = format!("PatientID={}", "P".repeat(100));
        let r = dispatch(&store, "http://x/wado", &get(&format!("/studies?{ok}")));
        assert_eq!(r.status, 200);
    }

    #[test]
    fn debug_redacts_auth_and_body() {
        let mut req = get("/studies?PatientID=1");
        req.headers
            .insert("authorization".into(), "Bearer s3cret".into());
        req.headers
            .insert("accept".into(), "application/json".into());
        req.headers.insert("cookie".into(), "session=abc123".into());
        req.headers
            .insert("set-cookie".into(), "session=def456".into());
        req.body = Bytes::from_static(b"PHI-BYTES");
        let shown = format!("{req:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(!shown.contains("abc123"), "{shown}");
        assert!(!shown.contains("def456"), "{shown}");
        assert!(!shown.contains("PHI-BYTES"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(shown.contains("body_len"), "{shown}");
        // Non-secret fields still show.
        assert!(shown.contains("/studies"), "{shown}");
        assert!(shown.contains("application/json"), "{shown}");
    }

    #[test]
    fn qido_search_and_filters() {
        let store = stored();
        // All studies.
        let r = dispatch(&store, "http://x/wado", &get("/studies"));
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1); // de-duplicated by study
        // Instances of series 1.
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1/instances"),
        );
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        // PatientID filter hit + miss.
        let r = dispatch(&store, "http://x/wado", &get("/studies?PatientID=W123"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&r.body)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let r = dispatch(&store, "http://x/wado", &get("/studies?PatientID=NOBODY"));
        assert!(
            serde_json::from_slice::<serde_json::Value>(&r.body)
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Prefix wildcard + range + limit.
        let r = dispatch(&store, "http://x/wado", &get("/studies?PatientID=W*"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&r.body)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let r = dispatch(&store, "http://x/wado", &get("/studies?limit=0"));
        assert!(
            serde_json::from_slice::<serde_json::Value>(&r.body)
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        // QIDO responses carry no pixel data.
        let r = dispatch(&store, "http://x/wado", &get("/studies"));
        assert!(!String::from_utf8_lossy(&r.body).contains("7FE00010"));
    }

    #[test]
    fn qido_offset_applies_after_filters() {
        let store = stored();
        let count = |q: &str| {
            let r = dispatch(&store, "http://x/wado", &get(q));
            assert_eq!(r.status, 200);
            serde_json::from_slice::<serde_json::Value>(&r.body)
                .unwrap()
                .as_array()
                .unwrap()
                .len()
        };
        // The filter matches only the second stored instance: offset
        // pages the single match, not the raw store order (offset=1
        // must yield nothing, not the second instance).
        assert_eq!(count("/instances?SeriesInstanceUID=1.2.3.2"), 1);
        assert_eq!(count("/instances?SeriesInstanceUID=1.2.3.2&offset=1"), 0);
        assert_eq!(count("/instances?offset=1&limit=10"), 1);
        assert_eq!(count("/instances?offset=9&limit=10"), 0);
    }

    #[test]
    fn match_attribute_ranges_and_pn_prefix() {
        use vyna_dcm_core::PersonName;
        let mut ds = DataSet::new();
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20240115");
        ds.put_person_name(tags::PATIENT_NAME, vec![PersonName::parse("Doe^John")]);
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "W123");
        // Closed, open-ended, and bare-dash ranges.
        assert!(match_attribute(&ds, tags::STUDY_DATE, "20240101-20240201"));
        assert!(match_attribute(&ds, tags::STUDY_DATE, "20240101-"));
        assert!(match_attribute(&ds, tags::STUDY_DATE, "-20240201"));
        assert!(match_attribute(&ds, tags::STUDY_DATE, "-"));
        assert!(!match_attribute(&ds, tags::STUDY_DATE, "20240201-"));
        assert!(!match_attribute(&ds, tags::STUDY_DATE, "-20240101"));
        assert!(!match_attribute(&ds, tags::STUDY_DATE, "20240201-20240301"));
        // PN prefix is case-insensitive like PN exact match; other VRs
        // keep case-sensitive prefixes.
        assert!(match_attribute(&ds, tags::PATIENT_NAME, "doe^john"));
        assert!(match_attribute(&ds, tags::PATIENT_NAME, "DOE^*"));
        assert!(match_attribute(&ds, tags::PATIENT_NAME, "doe^j*"));
        assert!(!match_attribute(&ds, tags::PATIENT_NAME, "SMITH*"));
        assert!(match_attribute(&ds, tags::PATIENT_ID, "W*"));
        assert!(!match_attribute(&ds, tags::PATIENT_ID, "w*"));
    }

    #[test]
    fn wado_instance_metadata_and_bulk() {
        let store = stored();
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1"),
        );
        assert_eq!(r.status, 200);
        assert!(
            r.headers
                .iter()
                .any(|(_, v)| v.contains("multipart/related"))
        );

        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/metadata"),
        );
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let uri = v[0]["7FE00010"]["BulkDataURI"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(uri.starts_with("http://x/wado/bulk/"), "{uri}");
        // The BulkDataURI resolves.
        let path = uri.replace("http://x/wado", "");
        let r = dispatch(&store, "http://x/wado", &get(&path));
        assert_eq!(r.status, 200);
        assert_eq!(r.body.len(), 4 * 4 * 2);

        // Unknown instance 404s.
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/9.9/series/9.9/instances/9.9"),
        );
        assert_eq!(r.status, 404);
    }

    /// Store double that shortens bulk ids (contract violation).
    struct ShortBulkStore {
        inner: MemoryStore,
    }

    impl DicomWebStore for ShortBulkStore {
        fn search(
            &self,
            scope: &QidoScope,
            filters: &[(Tag, String)],
            limit: usize,
            offset: usize,
        ) -> Vec<DataSet> {
            self.inner.search(scope, filters, limit, offset)
        }

        fn instance_file(&self, study: &str, series: &str, instance: &str) -> Option<Bytes> {
            self.inner.instance_file(study, series, instance)
        }

        fn study_files(&self, study: &str) -> Vec<Bytes> {
            self.inner.study_files(study)
        }

        fn series_files(&self, study: &str, series: &str) -> Vec<Bytes> {
            self.inner.series_files(study, series)
        }

        fn instance_metadata(
            &self,
            study: &str,
            series: &str,
            instance: &str,
        ) -> Option<(DataSet, Vec<BulkBlob>)> {
            self.inner.instance_metadata(study, series, instance)
        }

        fn instance_frames(
            &self,
            study: &str,
            series: &str,
            instance: &str,
            frames: &[u32],
        ) -> std::result::Result<Option<Vec<Vec<u8>>>, String> {
            self.inner.instance_frames(study, series, instance, frames)
        }

        fn store_instance(
            &self,
            file: Bytes,
            study_hint: Option<&str>,
        ) -> std::result::Result<StoredIds, String> {
            self.inner.store_instance(file, study_hint)
        }

        fn put_bulk(&self, blobs: Vec<(Bytes, String)>) -> Vec<String> {
            let mut ids = self.inner.put_bulk(blobs);
            ids.pop();
            ids
        }

        fn bulk_data(&self, id: &str) -> Option<(Bytes, String)> {
            self.inner.bulk_data(id)
        }
    }

    #[test]
    fn oversize_single_instance_rejected_not_stored_then_evicted() {
        let store = MemoryStore::new();
        let big = Bytes::from(vec![0u8; MAX_INSTANCE_BYTES + 1]);
        let err = store.store_instance(big, None).unwrap_err();
        assert!(err.contains("exceeds the store cap"), "{err}");
        assert!(store.is_empty());
    }

    #[test]
    fn metadata_short_bulk_ids_is_500_not_silent_drop() {
        let store = ShortBulkStore { inner: stored() };
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/metadata"),
        );
        assert_eq!(r.status, 500);
    }

    #[test]
    fn bulk_ids_are_unpredictable_and_evict_oldest() {
        let store = MemoryStore::new();
        let ids = store.put_bulk(vec![
            (
                Bytes::from(vec![1u8; 8]),
                "application/octet-stream".to_string(),
            ),
            (
                Bytes::from(vec![2u8; 8]),
                "application/octet-stream".to_string(),
            ),
        ]);
        // UUID-simple ids: 32 lowercase hex chars, unique.
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        for id in &ids {
            assert_eq!(id.len(), 32, "{id}");
            assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
            assert!(store.bulk_data(id).is_some());
        }
        // Past the blob cap the oldest entries are evicted.
        let mut first_extra = String::new();
        for i in 0..(MAX_BULK_BLOBS + 10) {
            let id = store.put_bulk(vec![(
                Bytes::from(vec![0u8; 8]),
                "application/octet-stream".to_string(),
            )]);
            if i == 0 {
                first_extra = id[0].clone();
            }
        }
        assert!(store.bulk_data(&ids[0]).is_none(), "oldest must evict");
        assert!(store.bulk_data(&first_extra).is_none());
        assert_eq!(store.lock().bulk.len(), MAX_BULK_BLOBS);
    }

    #[test]
    fn instances_evict_oldest_past_cap() {
        let store = MemoryStore::new();
        for i in 0..(MAX_INSTANCES + 5) {
            let uid = format!("1.2.3.{i}");
            store
                .store_instance(Bytes::from(test_file("1.2.3", "1.2.3.1", &uid, 2, 2)), None)
                .unwrap();
        }
        assert_eq!(store.len(), MAX_INSTANCES);
        assert!(store.instance_file("1.2.3", "1.2.3.1", "1.2.3.0").is_none());
        assert!(store.instance_file("1.2.3", "1.2.3.1", "1.2.3.1").is_none());
        assert!(
            store
                .instance_file("1.2.3", "1.2.3.1", &format!("1.2.3.{}", MAX_INSTANCES + 4))
                .is_some()
        );
    }

    #[test]
    fn qido_ranges_are_vr_gated_and_numeric() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "ABC-123");
        ds.put_text(tags::STUDY_DATE, Vr::Da, "20240115");
        ds.put_text(tags::SERIES_NUMBER, Vr::Is, "9");
        // Hyphen in a non-range VR is literal, not a range.
        assert!(match_attribute(&ds, tags::PATIENT_ID, "ABC-123"));
        assert!(!match_attribute(&ds, tags::PATIENT_ID, "ABC-124"));
        // IS ranges compare numerically: 9 is inside 1-10.
        assert!(match_attribute(&ds, tags::SERIES_NUMBER, "1-10"));
        assert!(match_attribute(&ds, tags::SERIES_NUMBER, "-5-10"));
        assert!(!match_attribute(&ds, tags::SERIES_NUMBER, "10-20"));
        // DA ranges stay lexicographic (fixed-width).
        assert!(match_attribute(&ds, tags::STUDY_DATE, "20240101-20240131"));
        assert!(!match_attribute(&ds, tags::STUDY_DATE, "20240201-20240228"));
    }

    #[test]
    fn qido_wildcards_match_anywhere() {
        let store = stored();
        let count = |q: &str| {
            let r = dispatch(&store, "http://x/wado", &get(q));
            assert_eq!(r.status, 200);
            serde_json::from_slice::<serde_json::Value>(&r.body)
                .unwrap()
                .as_array()
                .unwrap()
                .len()
        };
        // PatientID is W123 in both instances (one study).
        assert_eq!(count("/studies?PatientID=*123"), 1); // leading *
        assert_eq!(count("/studies?PatientID=W*3"), 1); // infix *
        assert_eq!(count("/studies?PatientID=W12?"), 1); // ? wildcard
        assert_eq!(count("/studies?PatientID=*2*"), 1);
        assert_eq!(count("/studies?PatientID=W12??"), 0); // too long
        assert_eq!(count("/studies?PatientID=*999*"), 0);
        // PN stays case-insensitive under wildcards.
        assert_eq!(count("/studies?PatientName=web^*"), 1);
    }

    #[test]
    fn qido_matches_wide_numerics_and_tags() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "W1");
        ds.insert(vyna_dcm_core::DataElement::new(
            Tag::new(0x0020, 0x4000),
            Vr::Lt,
            Value::U64(vec![123456789012345]),
        ));
        ds.insert(vyna_dcm_core::DataElement::new(
            tags::SELECTOR_AT_VALUE,
            Vr::At,
            Value::Tags(vec![Tag::new(0x0010, 0x0010)]),
        ));
        assert!(match_attribute(
            &ds,
            Tag::new(0x0020, 0x4000),
            "123456789012345"
        ));
        assert!(!match_attribute(&ds, Tag::new(0x0020, 0x4000), "123"));
        assert!(match_attribute(&ds, tags::SELECTOR_AT_VALUE, "00100010"));
        assert!(!match_attribute(&ds, tags::SELECTOR_AT_VALUE, "00100020"));
    }

    #[test]
    fn qido_ignores_unknown_spelled_keys() {
        assert_eq!(qido_key_to_tag("00100010"), Some(Tag::new(0x0010, 0x0010)));
        assert_eq!(
            qido_key_to_tag("(0010,0010)"),
            Some(Tag::new(0x0010, 0x0010))
        );
        // Unknown keys are ignored, never fabricated into tags.
        assert_eq!(qido_key_to_tag("deadbeef"), None);
        assert_eq!(qido_key_to_tag("StudyDate2024"), None);
        assert_eq!(qido_key_to_tag("not-a-key!"), None);
    }

    #[test]
    fn qido_rejects_param_flood() {
        let store = stored();
        let query = (0..=MAX_QIDO_PARAMS)
            .map(|i| format!("UnknownKey{i}=x"))
            .collect::<Vec<_>>()
            .join("&");
        let r = dispatch(&store, "http://x/wado", &get(&format!("/studies?{query}")));
        assert_eq!(r.status, 400);
    }

    #[test]
    fn frames_and_stow() {
        let store = stored();
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/frames/1"),
        );
        assert_eq!(r.status, 200);
        // Bad frame addresses are 400 (not silently dropped); frame 0
        // is invalid since frames are 1-based.
        for bad in ["0", "1,banana", "x", "1,,2", "4294967297"] {
            let r = dispatch(
                &store,
                "http://x/wado",
                &get(&format!(
                    "/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/frames/{bad}"
                )),
            );
            assert_eq!(r.status, 400, "{bad}");
        }
        // A well-formed but absent frame stays 404.
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/frames/2"),
        );
        assert_eq!(r.status, 404);
        // Duplicates are 400 (each entry embeds a full frame).
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/frames/1,1"),
        );
        assert_eq!(r.status, 400);
        // Past the frame-count cap is 400 even for well-formed entries.
        let flood = (1..=MAX_WADO_FRAMES + 1)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let r = dispatch(
            &store,
            "http://x/wado",
            &get(&format!(
                "/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/frames/{flood}"
            )),
        );
        assert_eq!(r.status, 400);

        let file = test_file("9.9.9", "9.9.9.1", "9.9.9.1.1", 2, 2);
        let (ct, body) = encode_multipart(
            &[Part::new("application/dicom", file)],
            "b1",
            "application/dicom",
        )
        .unwrap();
        let req = HttpRequest {
            method: "POST".into(),
            path: "/studies".into(),
            query: String::new(),
            headers: [("content-type".into(), ct)].into(),
            body: Bytes::from(body),
        };
        let r = dispatch(&store, "http://x/wado", &req);
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        assert!(String::from_utf8_lossy(&r.body).contains("00081190"));
        // Success carries the stored-instance refs too (PS3.18).
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let refs = v["00081199"]["Value"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0]["00081150"]["Value"][0],
            serde_json::json!(vyna_dcm_core::sop_class::CT_IMAGE)
        );
        assert_eq!(
            refs[0]["00081155"]["Value"][0],
            serde_json::json!("9.9.9.1.1")
        );
        assert_eq!(store.len(), 3);

        // Study mismatch is rejected.
        let file = test_file("8.8.8", "8.8.8.1", "8.8.8.1.1", 2, 2);
        let (ct, body) = encode_multipart(
            &[Part::new("application/dicom", file)],
            "b2",
            "application/dicom",
        )
        .unwrap();
        let req = HttpRequest {
            method: "POST".into(),
            path: "/studies/7.7.7".into(),
            query: String::new(),
            headers: [("content-type".into(), ct)].into(),
            body: Bytes::from(body),
        };
        assert_eq!(dispatch(&store, "http://x/wado", &req).status, 400);

        // Parameterized parts (transfer-syntax suffix) are stored, not
        // dropped into "nothing stored".
        let file = test_file("6.6.6", "6.6.6.1", "6.6.6.1.1", 2, 2);
        let (ct, body) = encode_multipart(
            &[Part::new(
                "application/dicom; transfer-syntax=\"1.2.840.10008.1.2.1\"",
                file,
            )],
            "b3",
            "application/dicom",
        )
        .unwrap();
        let req = HttpRequest {
            method: "POST".into(),
            path: "/studies".into(),
            query: String::new(),
            headers: [("content-type".into(), ct)].into(),
            body: Bytes::from(body),
        };
        let r = dispatch(&store, "http://x/wado", &req);
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    }

    #[test]
    fn retrieve_study_and_series_serve_every_instance() {
        let store = stored();
        // Study 1.2.3 holds two instances (different series).
        let r = dispatch(&store, "http://x/wado", &get("/studies/1.2.3"));
        assert_eq!(r.status, 200);
        let ct = r
            .headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .unwrap()
            .1
            .clone();
        let (mime, params) = parse_content_type(&ct);
        assert_eq!(mime, "multipart/related");
        let parts = decode_multipart(
            &r.body,
            params.get("boundary").map(String::as_str).unwrap_or(""),
        )
        .unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.content_type == "application/dicom"));
        // One series narrows to its single instance.
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/1.2.3/series/1.2.3.1"),
        );
        assert_eq!(r.status, 200);
        let ct = r
            .headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .unwrap()
            .1
            .clone();
        let (_, params) = parse_content_type(&ct);
        let parts = decode_multipart(
            &r.body,
            params.get("boundary").map(String::as_str).unwrap_or(""),
        )
        .unwrap();
        assert_eq!(parts.len(), 1);
        // Unknown study/series 404; wrong media 406s.
        assert_eq!(
            dispatch(&store, "http://x/wado", &get("/studies/9.9")).status,
            404
        );
        assert_eq!(
            dispatch(&store, "http://x/wado", &get("/studies/1.2.3/series/9.9")).status,
            404
        );
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept("/studies/1.2.3", "image/jpeg")
            )
            .status,
            406
        );
    }

    #[test]
    fn unknown_routes_404() {
        let store = stored();
        assert_eq!(dispatch(&store, "http://x", &get("/nope")).status, 404);
        let mut bad = get("/studies");
        bad.method = "DELETE".into();
        let r = dispatch(&store, "http://x", &bad);
        assert_eq!(r.status, 405);
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| k == "allow" && v == "GET, POST"),
            "{:?}",
            r.headers
        );
    }

    #[test]
    fn query_parsing() {
        assert_eq!(
            parse_query("a=1&b=&c"),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "".into()),
                ("c".into(), "".into())
            ]
        );
        assert_eq!(
            parse_query("n=Doe%5EJohn&x=a+b"),
            vec![("n".into(), "Doe^John".into()), ("x".into(), "a b".into())]
        );
        assert_eq!(qido_key_to_tag("PatientID"), Some(tags::PATIENT_ID));
        assert_eq!(qido_key_to_tag("00100020"), Some(tags::PATIENT_ID));
        assert_eq!(qido_key_to_tag("patientid"), Some(tags::PATIENT_ID));
        assert_eq!(qido_key_to_tag("Nope"), None);
    }

    #[test]
    fn json_helper_round_trip() {
        // Exercise dataset_from_json import path via metadata JSON.
        let store = stored();
        let r = dispatch(&store, "http://x/wado", &get("/studies?PatientID=W123"));
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let ds = dataset_from_json(&v[0]).unwrap();
        assert_eq!(ds.string(tags::PATIENT_ID), Some("W123"));
    }

    #[test]
    fn qido_dedup_claims_only_matched_rows() {
        // Regression: dedup used to insert before the filter check, so
        // a study whose FIRST instance missed the filter hid a LATER
        // matching instance (0 hits instead of 1).
        let store = stored();
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies?SOPInstanceUID=1.2.3.2.1"),
        );
        assert_eq!(r.status, 200);
        let hits: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1);
        assert_eq!(hits[0]["0020000D"]["Value"][0], serde_json::json!("1.2.3"));
        // Same shape at SERIES scope (global /series endpoint).
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/series?SOPInstanceUID=1.2.3.2.1"),
        );
        let hits: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1);
        assert_eq!(
            hits[0]["0020000E"]["Value"][0],
            serde_json::json!("1.2.3.2")
        );
    }

    #[test]
    fn frames_garbage_count_is_404() {
        let store = MemoryStore::new();
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, vyna_dcm_core::sop_class::CT_IMAGE);
        ds.put_uid(tags::SOP_INSTANCE_UID, "9.9.9.1.1");
        ds.put_uid(tags::STUDY_INSTANCE_UID, "9.9.9");
        ds.put_uid(tags::SERIES_INSTANCE_UID, "9.9.9.1");
        ds.put_u16_single(tags::ROWS, 2);
        ds.put_u16_single(tags::COLUMNS, 2);
        ds.put_u16_single(tags::SAMPLES_PER_PIXEL, 1);
        ds.put_u16_single(tags::BITS_ALLOCATED, 16);
        ds.put_u16_single(tags::BITS_STORED, 12);
        ds.put_u16_single(tags::HIGH_BIT, 11);
        ds.put_u16_single(tags::PIXEL_REPRESENTATION, 0);
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, vec![0u8; 8]);
        ds.put_text(tags::NUMBER_OF_FRAMES, Vr::Is, "banana");
        let meta = FileMeta::new(
            vyna_dcm_core::sop_class::CT_IMAGE,
            "9.9.9.1.1",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        store
            .store_instance(Bytes::from(write_part10(&meta, &ds).unwrap()), None)
            .unwrap();
        // Present-but-garbage count: the instance exists but is
        // undecodable, so 500 — not 404, not frame-1-of-everything.
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/9.9.9/series/9.9.9.1/instances/9.9.9.1.1/frames/1"),
        );
        assert_eq!(r.status, 500);
    }

    #[test]
    fn frames_for_pixel_less_instance_is_500_not_404() {
        let store = MemoryStore::new();
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, vyna_dcm_core::sop_class::CT_IMAGE);
        ds.put_uid(tags::SOP_INSTANCE_UID, "9.9.8.1.1");
        ds.put_uid(tags::STUDY_INSTANCE_UID, "9.9.8");
        ds.put_uid(tags::SERIES_INSTANCE_UID, "9.9.8.1");
        let meta = FileMeta::new(
            vyna_dcm_core::sop_class::CT_IMAGE,
            "9.9.8.1.1",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        store
            .store_instance(Bytes::from(write_part10(&meta, &ds).unwrap()), None)
            .unwrap();
        // Stored fine (no pixels required), but frames are undecodable.
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/9.9.8/series/9.9.8.1/instances/9.9.8.1.1/frames/1"),
        );
        assert_eq!(r.status, 500);
    }

    #[test]
    fn encap_bulk_honors_mime_type() {
        let mut ds = DataSet::new();
        ds.put_bytes(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, b"<doc/>".to_vec());
        ds.put_text(tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT, Vr::Lo, "text/xml");
        let blobs = extract_bulk(&mut ds);
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].content_type, "text/xml");
        // Absent or malformed MIME falls back to legacy PDF.
        for mime in [None, Some(""), Some("not a mime"), Some("text/html; x=1")] {
            let mut ds = DataSet::new();
            ds.put_bytes(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, b"%PDF".to_vec());
            if let Some(m) = mime {
                ds.put_text(tags::MIME_TYPE_OF_ENCAPSULATED_DOCUMENT, Vr::Lo, m);
            }
            let blobs = extract_bulk(&mut ds);
            assert_eq!(blobs[0].content_type, "application/pdf", "{mime:?}");
        }
    }

    fn get_accept(path: &str, accept: &str) -> HttpRequest {
        let mut r = get(path);
        r.headers.insert("accept".to_string(), accept.to_string());
        r
    }

    #[test]
    fn wado_accept_negotiation() {
        let store = stored();
        let inst = "/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1";
        // Instance: dicom or multipart only.
        assert_eq!(
            dispatch(&store, "http://x/wado", &get_accept(inst, "image/jpeg")).status,
            406
        );
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept(inst, "application/dicom")
            )
            .status,
            200
        );
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept(inst, "multipart/related")
            )
            .status,
            200
        );
        // Frames: octet-stream (wildcards pass).
        let frames = format!("{inst}/frames/1");
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept(&frames, "application/json")
            )
            .status,
            406
        );
        assert_eq!(
            dispatch(&store, "http://x/wado", &get_accept(&frames, "*/*")).status,
            200
        );
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept(&frames, "text/html, application/*")
            )
            .status,
            200
        );
        // Metadata: dicom+json (params ignored, mismatch rejected).
        let meta = format!("{inst}/metadata");
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept(&meta, "application/dicom+json; charset=utf-8")
            )
            .status,
            200
        );
        assert_eq!(
            dispatch(&store, "http://x/wado", &get_accept(&meta, "text/html")).status,
            406
        );
    }

    #[test]
    fn dispatch_strips_public_base_prefix() {
        let store = stored();
        // Prefixed deployment: BulkDataURI-style paths route.
        let r = dispatch(
            &store,
            "http://x/dicomweb",
            &get("/dicomweb/studies?PatientID=W123"),
        );
        assert_eq!(r.status, 200);
        let hits: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1);
        // Segment-boundary only: /dicomweb2/* must not collapse.
        let r = dispatch(
            &store,
            "http://x/dicomweb",
            &get("/dicomweb2/studies?PatientID=W123"),
        );
        assert_eq!(r.status, 404);
        // Bare-root base keeps root routing.
        let r = dispatch(&store, "http://x", &get("/studies?PatientID=W123"));
        assert_eq!(r.status, 200);
    }

    fn stow_req(path: &str, content_type: &str, body: Vec<u8>) -> HttpRequest {
        HttpRequest {
            method: "POST".into(),
            path: path.into(),
            query: String::new(),
            headers: [("content-type".into(), content_type.into())].into(),
            body: Bytes::from(body),
        }
    }

    #[test]
    fn qido_and_stow_negotiate_accept() {
        let store = stored();
        // QIDO speaks dicom+json only.
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept("/studies", "application/dicom+xml")
            )
            .status,
            406
        );
        assert_eq!(
            dispatch(
                &store,
                "http://x/wado",
                &get_accept("/studies", "application/dicom+json")
            )
            .status,
            200
        );
        let (ct, body) = encode_multipart(
            &[Part::new(
                "application/dicom",
                test_file("7.7.7", "7.7.7.1", "7.7.7.1.1", 2, 2),
            )],
            "b406",
            "application/dicom",
        )
        .unwrap();
        // STOW answers dicom+json only, even on success.
        let mut xml_only = stow_req("/studies", &ct, body.clone());
        xml_only
            .headers
            .insert("accept".into(), "application/dicom+xml".into());
        assert_eq!(dispatch(&store, "http://x/wado", &xml_only).status, 406);
        let mut json = stow_req("/studies", &ct, body);
        json.headers
            .insert("accept".into(), "application/dicom+json".into());
        assert_eq!(dispatch(&store, "http://x/wado", &json).status, 200);
    }

    #[test]
    fn stow_without_dicom_parts_is_json_failure() {
        let store = stored();
        let (ct, body) = encode_multipart(
            &[Part::new("application/octet-stream", b"hello".to_vec())],
            "bnone",
            "application/octet-stream",
        )
        .unwrap();
        let r = dispatch(&store, "http://x/wado", &stow_req("/studies", &ct, body));
        assert_eq!(r.status, 400);
        // DICOM JSON per the STOW contract, not free text: an empty
        // FailedSOPSequence (no DICOM part yielded any refs).
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| k == "content-type" && v == "application/dicom+json"),
            "{:?}",
            r.headers
        );
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["00081198"]["vr"], serde_json::json!("SQ"));
        assert!(v["00081198"]["Value"].as_array().unwrap().is_empty());
    }

    #[test]
    fn stow_rejects_malformed_uploads() {
        let store = stored();
        // Not multipart/related.
        let r = dispatch(
            &store,
            "http://x/wado",
            &stow_req("/studies", "application/json", b"{}".to_vec()),
        );
        assert_eq!(r.status, 400);
        // Missing boundary.
        let r = dispatch(
            &store,
            "http://x/wado",
            &stow_req("/studies", "multipart/related", b"x".to_vec()),
        );
        assert_eq!(r.status, 400);
        // Empty boundary.
        let r = dispatch(
            &store,
            "http://x/wado",
            &stow_req("/studies", "multipart/related; boundary=", b"x".to_vec()),
        );
        assert_eq!(r.status, 400);
    }

    #[test]
    fn stow_partial_failure_lists_failed_items() {
        let store = stored();
        let good = test_file("5.5.5", "5.5.5.1", "5.5.5.1.1", 2, 2);
        // Unparseable part: no UIDs to report.
        let garbage = b"not a dicom file at all".to_vec();
        // Parses but misses Study UID: class UID reported, instance missing.
        let mut ds = DataSet::new();
        ds.put_uid(tags::SOP_CLASS_UID, vyna_dcm_core::sop_class::CT_IMAGE);
        ds.put_uid(tags::SOP_INSTANCE_UID, "5.5.5.1.2");
        ds.put_uid(tags::SERIES_INSTANCE_UID, "5.5.5.1");
        let meta = FileMeta::new(
            vyna_dcm_core::sop_class::CT_IMAGE,
            "5.5.5.1.2",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        let missing_uid = write_part10(&meta, &ds).unwrap();
        let (ct, body) = encode_multipart(
            &[
                Part::new("application/dicom", good),
                Part::new("application/dicom", garbage),
                Part::new("application/dicom", missing_uid),
            ],
            "b409",
            "application/dicom",
        )
        .unwrap();
        let r = dispatch(&store, "http://x/wado", &stow_req("/studies", &ct, body));
        assert_eq!(r.status, 409, "{}", String::from_utf8_lossy(&r.body));
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert!(v.get("error").is_none(), "non-standard error key present");
        let failed = v["00081198"]["Value"].as_array().unwrap();
        assert_eq!(failed.len(), 2);
        // Garbage part: reason only.
        assert!(failed[0].get("00081150").is_none());
        assert!(failed[0].get("00081155").is_none());
        assert_eq!(failed[0]["00081197"]["Value"][0], serde_json::json!(0x0110));
        // Missing-UID part: class UID recovered from the header.
        assert_eq!(
            failed[1]["00081150"]["Value"][0],
            serde_json::json!(vyna_dcm_core::sop_class::CT_IMAGE)
        );
        assert_eq!(
            failed[1]["00081155"]["Value"][0],
            serde_json::json!("5.5.5.1.2")
        );
        // The good instance still stored — and referenced: 409
        // carries ReferencedSOPSequence for what landed, alongside
        // FailedSOPSequence for what did not.
        assert_eq!(store.len(), 3);
        let refs = v["00081199"]["Value"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0]["00081155"]["Value"][0],
            serde_json::json!("5.5.5.1.1")
        );
    }

    #[test]
    fn metadata_and_bulk_404s() {
        let store = stored();
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/9.9.9/series/9.9.9.1/instances/9.9.9.1.9/metadata"),
        );
        assert_eq!(r.status, 404);
        let r = dispatch(&store, "http://x/wado", &get("/bulk/does-not-exist"));
        assert_eq!(r.status, 404);
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies/9.9.9/series/9.9.9.1/instances/9.9.9.1.9"),
        );
        assert_eq!(r.status, 404);
    }

    #[test]
    fn qido_limit_offset_clamp_and_garbage() {
        let store = MemoryStore::new();
        for i in 0..5 {
            store
                .store_instance(
                    Bytes::from(test_file(
                        &format!("7.7.{i}"),
                        &format!("7.7.{i}.1"),
                        &format!("7.7.{i}.1.1"),
                        2,
                        2,
                    )),
                    None,
                )
                .unwrap();
        }
        // Limit honored.
        let r = dispatch(&store, "http://x/wado", &get("/studies?limit=2"));
        let hits: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 2);
        // Garbage limit/offset fall back to defaults (100/0): all rows.
        let r = dispatch(
            &store,
            "http://x/wado",
            &get("/studies?limit=banana&offset=nope"),
        );
        let hits: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 5);
        // Offset skips matched rows.
        let r = dispatch(&store, "http://x/wado", &get("/studies?limit=2&offset=4"));
        let hits: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1);
        // Clamp: past 1000 the server caps (1005 rows, limit=99999).
        for i in 5..1005 {
            store
                .store_instance(
                    Bytes::from(test_file(
                        &format!("7.7.{i}"),
                        &format!("7.7.{i}.1"),
                        &format!("7.7.{i}.1.1"),
                        2,
                        2,
                    )),
                    None,
                )
                .unwrap();
        }
        let r = dispatch(&store, "http://x/wado", &get("/studies?limit=99999"));
        let hits: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1000);
    }

    #[test]
    fn wildcard_match_caps_oversized_inputs() {
        // Oversized value: non-match, never a multi-GB char vector.
        let huge = "A".repeat(MAX_MATCH_VALUE_BYTES + 1);
        assert!(!wildcard_match(&huge, "*", false));
        assert!(!wildcard_match(&huge, &huge, false));
        // Boundary: exactly at the cap still compares.
        let edge = "A".repeat(MAX_MATCH_VALUE_BYTES);
        assert!(wildcard_match(&edge, "*", false));
        // Oversized pattern: non-match.
        let big_pat = format!("*{}*", "B".repeat(MAX_MATCH_PATTERN_BYTES));
        assert!(!wildcard_match("B", &big_pat, false));
        // Star-heavy pattern collapses: 10k stars cost one state.
        let stars = "*".repeat(10_000);
        assert!(wildcard_match("anything", &stars, false));
        assert!(wildcard_match("", &stars, false));
        // Multibyte stays on char boundaries: `?` is one char, and
        // a value split mid-char never matches mid-char.
        assert!(wildcard_match("héllo", "h?llo", false));
        assert!(!wildcard_match("héllo", "h??o", false));
        assert!(wildcard_match("日本語", "日本?", false));
    }

    #[test]
    fn wildcard_match_star_heavy_cpu_is_bounded() {
        // Alternating star/char runs defeat star collapsing and send
        // naive backtracking quadratic: 2000 segments against a 64 KiB
        // non-matching value must still return promptly (step budget
        // bails to non-match) instead of hanging the QIDO handler.
        let value = "a".repeat(64 * 1024);
        let mut pattern = "*a".repeat(2000);
        pattern.push('!');
        let t0 = std::time::Instant::now();
        assert!(!wildcard_match(&value, &pattern, false));
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "star-heavy match took {:?}",
            t0.elapsed()
        );
        // Legit segmented patterns still match under the budget.
        assert!(wildcard_match("xxaxxaxxa", "*a*a*a", false));
        assert!(!wildcard_match("xxaxxaxxa", "*a*a*a!", false));
    }

    #[test]
    fn match_attribute_skips_huge_values_borrowed() {
        let mut ds = DataSet::new();
        // One oversized component is a non-match for itself, while a
        // later normal component still matches (lazy per-value scan,
        // no whole-element clone).
        ds.insert(vyna_dcm_core::DataElement::new(
            tags::PATIENT_ID,
            Vr::Lo,
            Value::Text(vec!["X".repeat(MAX_MATCH_VALUE_BYTES + 1), "W123".into()]),
        ));
        assert!(match_attribute(&ds, tags::PATIENT_ID, "W123"));
        assert!(match_attribute(&ds, tags::PATIENT_ID, "W*"));
        assert!(!match_attribute(&ds, tags::PATIENT_ID, "X*"));
        // Huge-VM numerics scan under the total budget: the early
        // values still match without formatting millions first.
        let mut many = vec![7u16; 200_000];
        many[0] = 42;
        ds.insert(vyna_dcm_core::DataElement::new(
            tags::SELECTOR_US_VALUE,
            Vr::Us,
            Value::U16(many),
        ));
        assert!(match_attribute(&ds, tags::SELECTOR_US_VALUE, "42"));
    }

    #[test]
    fn ds_is_unparsable_bound_falls_back_to_literal() {
        let mut ds = DataSet::new();
        ds.put_text(tags::SERIES_NUMBER, Vr::Is, "1e-3");
        // Looks like a range ("1e"-"3") but the bound is unparsable:
        // the exact query must still match.
        assert!(match_attribute(&ds, tags::SERIES_NUMBER, "1e-3"));
        assert!(!match_attribute(&ds, tags::SERIES_NUMBER, "1e-4"));
        let mut ds = DataSet::new();
        ds.put_text(tags::SLICE_THICKNESS, Vr::Ds, "ABC-DEF");
        assert!(match_attribute(&ds, tags::SLICE_THICKNESS, "ABC-DEF"));
        // Real ranges keep range semantics (no literal fallback on a
        // parsed-but-missing range).
        let mut ds = DataSet::new();
        ds.put_text(tags::SERIES_NUMBER, Vr::Is, "7");
        assert!(match_attribute(&ds, tags::SERIES_NUMBER, "1-10"));
        assert!(!match_attribute(&ds, tags::SERIES_NUMBER, "10-20"));
        assert!(match_attribute(&ds, tags::SERIES_NUMBER, "-5-10"));
    }

    #[test]
    fn wado_frame_cap_trips_during_parse() {
        let store = stored();
        let inst = "/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/frames/";
        // Past the cap trips mid-parse with its own message (the old
        // code built the full Vec+HashSet, then 400'd as "bad frame
        // list" — same status, unbounded transient).
        let flood = (1..=MAX_WADO_FRAMES + 1)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let r = dispatch(&store, "http://x/wado", &get(&format!("{inst}{flood}")));
        assert_eq!(r.status, 400);
        assert_eq!(String::from_utf8_lossy(&r.body).as_ref(), "too many frames");
        // Cap trips before the duplicate check: entry #10001 is a dup
        // of frame 1, yet the answer is the cap, not "duplicate".
        let dup_tail = (1..=MAX_WADO_FRAMES)
            .map(|n| n.to_string())
            .chain(std::iter::once("1".to_string()))
            .collect::<Vec<_>>()
            .join(",");
        let r = dispatch(&store, "http://x/wado", &get(&format!("{inst}{dup_tail}")));
        assert_eq!(r.status, 400);
        assert_eq!(String::from_utf8_lossy(&r.body).as_ref(), "too many frames");
        // Duplicates under the cap still report as duplicates.
        let r = dispatch(&store, "http://x/wado", &get(&format!("{inst}1,2,1")));
        assert_eq!(r.status, 400);
        assert_eq!(
            String::from_utf8_lossy(&r.body).as_ref(),
            "duplicate frame number"
        );
    }

    #[test]
    fn dispatch_caps_path_shape() {
        let store = stored();
        // A million-slash path 404s without building the vector.
        let segs = ["seg"; 100].join("/");
        let r = dispatch(&store, "http://x", &get(&format!("/{segs}")));
        assert_eq!(r.status, 404);
        let exact: String = format!("/{}/studies", ["s"; MAX_PATH_SEGMENTS].join("/"));
        let r = dispatch(&store, "http://x", &get(&exact));
        assert_eq!(r.status, 404);
        // Past the byte cap is 414 before any routing work.
        let long = format!("/studies/{}", "9".repeat(MAX_PATH_BYTES));
        let r = dispatch(&store, "http://x", &get(&long));
        assert_eq!(r.status, 414);
        // Real routes (including the 8-segment frames route) pass.
        assert_eq!(dispatch(&store, "http://x", &get("/studies")).status, 200);
        let r = dispatch(
            &store,
            "http://x",
            &get("/studies/1.2.3/series/1.2.3.1/instances/1.2.3.1.1/frames/1"),
        );
        assert_eq!(r.status, 200);
    }

    #[test]
    fn accepts_honors_q_zero() {
        let store = stored();
        // q=0 means "not acceptable" (RFC 7231 §5.3.2), in every
        // spelling — even when the type matches.
        for accept in [
            "application/dicom+json;q=0",
            "application/dicom+json; q=0.0",
            "application/dicom+json;Q=0.000",
            "*/*;q=0",
            "text/html, application/dicom+json;q=0",
        ] {
            let r = dispatch(&store, "http://x/wado", &get_accept("/studies", accept));
            assert_eq!(r.status, 406, "{accept}");
        }
        // Nonzero q (and q-less ranges) still allow.
        for accept in [
            "application/dicom+json;q=0.5",
            "application/dicom+json;q=1",
            "application/dicom+json",
            "text/html, application/dicom+json;q=0.1",
            "application/*",
        ] {
            let r = dispatch(&store, "http://x/wado", &get_accept("/studies", accept));
            assert_eq!(r.status, 200, "{accept}");
        }
    }

    #[test]
    fn multipart_reports_typed_error() {
        let good = HttpResponse::multipart(
            &[Part::new("application/dicom", vec![1])],
            "application/dicom",
        );
        assert!(good.is_ok());
        // CR/LF framing injection surfaces as Error::Multipart (not a
        // stringly Result<Self, String>).
        let evil = HttpResponse::multipart(
            &[Part::new("application/dicom\r\nX-Injected: yes", vec![1])],
            "application/dicom",
        );
        match evil {
            Err(crate::Error::Multipart(_)) => {}
            other => panic!("expected Error::Multipart, got {other:?}"),
        }
    }

    #[test]
    fn stow_total_failure_is_json_with_refs() {
        let store = stored();
        // All-DICOM-but-unparseable: 400 in DICOM JSON carrying the
        // FailedSOPSequence refs (garbage part: reason-only item).
        let (ct, body) = encode_multipart(
            &[Part::new("application/dicom", b"not a dicom file".to_vec())],
            "b400",
            "application/dicom",
        )
        .unwrap();
        let r = dispatch(&store, "http://x/wado", &stow_req("/studies", &ct, body));
        assert_eq!(r.status, 400);
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| k == "content-type" && v == "application/dicom+json"),
            "{:?}",
            r.headers
        );
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let failed = v["00081198"]["Value"].as_array().unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["00081197"]["Value"][0], serde_json::json!(0x0110));
        // Malformed uploads are JSON 400s too (empty sequence: no UIDs
        // could be recovered).
        for (ct, body) in [
            ("application/json".to_string(), b"{}".to_vec()),
            ("multipart/related".to_string(), b"x".to_vec()),
        ] {
            let r = dispatch(&store, "http://x/wado", &stow_req("/studies", &ct, body));
            assert_eq!(r.status, 400);
            let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            assert!(v["00081198"]["Value"].as_array().unwrap().is_empty());
        }
    }

    #[test]
    fn stow_retrieve_url_lists_every_study() {
        let store = stored();
        let (ct, body) = encode_multipart(
            &[
                Part::new(
                    "application/dicom",
                    test_file("4.4.1", "4.4.1.1", "4.4.1.1.1", 2, 2),
                ),
                Part::new(
                    "application/dicom",
                    test_file("4.4.2", "4.4.2.1", "4.4.2.1.1", 2, 2),
                ),
                // Same study twice: one URL, not two.
                Part::new(
                    "application/dicom",
                    test_file("4.4.1", "4.4.1.2", "4.4.1.2.1", 2, 2),
                ),
            ],
            "bmulti",
            "application/dicom",
        )
        .unwrap();
        let r = dispatch(&store, "http://x/wado", &stow_req("/studies", &ct, body));
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let urls = v["00081190"]["Value"].as_array().unwrap();
        assert_eq!(urls.len(), 2, "{v}");
        assert_eq!(urls[0], serde_json::json!("http://x/wado/studies/4.4.1"));
        assert_eq!(urls[1], serde_json::json!("http://x/wado/studies/4.4.2"));
    }

    #[test]
    fn try_test_file_refuses_giant_dims() {
        // u16::MAX dims would allocate ~8 GiB: refused in-process,
        // without allocating.
        let err = try_test_file("1.1", "1.1.1", "1.1.1.1", 65535, 65535).unwrap_err();
        assert!(err.contains("exceed"), "{err}");
        let err = try_test_file("1.1", "1.1.1", "1.1.1.1", 2048, 2048).unwrap_err();
        assert!(err.contains("exceed"), "{err}");
        // In-cap dims still build.
        assert!(try_test_file("1.1", "1.1.1", "1.1.1.1", 1024, 1024).is_ok());
        assert!(try_test_file("1.1", "1.1.1", "1.1.1.1", 2, 2).is_ok());
    }

    #[test]
    fn clone_without_bulk_drops_only_bulk() {
        let mut ds = DataSet::new();
        ds.put_text(tags::PATIENT_ID, Vr::Lo, "W123");
        ds.put_bytes(tags::PIXEL_DATA, Vr::Ow, vec![1u8; 16]);
        ds.put_bytes(tags::ENCAPSULATED_DOCUMENT, Vr::Ob, b"%PDF".to_vec());
        let slim = clone_without_bulk(&ds);
        assert_eq!(slim.string(tags::PATIENT_ID), Some("W123"));
        assert!(!slim.contains(tags::PIXEL_DATA));
        assert!(!slim.contains(tags::ENCAPSULATED_DOCUMENT));
        // The source keeps its bulk (QIDO must not mutate the store).
        assert!(ds.contains(tags::PIXEL_DATA));
    }

    #[test]
    fn instance_frames_decodes_alongside_store_use() {
        use std::sync::Arc;
        // instance_frames snapshots under the lock and decodes after
        // it: concurrent readers/writers proceed instead of
        // serializing on a held mutex. (A hold-across-decode
        // regression still passes functionally — this pins the
        // concurrent shape so the refactor stays covered.)
        let store = Arc::new(stored());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let s = store.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    let frames = s
                        .instance_frames("1.2.3", "1.2.3.1", "1.2.3.1.1", &[1])
                        .unwrap()
                        .unwrap();
                    assert_eq!(frames.len(), 1);
                    let hits = s.search(
                        &QidoScope {
                            level: "STUDY",
                            study: None,
                            series: None,
                        },
                        &[],
                        10,
                        0,
                    );
                    assert_eq!(hits.len(), 1);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }
}
