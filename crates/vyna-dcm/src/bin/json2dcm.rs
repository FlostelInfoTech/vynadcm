//! `json2dcm`: convert DICOM JSON back to a Part-10 file.

#[path = "iocommon.rs"]
mod iocommon;

use std::path::PathBuf;

use clap::Parser;
use vyna_dcm::{FileMeta, TransferSyntax, dataset_from_json, tags, write_part10};

/// JSON-to-DICOM tool.
#[derive(Parser, Debug)]
#[command(name = "json2dcm", about = "Convert DICOM JSON to a Part-10 file")]
struct Args {
    /// DICOM JSON file to convert.
    file: PathBuf,
    /// Output Part-10 file.
    #[arg(short, long)]
    output: PathBuf,
    /// Transfer syntax UID for the dataset.
    #[arg(long, default_value = "1.2.840.10008.1.2.1")]
    transfer_syntax: String,
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("json2dcm: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let text = std::fs::read_to_string(&args.file).map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut ds = dataset_from_json(&json).map_err(|e| e.to_string())?;
    let ts = TransferSyntax::from_uid(&args.transfer_syntax)
        .ok_or_else(|| format!("unknown transfer syntax {}", args.transfer_syntax))?;
    // Encapsulated syntaxes need fragments, not bytes: re-fragment Pixel
    // Data when the JSON carried InlineBinary bytes.
    if ts.is_encapsulated()
        && let Some(el) = ds.get(tags::PIXEL_DATA).cloned()
        && let vyna_dcm::Value::Bytes(b) = &el.value
    {
        let b = b.clone();
        ds.remove(tags::PIXEL_DATA);
        ds.insert(vyna_dcm::DataElement::new(
            tags::PIXEL_DATA,
            el.vr,
            vyna_dcm::Value::Fragments(vyna_dcm::codec::fragment_payload(&b, 65536)),
        ));
    }
    let class = ds
        .string(tags::SOP_CLASS_UID)
        .ok_or("dataset without SOP Class UID")?
        .to_string();
    let inst = ds
        .string(tags::SOP_INSTANCE_UID)
        .ok_or("dataset without SOP Instance UID")?
        .to_string();
    let meta = FileMeta::new(class, inst, ts);
    let bytes = write_part10(&meta, &ds).map_err(|e| e.to_string())?;
    iocommon::write_atomic(&args.output, &bytes).map_err(|e| e.to_string())?;
    Ok(())
}
