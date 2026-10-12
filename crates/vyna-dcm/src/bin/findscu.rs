//! `findscu`: C-FIND / Modality Worklist queries.

#[path = "common.rs"]
mod common;

use clap::Parser;
use common::{ConnArgs, anyhowish::Result, build_qr_query};
use vyna_dcm::core::sop_class;
use vyna_dcm::dump_dataset;

/// C-FIND SCU (Q/R + Modality Worklist).
#[derive(Parser, Debug)]
#[command(name = "findscu", about = "C-FIND query and print matches")]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
    /// Query SOP class: studyroot (default), patientroot, patientstudy, mwl.
    #[arg(long, default_value = "studyroot")]
    model: String,
    /// Matching keys as KEY=value (keyword or GGGGEEEE tag), e.g.
    /// PatientID=E123 Modality=ECG. Repeatable.
    #[arg(short, long = "key")]
    keys: Vec<String>,
    /// Print matches as DICOM JSON instead of text dump.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let sop = match args.model.to_ascii_lowercase().as_str() {
        "studyroot" | "study" => sop_class::STUDY_ROOT_FIND,
        "patientroot" | "patient" => sop_class::PATIENT_ROOT_FIND,
        "patientstudy" => sop_class::PATIENT_STUDY_ONLY_FIND,
        "mwl" | "worklist" | "modality-worklist" => sop_class::MODALITY_WORKLIST_FIND,
        other => {
            return Err(common::anyhowish::Error::msg(format!(
                "unknown model {other:?}"
            )));
        }
    };
    let is_mwl = sop == sop_class::MODALITY_WORKLIST_FIND;
    let query = build_qr_query(&args.keys, is_mwl)?;

    let mut assoc = args
        .conn
        .connect(&[sop], &ConnArgs::default_syntaxes())
        .await?;
    let mut count = 0;
    let status = assoc
        .find_with(sop, &query, 0, |ds| {
            count += 1;
            println!("--- match {count} ---");
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&vyna_dcm::dataset_to_json(&ds)).unwrap()
                );
            } else {
                print!("{}", dump_dataset(&ds));
            }
        })
        .await?;
    println!("C-FIND complete: {count} match(es), status {status:#06X}");
    assoc.release().await?;
    if status != 0 {
        return Err(common::anyhowish::Error::msg(format!(
            "C-FIND failed: {status:#06X}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm::tags;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn level_derives_from_deepest_uid() {
        let q = build_qr_query(&keys(&[]), false).unwrap();
        assert_eq!(q.string(tags::QUERY_RETRIEVE_LEVEL), Some("PATIENT"));
        let q = build_qr_query(&keys(&["StudyInstanceUID=1.2.3"]), false).unwrap();
        assert_eq!(q.string(tags::QUERY_RETRIEVE_LEVEL), Some("STUDY"));
        let q = build_qr_query(&keys(&["SeriesInstanceUID=1.2.3.4"]), false).unwrap();
        assert_eq!(q.string(tags::QUERY_RETRIEVE_LEVEL), Some("SERIES"));
        let q = build_qr_query(&keys(&["SOPInstanceUID=1.2.3.4.5"]), false).unwrap();
        assert_eq!(q.string(tags::QUERY_RETRIEVE_LEVEL), Some("IMAGE"));
    }

    #[test]
    fn mwl_carries_no_level() {
        let q = build_qr_query(&keys(&["Modality=ECG"]), true).unwrap();
        assert!(!q.contains(tags::QUERY_RETRIEVE_LEVEL));
        assert_eq!(q.string(tags::MODALITY), Some("ECG"));
    }

    #[test]
    fn keys_accept_keywords_and_hex_tags() {
        let q = build_qr_query(&keys(&["PatientID=E123", "00100010=Doe^Jane"]), false).unwrap();
        assert_eq!(q.string(tags::PATIENT_ID), Some("E123"));
        let names = q.person_names(tags::PATIENT_NAME).unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].to_dicom_string(), "Doe^Jane");
    }

    #[test]
    fn bad_keys_fail() {
        assert!(build_qr_query(&keys(&["NoEquals"]), false).is_err());
        assert!(build_qr_query(&keys(&["BogusKey=x"]), false).is_err());
    }
}
