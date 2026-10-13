//! Shared file-output plumbing for the `vyna-dcm` tools.
//!
//! Deliberately dependency-free (no DIMSE/DICOMweb): file-only tools
//! build without the `net`/`web` features.
#![allow(dead_code)] // each binary uses a subset

/// Write `bytes` to `dest` atomically: temp file in the same
/// directory (so the rename stays on one filesystem) plus rename, so
/// a crash or a concurrent reader never sees a truncated output.
/// Cleans up the temp file on failure.
pub fn write_atomic(dest: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let stem = dest
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    for attempt in 0..10u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = dir.join(format!(
            ".{stem}.tmp-{}-{nanos}-{attempt}",
            std::process::id()
        ));
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        // No fsync: rename atomicity (never a truncated output) is
        // the guarantee; per-file syncs would throttle receivers.
        let r = f.write_all(bytes).and_then(|()| {
            drop(f);
            std::fs::rename(&tmp, dest)
        });
        match r {
            Ok(()) => return Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
    Err(Error::new(
        ErrorKind::AlreadyExists,
        format!("cannot pick a temp name in {}", dir.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::write_atomic;

    #[test]
    fn atomic_write_round_trip() {
        let dir = std::env::temp_dir().join(format!("vyna-iocommon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.bin");
        write_atomic(&dest, b"one").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");
        write_atomic(&dest, b"two").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"two");
        // Exactly one directory entry: no temp files left behind.
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
