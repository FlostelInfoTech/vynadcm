//! `img2dcm`: replace the Pixel Data of a Part-10 file from a PNG.
//!
//! The template must use an uncompressed transfer syntax; geometry
//! tags are rewritten from the PNG (8/16-bit gray, 8-bit RGB) and
//! UIDs are kept (test/debug injection).

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{FileMeta, inject_png, read_part10, tags, write_part10};

/// PNG-to-DICOM tool.
#[derive(Parser, Debug)]
#[command(name = "img2dcm", about = "Inject PNG pixels into a Part-10 file")]
struct Args {
    /// PNG image to inject.
    image: PathBuf,
    /// Template Part-10 file.
    template: PathBuf,
    /// Output Part-10 file.
    #[arg(short, long)]
    output: PathBuf,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("img2dcm: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let png = std::fs::read(&args.image).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&args.template).map_err(|e| e.to_string())?;
    let (meta, mut ds) = read_part10(&bytes).map_err(|e| e.to_string())?;
    let ts = meta.transfer_syntax().map_err(|e| e.to_string())?;
    if !ts.is_uncompressed() {
        return Err(format!(
            "template uses {ts:?}; transcode to a native syntax first"
        ));
    }
    inject_png(&mut ds, &png).map_err(|e| e.to_string())?;
    let class = ds
        .string(tags::SOP_CLASS_UID)
        .ok_or("dataset without SOP Class UID")?
        .to_string();
    let inst = ds
        .string(tags::SOP_INSTANCE_UID)
        .ok_or("dataset without SOP Instance UID")?
        .to_string();
    let out = write_part10(&FileMeta::new(class, inst, ts), &ds).map_err(|e| e.to_string())?;
    std::fs::write(&args.output, out).map_err(|e| e.to_string())?;
    Ok(())
}
