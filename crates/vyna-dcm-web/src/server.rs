//! DICOMweb server core: framework-agnostic dispatch + in-memory store.
//!
//! [`dispatch`] routes an [`HttpRequest`] to an [`HttpResponse`] against any
//! [`DicomWebStore`]. The `server` feature adds a hyper HTTP/1.1 front end
//! ([`crate::hyper_server`]); embedders can also adapt `dispatch` to their
//! own HTTP stack (as VynaLink does).

use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use vyna_dcm_codec::{FileMeta, dataset_to_json, decode_pixels, read_part10, write_part10};
use vyna_dcm_core::{DataSet, Tag, TransferSyntax, Value, Vr, lookup_keyword, tags};

use crate::multipart::{
    Part, decode_multipart, encode_multipart, new_boundary, parse_content_type,
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
#[derive(Clone, Debug)]
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

/// Framework-agnostic HTTP response.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// JSON response.
    #[must_use]
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/dicom+json".into())],
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    /// Plain-text response.
    #[must_use]
    pub fn text(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: message.into().into_bytes(),
        }
    }

    /// `multipart/related` response.
    #[must_use]
    pub fn multipart(parts: &[Part], root_type: &str) -> Self {
        let (content_type, body) = encode_multipart(parts, &new_boundary(), root_type);
        Self {
            status: 200,
            headers: vec![("content-type".into(), content_type)],
            body,
        }
    }

    /// Raw-bytes response.
    #[must_use]
    pub fn bytes(status: u16, content_type: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body,
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

    /// Full Part-10 bytes of an instance.
    fn instance_file(&self, study: &str, series: &str, instance: &str) -> Option<Vec<u8>>;

    /// Metadata dataset (bulk elements removed) plus extracted blobs.
    fn instance_metadata(
        &self,
        study: &str,
        series: &str,
        instance: &str,
    ) -> Option<(DataSet, Vec<BulkBlob>)>;

    /// Raw native-order frame bytes for 1-based frame numbers.
    fn instance_frames(
        &self,
        study: &str,
        series: &str,
        instance: &str,
        frames: &[u32],
    ) -> Option<Vec<Vec<u8>>>;

    /// Store one Part-10 file; `study_hint` (STOW URL) must match the file.
    fn store_instance(
        &self,
        file: &[u8],
        study_hint: Option<&str>,
    ) -> std::result::Result<StoredIds, String>;

    /// Register blobs, returning one id per blob for `BulkDataURI`s.
    fn put_bulk(&self, blobs: Vec<(Vec<u8>, String)>) -> Vec<String>;

    /// Resolve a bulk id to `(bytes, content_type)`.
    fn bulk_data(&self, id: &str) -> Option<(Vec<u8>, String)>;
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
    bulk: HashMap<String, (Vec<u8>, String)>,
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
    file: Vec<u8>,
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
            if scope.level == "STUDY" && !seen_studies.insert(inst.study.clone()) {
                continue;
            }
            if scope.level == "SERIES"
                && !seen_series.insert((inst.study.clone(), inst.series.clone()))
            {
                continue;
            }
            if !filters
                .iter()
                .all(|(t, m)| match_attribute(&inst.dataset, *t, m))
            {
                continue;
            }
            let mut ds = inst.dataset.clone();
            strip_bulk(&mut ds);
            out.push(ds);
            if out.len() >= offset.saturating_add(limit) {
                break;
            }
        }
        out.into_iter().skip(offset).take(limit).collect()
    }

    fn instance_file(&self, study: &str, series: &str, instance: &str) -> Option<Vec<u8>> {
        let inner = self.lock();
        inner
            .instances
            .iter()
            .find(|i| i.study == study && i.series == series && i.instance == instance)
            .map(|i| i.file.clone())
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
    ) -> Option<Vec<Vec<u8>>> {
        let inner = self.lock();
        let inst = inner
            .instances
            .iter()
            .find(|i| i.study == study && i.series == series && i.instance == instance)?;
        let pixels = decode_pixels(&inst.dataset, inst.transfer_syntax).ok()?;
        // decode_pixels already validated the frame count; absent means 1.
        let count: usize = inst
            .dataset
            .string(tags::NUMBER_OF_FRAMES)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
            .max(1);
        if pixels.len() % count.max(1) != 0 {
            return None;
        }
        let frame_len = pixels.len() / count.max(1);
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            if *f == 0 || *f as usize > count {
                return None;
            }
            let start = (*f as usize - 1) * frame_len;
            out.push(pixels[start..start + frame_len].to_vec());
        }
        Some(out)
    }

    fn store_instance(
        &self,
        file: &[u8],
        study_hint: Option<&str>,
    ) -> std::result::Result<StoredIds, String> {
        let (meta, dataset) = read_part10(file).map_err(|e| e.to_string())?;
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
            inner.instance_bytes -= old.file.len();
        }
        inner.instance_bytes += file.len();
        inner.instances.push_back(StoredInstance {
            study: study.clone(),
            series: series.clone(),
            instance: instance.clone(),
            file: file.to_vec(),
            dataset,
            transfer_syntax: ts,
        });
        while inner.instances.len() > MAX_INSTANCES || inner.instance_bytes > MAX_INSTANCE_BYTES {
            let Some(oldest) = inner.instances.pop_front() else {
                break;
            };
            inner.instance_bytes -= oldest.file.len();
        }
        Ok(StoredIds {
            study,
            series,
            instance,
        })
    }

    fn put_bulk(&self, blobs: Vec<(Vec<u8>, String)>) -> Vec<String> {
        let mut inner = self.lock();
        blobs
            .into_iter()
            .map(|(bytes, ct)| {
                // Unpredictable ids: sequential hex ids let anyone
                // enumerate every stored blob.
                let id = uuid::Uuid::new_v4().as_simple().to_string();
                inner.bulk_bytes += bytes.len();
                inner.bulk_order.push_back(id.clone());
                inner.bulk.insert(id.clone(), (bytes, ct));
                while inner.bulk.len() > MAX_BULK_BLOBS || inner.bulk_bytes > MAX_BULK_BYTES {
                    let Some(oldest) = inner.bulk_order.pop_front() else {
                        break;
                    };
                    if let Some((b, _)) = inner.bulk.remove(&oldest) {
                        inner.bulk_bytes -= b.len();
                    }
                }
                id
            })
            .collect()
    }

    fn bulk_data(&self, id: &str) -> Option<(Vec<u8>, String)> {
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
                    "application/pdf".to_string()
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

/// Case-insensitive keyword/tag matching for one QIDO filter.
fn match_attribute(ds: &DataSet, tag: Tag, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true; // universal match / return key
    }
    let Some(el) = ds.get(tag) else { return false };
    let values: Vec<String> = match &el.value {
        Value::Text(v) => v.clone(),
        Value::PersonName(n) => n.iter().map(|p| p.to_dicom_string()).collect(),
        Value::U16(v) => v.iter().map(ToString::to_string).collect(),
        Value::U32(v) => v.iter().map(ToString::to_string).collect(),
        Value::I16(v) => v.iter().map(ToString::to_string).collect(),
        Value::I32(v) => v.iter().map(ToString::to_string).collect(),
        _ => return false,
    };
    // Comma-separated list = OR; for DA/TM/DT/DS/IS, `a-b` is an
    // inclusive range (numeric for DS/IS, lexicographic for the
    // fixed-width datetimes); trailing `*` = prefix; otherwise exact
    // (case-insensitive for PN). A `-` in any other VR is a literal
    // character, not a range (mirrors C-FIND matching).
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
                let hit = if matches!(el.vr, Vr::Ds | Vr::Is) {
                    let bound = |s: &str, open: f64| {
                        if s.is_empty() {
                            Some(open)
                        } else {
                            s.parse().ok()
                        }
                    };
                    match (bound(lo, f64::NEG_INFINITY), bound(hi, f64::INFINITY)) {
                        (Some(lo), Some(hi)) => values
                            .iter()
                            .any(|v| v.parse::<f64>().is_ok_and(|v| v >= lo && v <= hi)),
                        _ => false,
                    }
                } else {
                    values.iter().any(|v| {
                        (lo.is_empty() || v.as_str() >= lo) && (hi.is_empty() || v.as_str() <= hi)
                    })
                };
                if hit {
                    return true;
                }
                continue;
            }
            // Lone leading '-' (a negative literal): fall through to
            // prefix/exact below.
        }
        if let Some(prefix) = alt.strip_suffix('*') {
            // PN matching is case-insensitive (as for exact matches);
            // byte-prefix slicing must respect UTF-8 boundaries.
            let hit = if el.vr == Vr::Pn {
                values.iter().any(|v| {
                    v.get(..prefix.len())
                        .is_some_and(|h| h.eq_ignore_ascii_case(prefix))
                })
            } else {
                values.iter().any(|v| v.starts_with(prefix))
            };
            if hit {
                return true;
            }
            continue;
        }
        if el.vr == Vr::Pn {
            if values.iter().any(|v| v.eq_ignore_ascii_case(alt)) {
                return true;
            }
        } else if values.iter().any(|v| v == alt) {
            return true;
        }
    }
    false
}

