//! Freshness gate for the generated DICOM Conformance Statement.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn conformance_statement_is_current() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap();
    let out = Command::new("python3")
        .arg("docs/generate-conformance.py")
        .arg("--check")
        .current_dir(workspace)
        .output()
        .expect("run generate-conformance.py --check");
    assert!(
        out.status.success(),
        "docs/conformance.md is stale; rerun python3 docs/generate-conformance.py:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
