//! `dcmdump`: print a Part-10 file as human-readable text.

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{dump_dataset, read_part10};

/// DICOM dump tool.
#[derive(Parser, Debug)]
#[command(name = "dcmdump", about = "Dump a Part-10 file as text")]
struct Args {
    /// Part-10 file to dump.
    file: PathBuf,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("dcmdump: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let bytes = std::fs::read(&args.file).map_err(|e| e.to_string())?;
    let (meta, ds) = read_part10(&bytes).map_err(|e| e.to_string())?;
    println!("# file: {}", args.file.display());
    println!(
        "# media-storage: {} / {}",
        meta.media_storage_sop_class_uid, meta.media_storage_sop_instance_uid
    );
    println!("# transfer-syntax: {}", meta.transfer_syntax_uid);
    println!(
        "# implementation: {} {:?}",
        meta.implementation_class_uid, meta.implementation_version_name
    );
    print!("{}", dump_dataset(&ds));
    Ok(())
}
