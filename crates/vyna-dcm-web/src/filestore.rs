//! Directory-backed [`DicomWebStore`].
//!
//! [`FileStore`] keeps the same in-memory index, search, and bulk
//! semantics as [`MemoryStore`] (including
//! its DoS caps), and additionally persists every accepted instance
//! as a Part-10 file under `{dir}/{study}/{series}/{instance}.dcm`.
//! [`FileStore::open`] rescans that directory, so STOW uploads and
//! restarts survive each other; bulk blobs stay session-local.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use vyna_dcm_core::{DataSet, Tag, is_valid_uid};

use crate::server::{BulkBlob, DicomWebStore, MemoryStore, QidoScope, StoredIds, parse_instance};

/// Maximum directory depth [`FileStore::open`] descends: pathological
/// nesting must not overflow the scanner's stack.
const MAX_SCAN_DEPTH: usize = 64;

/// Counter disambiguating concurrent temp writes of one instance.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Thread-safe directory-backed [`DicomWebStore`].
///
/// Instances with syntactically invalid UIDs are rejected outright:
/// UIDs become path segments, and only strict-grammar UIDs (digits
/// and dots, [`is_valid_uid`]) can do that without escaping the root.
/// Unreadable files found by [`FileStore::open`] are skipped and
/// counted ([`FileStore::skipped`]), never fatal.
#[derive(Debug)]
pub struct FileStore {
    dir: PathBuf,
    mem: MemoryStore,
    skipped: usize,
}

impl FileStore {
    /// Open (creating) `dir` and index every `*.dcm` file under it.
    ///
    /// Symlinks are never followed and unreadable files are skipped
    /// (see [`FileStore::skipped`]); only directory creation and
    /// I/O failures on the root itself are errors.
    pub fn open(dir: impl Into<PathBuf>) -> std::result::Result<Self, String> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mem = MemoryStore::new();
        let mut skipped = 0usize;
        for file in collect_dcm_files(&dir) {
            let bytes = match std::fs::read(&file) {
                Ok(b) => b,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if mem.store_instance(Bytes::from(bytes), None).is_err() {
                skipped += 1;
            }
        }
        Ok(Self { dir, mem, skipped })
    }

    /// Number of indexed instances.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mem.len()
    }

    /// True when no instances are indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mem.is_empty()
    }

    /// Files skipped by [`FileStore::open`] (unreadable or unparsable).
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// Storage root.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Storage path for one instance, or `None` when any UID is not
/// strict-grammar: only [`is_valid_uid`] strings (digits and dots)
/// become segments, so the result cannot escape `dir`.
fn instance_path(dir: &Path, study: &str, series: &str, instance: &str) -> Option<PathBuf> {
    for uid in [study, series, instance] {
        if !is_valid_uid(uid) {
            return None;
        }
    }
    Some(dir.join(study).join(series).join(format!("{instance}.dcm")))
}

/// Collect `*.dcm` files under `dir`: recursive, sorted, never
/// following symlinks, depth-capped against pathological nesting.
fn collect_dcm_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_dir(dir, &mut out, 0);
    out.sort();
    out
}

fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth >= MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            collect_dir(&path, out, depth + 1);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dcm"))
        {
            out.push(path);
        }
    }
}

impl DicomWebStore for FileStore {
    fn search(
        &self,
        scope: &QidoScope,
        filters: &[(Tag, String)],
        limit: usize,
        offset: usize,
    ) -> Vec<DataSet> {
        self.mem.search(scope, filters, limit, offset)
    }

    fn instance_file(&self, study: &str, series: &str, instance: &str) -> Option<Bytes> {
        self.mem.instance_file(study, series, instance)
    }

    fn study_files(&self, study: &str) -> Vec<Bytes> {
        self.mem.study_files(study)
    }

    fn series_files(&self, study: &str, series: &str) -> Vec<Bytes> {
        self.mem.series_files(study, series)
    }

    fn instance_metadata(
        &self,
        study: &str,
        series: &str,
        instance: &str,
    ) -> Option<(DataSet, Vec<BulkBlob>)> {
        self.mem.instance_metadata(study, series, instance)
    }

    fn instance_frames(
        &self,
        study: &str,
        series: &str,
        instance: &str,
        frames: &[u32],
    ) -> std::result::Result<Option<Vec<Vec<u8>>>, String> {
        self.mem.instance_frames(study, series, instance, frames)
    }

