//! `dcm2xml`: convert a Part-10 file to Native DICOM Model XML (PS3.19 A).

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{dataset_to_xml, read_part10};

/// DICOM-to-XML tool.
#[derive(Parser, Debug)]
#[command(name = "dcm2xml", about = "Convert a Part-10 file to DICOM XML")]
struct Args {
    /// Part-10 file to convert.
    file: PathBuf,
    /// Include file meta (group 0x0002) attributes in the output.
    #[arg(long, default_value_t = false)]
    with_meta: bool,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("dcm2xml: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let bytes = std::fs::read(&args.file).map_err(|e| e.to_string())?;
    let (meta, ds) = read_part10(&bytes).map_err(|e| e.to_string())?;
    let mut both = ds.clone();
    if args.with_meta {
        for (_, el) in meta.to_dataset().iter() {
            both.insert(el.clone());
        }
    }
    print!("{}", dataset_to_xml(&both));
    Ok(())
}
