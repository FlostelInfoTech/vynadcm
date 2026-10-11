//! `dcm2json`: convert a Part-10 file to DICOM JSON (PS3.18 Annex F).

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{dataset_to_json, read_part10};

/// DICOM-to-JSON tool.
#[derive(Parser, Debug)]
#[command(name = "dcm2json", about = "Convert a Part-10 file to DICOM JSON")]
struct Args {
    /// Part-10 file to convert.
    file: PathBuf,
    /// Include file meta (group 0x0002) as "00020000" sibling object.
    #[arg(long, default_value_t = false)]
    with_meta: bool,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("dcm2json: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let bytes = std::fs::read(&args.file).map_err(|e| e.to_string())?;
    let (meta, ds) = read_part10(&bytes).map_err(|e| e.to_string())?;
    let mut out = dataset_to_json(&ds);
    if args.with_meta {
        let meta_ds = meta.to_dataset();
        let entry = out.as_object_mut().unwrap();
        for (tag, el) in meta_ds.iter() {
            entry.insert(
                format!("{:04X}{:04X}", tag.group(), tag.element()),
                dataset_to_json(&{
                    let mut single = vyna_dcm::DataSet::new();
                    single.insert(el.clone());
                    single
                })
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .clone(),
            );
        }
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(())
}