    fn store_instance(
        &self,
        file: Bytes,
        study_hint: Option<&str>,
    ) -> std::result::Result<StoredIds, String> {
        // Same validation as `MemoryStore` (shared helper), before
        // anything touches disk: rejected inputs leave no file.
        let parsed = parse_instance(&file, study_hint)?;
        let path = instance_path(&self.dir, &parsed.study, &parsed.series, &parsed.instance)
            .ok_or_else(|| {
                format!(
                    "refusing to store instance with invalid UID {:?}/{:?}/{:?}",
                    parsed.study, parsed.series, parsed.instance
                )
            })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        // Same-directory temp + rename: readers never see a partial
        // file, and the `.tmp` suffix keeps crash litter out of
        // rescans (only `*.dcm` is collected).
        let tmp = path.with_extension("dcm.tmp");
        let tmp = tmp.with_file_name(format!(
            "{}.{}-{}.tmp",
            tmp.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(e) = std::fs::write(&tmp, &file).and_then(|()| std::fs::rename(&tmp, &path)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{}: {e}", path.display()));
        }
        // Re-validates identically (same bytes, same hint) and indexes.
        self.mem.store_instance(file, study_hint)
    }

    fn put_bulk(&self, blobs: Vec<(Bytes, String)>) -> Vec<String> {
        self.mem.put_bulk(blobs)
    }

    fn bulk_data(&self, id: &str) -> Option<(Bytes, String)> {
        self.mem.bulk_data(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_file;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vyna-filestore-{}-{}-{}",
            std::process::id(),
            name,
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn sample_uids(tag: &str) -> (String, String, String) {
        // `test_file` takes &str UIDs; suffix per test for isolation.
        (
            format!("1.2.300.{tag}"),
            format!("1.2.301.{tag}"),
            format!("1.2.302.{tag}"),
        )
    }

    #[test]
    fn store_round_trips_through_reopen() {
        let root = temp_root("reopen");
        let (study, series, instance) = sample_uids("1");
        let bytes = test_file(&study, &series, &instance, 16, 16);
        let expected = PathBuf::from(&root)
            .join(&study)
            .join(&series)
            .join(format!("{instance}.dcm"));
        {
            let store = FileStore::open(&root).unwrap();
            assert!(store.is_empty());
            let ids = store
                .store_instance(Bytes::from(bytes.clone()), None)
                .unwrap();
            assert_eq!(ids.instance, instance);
            assert_eq!(store.len(), 1);
            assert!(expected.is_file(), "{}", expected.display());
            assert_eq!(
                store.instance_file(&study, &series, &instance).unwrap()[..],
                bytes[..]
            );
        }
        // Drop + reopen: the file on disk re-indexes.
        let store = FileStore::open(&root).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.skipped(), 0);
        assert_eq!(
            store.instance_file(&study, &series, &instance).unwrap()[..],
            bytes[..]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_skips_garbage_and_counts_it() {
        let root = temp_root("skips");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("junk.dcm"), b"not a dicom file").unwrap();
        let (study, series, instance) = sample_uids("2");
        std::fs::write(
            root.join("good.dcm"),
            test_file(&study, &series, &instance, 8, 8),
        )
        .unwrap();
        let store = FileStore::open(&root).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.skipped(), 1);
        assert!(store.instance_file(&study, &series, &instance).is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restore_replaces_and_hint_mismatch_writes_nothing() {
        let root = temp_root("replace");
        let (study, series, instance) = sample_uids("3");
        let store = FileStore::open(&root).unwrap();
        let first = test_file(&study, &series, &instance, 8, 8);
        store
            .store_instance(Bytes::from(first), Some(&study))
            .unwrap();
        let second = test_file(&study, &series, &instance, 16, 16);
        store.store_instance(Bytes::from(second), None).unwrap();
        assert_eq!(store.len(), 1);
        // Hint mismatch: rejected, and no stray file appears.
        let err = store
            .store_instance(
                Bytes::from(test_file(&study, &series, &instance, 8, 8)),
                Some("9.9.9"),
            )
            .unwrap_err();
        assert!(err.contains("does not match file study"), "{err}");
        let files = collect_dcm_files(&root);
        assert_eq!(files.len(), 1, "{files:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn traversal_uids_have_no_path() {
        let root = PathBuf::from("/archive");
        assert!(instance_path(&root, "1.2.3", "1.2.4", "1.2.5").is_some());
        for bad in ["../x", "1.2/3", "..", "", "1..2", "1.2.3."] {
            assert!(
                instance_path(&root, bad, "1.2.4", "1.2.5").is_none(),
                "{bad}"
            );
            assert!(
                instance_path(&root, "1.2.3", bad, "1.2.5").is_none(),
                "{bad}"
            );
            assert!(
                instance_path(&root, "1.2.3", "1.2.4", bad).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn invalid_uid_files_are_rejected_without_disk_trace() {
        let root = temp_root("baduid");
        let store = FileStore::open(&root).unwrap();
        // `test_file` needs valid UIDs to build; corrupt the SOP
        // Instance UID post-encode instead: a `..` traversal payload
        // where the UID value sits must fail closed, not escape.
        let (study, series, instance) = sample_uids("4");
        let mut bytes = test_file(&study, &series, &instance, 8, 8);
        // Last occurrence: the dataset copy (the first sits in file
        // meta, which is not what storage reads for the instance UID).
        let pos = bytes
            .windows(instance.len())
            .rposition(|w| w == instance.as_bytes())
            .expect("instance UID present in fixture");
        bytes[pos..pos + 3].copy_from_slice(b"../");
        let err = store.store_instance(Bytes::from(bytes), None).unwrap_err();
        assert!(
            err.contains("invalid UID") || err.contains("missing SOP Instance UID"),
            "{err}"
        );
        assert!(collect_dcm_files(&root).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
