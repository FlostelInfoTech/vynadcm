//! `findscu`: C-FIND / Modality Worklist queries.

#[path = "common.rs"]
mod common;

use clap::Parser;
use common::{ConnArgs, anyhowish::Result};
use vyna_dcm::core::{Tag, lookup_keyword, sop_class};
use vyna_dcm::{DataSet, Vr, dump_dataset};

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
    let query = build_query(&args.keys, is_mwl)?;

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

fn build_query(keys: &[String], is_mwl: bool) -> Result<DataSet> {
    let mut ds = DataSet::new();
    for key in keys {
        let (name, value) = key
            .split_once('=')
            .ok_or_else(|| format!("key {key:?} must be KEY=value"))?;
        let tag = parse_key(name)?;
        // Look up the VR from the dictionary; default to LO for unknowns.
        let vr = vyna_dcm::core::dictionary_vr(tag).unwrap_or(Vr::Lo);
        if tag == vyna_dcm::tags::PATIENT_NAME {
            ds.put_person_name(tag, vec![value.parse().map_err(|e| format!("{e:?}"))?]);
        } else {
            ds.put_text(tag, vr, value);
        }
    }
    // Q/R models require Query/Retrieve Level; derive it from the keys
    // (MWL identifiers must not carry it).
    if !is_mwl && !ds.contains(vyna_dcm::tags::QUERY_RETRIEVE_LEVEL) {
        use vyna_dcm::tags as t;
        let level = if ds.contains(t::SOP_INSTANCE_UID) {
            "IMAGE"
        } else if ds.contains(t::SERIES_INSTANCE_UID) {
            "SERIES"
        } else if ds.contains(t::STUDY_INSTANCE_UID) {
            "STUDY"
        } else {
            "PATIENT"
        };
        ds.put_text(t::QUERY_RETRIEVE_LEVEL, Vr::Cs, level);
    }
    Ok(ds)
}

fn parse_key(name: &str) -> Result<Tag> {
    if let Some(e) = lookup_keyword(name) {
        return Ok(e.tag);
    }
    if let Some(e) = vyna_dcm::core::ENTRIES
        .iter()
        .find(|e| e.keyword.eq_ignore_ascii_case(name))
    {
        return Ok(e.tag);
    }
    let hex: String = name.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() == 8 {
        let g = u16::from_str_radix(&hex[0..4], 16).unwrap();
        let e = u16::from_str_radix(&hex[4..8], 16).unwrap();
        return Ok(Tag::new(g, e));
    }
    Err(common::anyhowish::Error::msg(format!(
        "unknown query key {name:?}"
    )))
}