/// Route one request against a store. `public_base` is the externally
/// visible base URL used for `BulkDataURI`s and RetrieveURLs.
pub fn dispatch<S: DicomWebStore>(store: &S, public_base: &str, req: &HttpRequest) -> HttpResponse {
    let base = public_base.trim_end_matches('/');
    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["studies"]) => qido(
            store,
            QidoScope {
                level: "STUDY",
                study: None,
                series: None,
            },
            &req.query,
        ),
        ("GET", ["series"]) => qido(
            store,
            QidoScope {
                level: "SERIES",
                study: None,
                series: None,
            },
            &req.query,
        ),
        ("GET", ["instances"]) => qido(
            store,
            QidoScope {
                level: "IMAGE",
                study: None,
                series: None,
            },
            &req.query,
        ),
        ("GET", ["studies", study, "series"]) => qido(
            store,
            QidoScope {
                level: "SERIES",
                study: Some((*study).to_string()),
                series: None,
            },
            &req.query,
        ),
        ("GET", ["studies", study, "series", series, "instances"]) => qido(
            store,
            QidoScope {
                level: "IMAGE",
                study: Some((*study).to_string()),
                series: Some((*series).to_string()),
            },
            &req.query,
        ),
        ("GET", ["studies", study, "series", series, "instances", instance]) => {
            match store.instance_file(study, series, instance) {
                Some(file) => HttpResponse::multipart(
                    &[Part::new("application/dicom", file)],
                    "application/dicom",
                ),
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
        ) => match store.instance_metadata(study, series, instance) {
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
                        .map(|b| (b.bytes, b.content_type))
                        .collect(),
                );
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
        },
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
            // Every addressed frame must be a positive integer: bad
            // entries are a 400 (not silently dropped, and frame 0 is
            // invalid — frames are 1-based).
            let mut frames = Vec::new();
            for f in list.split(',') {
                match f.parse::<u32>() {
                    Ok(n) if n >= 1 => frames.push(n),
                    _ => return HttpResponse::text(400, "bad frame list"),
                }
            }
            if frames.is_empty() {
                return HttpResponse::text(400, "bad frame list");
            }
            match store.instance_frames(study, series, instance, &frames) {
                Some(raw) => {
                    let parts: Vec<Part> = raw
                        .into_iter()
                        .map(|b| Part::new("application/octet-stream", b))
                        .collect();
                    HttpResponse::multipart(&parts, "application/octet-stream")
                }
                None => HttpResponse::text(404, "instance or frames not found"),
            }
        }
        ("GET", ["bulk", id]) => match store.bulk_data(id) {
            Some((bytes, ct)) => HttpResponse::bytes(200, ct, bytes),
            None => HttpResponse::text(404, "bulk data not found"),
        },
        ("POST", ["studies"]) => stow(store, base, None, req),
        ("POST", ["studies", study]) => stow(store, base, Some(study), req),
        ("GET", _) => HttpResponse::text(404, "unknown DICOMweb resource"),
        _ => HttpResponse::text(405, "method not allowed"),
    }
}

