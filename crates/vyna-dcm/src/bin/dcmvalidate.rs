//! `dcmvalidate`: check a Part-10 file against its IOD.
//!
//! Exit status: 0 when no errors were found (warnings allowed), 1 when
//! errors were found or the file could not be read, 2 when the SOP Class
//! is not supported.

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{Severity, TransferSyntax, read_part10, tags, validate};

/// DICOM IOD validation tool.
#[derive(Parser, Debug)]
#[command(
    name = "dcmvalidate",
    about = "Validate a Part-10 file against its IOD"
)]
struct Args {
    /// Part-10 file to validate.
    file: PathBuf,
    /// Override the SOP Class UID (default: from the dataset, else file meta).
    #[arg(long)]
    sop_class: Option<String>,
}

fn main() {
    std::process::exit(run(&Args::parse()));
}

fn run(args: &Args) -> i32 {
    let bytes = match std::fs::read(&args.file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcmvalidate: {}: {e}", args.file.display());
            return 1;
        }
    };
    let (meta, ds) = match read_part10(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("dcmvalidate: {}: {e}", args.file.display());
            return 1;
        }
    };
    let sop_uid = args.sop_class.clone().unwrap_or_else(|| {
        ds.string(tags::SOP_CLASS_UID)
            .map(str::to_string)
            .unwrap_or_else(|| meta.media_storage_sop_class_uid.clone())
    });
    let ts = TransferSyntax::from_uid(meta.transfer_syntax_uid.trim_end_matches(['\0', ' ']));
    if ts.is_none() {
        eprintln!(
            "dcmvalidate: {}: unknown transfer syntax {}",
            args.file.display(),
            meta.transfer_syntax_uid
        );
        return 1;
    }
    let Some(report) = validate(&ds, &sop_uid, ts) else {
        eprintln!("dcmvalidate: unsupported SOP Class UID {sop_uid}");
        return 2;
    };
    println!("# file: {}", args.file.display());
    println!("# iod: {} ({})", report.iod.name, report.iod.uid);
    let mut errors = 0;
    let mut warnings = 0;
    // File-meta consistency on top of the IOD findings.
    for (tag, meta_uid, what) in [
        (
            tags::SOP_CLASS_UID,
            meta.media_storage_sop_class_uid.as_str(),
            "SOP Class UID",
        ),
        (
            tags::SOP_INSTANCE_UID,
            meta.media_storage_sop_instance_uid.as_str(),
            "SOP Instance UID",
        ),
    ] {
        if let Some(ds_uid) = ds.string(tag)
            && ds_uid.trim_end_matches(['\0', ' ']) != meta_uid.trim_end_matches(['\0', ' '])
        {
            println!(
                "[error] ({tag}) MediaStorageMismatch []: file meta {what} {meta_uid} differs from dataset {ds_uid}"
            );
            errors += 1;
        }
    }
    for f in &report.findings {
        println!("{f}");
        match f.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
        }
    }
    println!("# {errors} error(s), {warnings} warning(s)");
    i32::from(errors > 0)
}
