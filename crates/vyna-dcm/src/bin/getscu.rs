//! `getscu`: C-GET retrieve into Part-10 files over the same association.

#[path = "common.rs"]
mod common;

use std::path::PathBuf;

use clap::Parser;
use common::{ConnArgs, anyhowish::Result, build_qr_query, safe_instance_filename};
use vyna_dcm::core::sop_class;
use vyna_dcm::{FileMeta, TransferSyntax};

/// C-GET SCU.
#[derive(Parser, Debug)]
#[command(
    name = "getscu",
    about = "C-GET retrieve into a directory (instances arrive on this association)"
)]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
    /// Query model: studyroot (default) or patientroot (no Patient/Study-only GET).
    #[arg(long, default_value = "studyroot")]
    model: String,
    /// Matching keys as KEY=value (keyword or GGGGEEEE tag). Repeatable.
    #[arg(short, long = "key")]
    keys: Vec<String>,
    /// Output directory for retrieved instances.
    #[arg(long, default_value = ".")]
    output_dir: PathBuf,
}

fn get_sop_for_model(model: &str) -> Result<&'static str> {
    match model.to_ascii_lowercase().as_str() {
        "studyroot" | "study" => Ok(sop_class::STUDY_ROOT_GET),
        "patientroot" | "patient" => Ok(sop_class::PATIENT_ROOT_GET),
        other => Err(common::anyhowish::Error::msg(format!(
            "unknown model {other:?} (C-GET has no Patient/Study-only or MWL form)"
        ))),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let sop = get_sop_for_model(&args.model)?;
    let query = build_qr_query(&args.keys, false)?;
    std::fs::create_dir_all(&args.output_dir)?;

    // Q/R context plus every storage class we can receive back.
    let mut sops = vec![sop];
    sops.extend(sop_class::DEFAULT_STORAGE_CLASSES.iter().copied());
    let mut assoc = args
        .conn
        .connect(&sops, &ConnArgs::default_syntaxes())
        .await?;

    let mut count = 0u32;
    let dir = &args.output_dir;
    let r = assoc
        .get_with(sop, &query, 0, |class: String, inst: String, ds, ts| {
            // Encapsulated arrivals transcode to native; store Explicit LE.
            let ds = if ts.is_encapsulated() {
                match vyna_dcm::transcode_dataset(
                    &ds,
                    ts,
                    TransferSyntax::ExplicitVrLittleEndian,
                    &vyna_dcm::TranscodeOptions::default(),
                ) {
                    Ok(ds) => ds,
                    Err(e) => {
                        println!("transcode failed for {inst}: {e}");
                        return 0xA900; // dataset does not match SOP class
                    }
                }
            } else {
                ds
            };
            let meta = FileMeta::new(&class, &inst, TransferSyntax::ExplicitVrLittleEndian);
            let path = dir.join(format!("{}.dcm", safe_instance_filename(&inst)));
            match vyna_dcm::write_part10(&meta, &ds) {
                Ok(bytes) => match std::fs::write(&path, bytes) {
                    Ok(()) => {
                        count += 1;
                        println!("stored {inst} -> {}", path.display());
                        0x0000
                    }
                    Err(e) => {
                        println!("write failed for {inst}: {e}");
                        0xA700 // out of resources
                    }
                },
                Err(e) => {
                    println!("encode failed for {inst}: {e}");
                    0xA900 // dataset does not match SOP class
                }
            }
        })
        .await?;
    println!(
        "C-GET complete: {count} file(s), {} failed, status {:#06X}",
        r.failed, r.status
    );
    assoc.release().await?;
    if r.status != 0 {
        return Err(common::anyhowish::Error::msg(format!(
            "C-GET failed: {:#06X}",
            r.status
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_map_to_get_sops() {
        assert_eq!(
            get_sop_for_model("studyroot").unwrap(),
            sop_class::STUDY_ROOT_GET
        );
        assert_eq!(
            get_sop_for_model("patient").unwrap(),
            sop_class::PATIENT_ROOT_GET
        );
        assert!(get_sop_for_model("patientstudy").is_err());
        assert!(get_sop_for_model("mwl").is_err());
    }
}