/// Maximum QIDO query parameters: each unknown key scans the whole
/// tag dictionary, so an unbounded list is a CPU DoS.
const MAX_QIDO_PARAMS: usize = 100;

fn qido<S: DicomWebStore>(store: &S, scope: QidoScope, query: &str) -> HttpResponse {
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
            "includefield" | "fuzzymatching" => {} // accepted, matching stays exact/prefix
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
    // 8-hex-digit tag, with optional comma/space separators.
    let hex: String = key.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() == 8 {
        let g = u16::from_str_radix(&hex[0..4], 16).ok()?;
        let e = u16::from_str_radix(&hex[4..8], 16).ok()?;
        return Some(Tag::new(g, e));
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
    let ct = req
        .headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("");
    let (mime, params) = parse_content_type(ct);
    if mime != "multipart/related" {
        return HttpResponse::text(400, "STOW needs multipart/related");
    }
    let Some(boundary) = params.get("boundary") else {
        return HttpResponse::text(400, "STOW multipart without boundary");
    };
    let parts = match decode_multipart(&req.body, boundary) {
        Ok(p) => p,
        Err(e) => return HttpResponse::text(400, e.to_string()),
    };
    let mut stored: Vec<StoredIds> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for part in parts
        .iter()
        .filter(|p| p.content_type.eq_ignore_ascii_case("application/dicom"))
    {
        match store.store_instance(&part.body, study_hint) {
            Ok(ids) => stored.push(ids),
            Err(reason) => failures.push(reason),
        }
    }
    if stored.is_empty() {
        return HttpResponse::text(400, format!("nothing stored: {}", failures.join("; ")));
    }
    if !failures.is_empty() {
        // Partial failure: 409 with a FailedSOPSequence-shaped body.
        return HttpResponse::json(
            409,
            &serde_json::json!({
                "00081198": {"vr": "SQ", "Value": []},
                "00081190": {"vr": "UR", "Value": [format!("{base}/studies/{}", stored[0].study)]},
                "error": failures.join("; "),
            }),
        );
    }
    HttpResponse::json(
        200,
        &serde_json::json!({
            "00081190": {"vr": "UR", "Value": [format!("{base}/studies/{}", stored[0].study)]}
        }),
    )
}

/// Build a Part-10 test file (also used by doc examples).
#[doc(hidden)]
pub fn test_file(study: &str, series: &str, instance: &str, rows: u16, cols: u16) -> Vec<u8> {
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
    ds.put_bytes(
        tags::PIXEL_DATA,
        Vr::Ow,
        vec![0u8; rows as usize * cols as usize * 2],
    );
    let meta = FileMeta::new(
        vyna_dcm_core::sop_class::CT_IMAGE,
        instance,
        TransferSyntax::ExplicitVrLittleEndian,
    );
    write_part10(&meta, &ds).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm_codec::dataset_from_json;

    fn stored() -> MemoryStore {
        let store = MemoryStore::new();
        store
            .store_instance(&test_file("1.2.3", "1.2.3.1", "1.2.3.1.1", 4, 4), None)
            .unwrap();
        store
            .store_instance(&test_file("1.2.3", "1.2.3.2", "1.2.3.2.1", 2, 2), None)
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

    #[test]
    fn bulk_ids_are_unpredictable_and_evict_oldest() {
        let store = MemoryStore::new();
        let ids = store.put_bulk(vec![
            (vec![1u8; 8], "application/octet-stream".to_string()),
            (vec![2u8; 8], "application/octet-stream".to_string()),
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
            let id = store.put_bulk(vec![(vec![0u8; 8], "application/octet-stream".to_string())]);
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
                .store_instance(&test_file("1.2.3", "1.2.3.1", &uid, 2, 2), None)
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

        let file = test_file("9.9.9", "9.9.9.1", "9.9.9.1.1", 2, 2);
        let (ct, body) = encode_multipart(
            &[Part::new("application/dicom", file)],
            "b1",
            "application/dicom",
        );
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
        assert_eq!(store.len(), 3);

        // Study mismatch is rejected.
        let file = test_file("8.8.8", "8.8.8.1", "8.8.8.1.1", 2, 2);
        let (ct, body) = encode_multipart(
            &[Part::new("application/dicom", file)],
            "b2",
            "application/dicom",
        );
        let req = HttpRequest {
            method: "POST".into(),
            path: "/studies/7.7.7".into(),
            query: String::new(),
            headers: [("content-type".into(), ct)].into(),
            body: Bytes::from(body),
        };
        assert_eq!(dispatch(&store, "http://x/wado", &req).status, 400);
    }

    #[test]
    fn unknown_routes_404() {
        let store = stored();
        assert_eq!(dispatch(&store, "http://x", &get("/nope")).status, 404);
        let mut bad = get("/studies");
        bad.method = "DELETE".into();
        assert_eq!(dispatch(&store, "http://x", &bad).status, 405);
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
}
