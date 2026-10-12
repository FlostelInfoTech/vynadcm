//! `dcm2pdf`: extract the encapsulated PDF from a Part-10 file.
//!
//! Exit status: 0 on success, 1 on failure.

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{pdf_from_dataset, read_part10, tags};

/// DICOM-to-PDF extraction tool.
#[derive(Parser, Debug)]
#[command(
    name = "dcm2pdf",
    about = "Extract the PDF from an Encapsulated PDF file"
)]
struct Args {
    /// Part-10 file to read.
    input: PathBuf,
    /// PDF file to write.
    output: PathBuf,
}

fn main() {
    std::process::exit(run(&Args::parse()));
}

fn run(args: &Args) -> i32 {
    let bytes = match std::fs::read(&args.input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("dcm2pdf: {}: {e}", args.input.display());
            return 1;
        }
    };
    let (meta, ds) = match read_part10(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("dcm2pdf: {}: {e}", args.input.display());
            return 1;
        }
    };
    let pdf = match pdf_from_dataset(&ds) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("dcm2pdf: {}: {e}", args.input.display());
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&args.output, &pdf.bytes) {
        eprintln!("dcm2pdf: {}: {e}", args.output.display());
        return 1;
    }
    println!(
        "{}: {} bytes (SOP Class {}, MIME {}{})",
        args.output.display(),
        pdf.bytes.len(),
        ds.string(tags::SOP_CLASS_UID)
            .unwrap_or(&meta.media_storage_sop_class_uid),
        pdf.mime_type.as_deref().unwrap_or("-"),
        pdf.title
            .as_deref()
            .map(|t| format!(", title {t:?}"))
            .unwrap_or_default()
    );
    0
}
