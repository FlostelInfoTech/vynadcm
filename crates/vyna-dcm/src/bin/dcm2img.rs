//! `dcm2img`: render one frame of a Part-10 file to PNG.
//!
//! Supports MONOCHROME1/2 at 8/16 bits and RGB at 8 bits; YBR,
//! palette, and sub-byte depths are refused (transcode first).

#[path = "common.rs"]
mod common;

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{frame_to_png, read_part10};

/// DICOM-to-PNG tool.
#[derive(Parser, Debug)]
#[command(name = "dcm2img", about = "Render one DICOM frame to PNG")]
struct Args {
    /// Part-10 file to read.
    input: PathBuf,
    /// PNG file to write.
    output: PathBuf,
    /// 1-based frame number.
    #[arg(long, default_value_t = 1)]
    frame: usize,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("dcm2img: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let bytes = std::fs::read(&args.input).map_err(|e| e.to_string())?;
    let (meta, ds) = read_part10(&bytes).map_err(|e| e.to_string())?;
    let ts = meta.transfer_syntax().map_err(|e| e.to_string())?;
    let png = frame_to_png(&ds, ts, args.frame).map_err(|e| e.to_string())?;
    common::write_atomic(&args.output, &png).map_err(|e| e.to_string())?;
    Ok(())
}
