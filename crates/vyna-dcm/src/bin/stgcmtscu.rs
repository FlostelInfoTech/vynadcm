//! `stgcmtscu`: Storage Commitment Push N-ACTION + wait for the report.

#[path = "common.rs"]
mod common;

use std::path::{Path, PathBuf};

use clap::Parser;
use common::{ConnArgs, anyhowish::Result};
use vyna_dcm::core::sop_class;
use vyna_dcm::net::worklist::storage_commitment_request;

/// Storage Commitment SCU (Push model, same association).
#[derive(Parser, Debug)]
#[command(
    name = "stgcmtscu",
    about = "Request storage commitment, then wait for the N-EVENT-REPORT (--timeout-secs bounds the wait)"
)]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
    /// Part-10 file whose SOP Class/Instance UIDs join the request. Repeatable.
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    /// Explicit instance as CLASS_UID:INSTANCE_UID. Repeatable.
    #[arg(long = "instance")]
    instances: Vec<String>,
    /// Storage Commitment request SOP Instance UID (generated when omitted).
    #[arg(long)]
    request_uid: Option<String>,
}

fn parse_instance_pair(s: &str) -> Result<(String, String)> {
    match s.split_once(':') {
        Some((c, i)) if !c.is_empty() && !i.is_empty() => Ok((c.to_string(), i.to_string())),
        _ => Err(common::anyhowish::Error::msg(format!(
            "instance {s:?} must be CLASS_UID:INSTANCE_UID"
        ))),
    }
}

fn uids_from_file(path: &Path) -> Result<(String, String)> {
    let bytes = std::fs::read(path)?;
    let (meta, _ds) = vyna_dcm::read_part10(&bytes).map_err(|e| e.to_string())?;
    Ok((
        meta.media_storage_sop_class_uid,
        meta.media_storage_sop_instance_uid,
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for f in &args.files {
        pairs.push(uids_from_file(f)?);
    }
    for s in &args.instances {
        pairs.push(parse_instance_pair(s)?);
    }
    if pairs.is_empty() {
        return Err(common::anyhowish::Error::msg(
            "need at least one --file or --instance",
        ));
    }
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(c, i)| (c.as_str(), i.as_str()))
        .collect();
    let (request, txn) = storage_commitment_request(&refs);
    let request_uid = args
        .request_uid
        .clone()
        .unwrap_or_else(vyna_dcm::core::new_uid);

    let sop = sop_class::STORAGE_COMMITMENT_PUSH;
    let mut assoc = args
        .conn
        .connect(&[sop], &ConnArgs::default_syntaxes())
        .await?;
    println!("N-ACTION sent, transaction {txn}; waiting for N-EVENT-REPORT...");
    let (action_status, report) = assoc.storage_commit(sop, &request_uid, &request).await?;
    assoc.release().await?;
    if action_status != 0 || report.is_none() {
        return Err(common::anyhowish::Error::msg(format!(
            "Storage Commitment N-ACTION failed: {action_status:#06X}"
        )));
    }
    let (got_txn, succeeded, failed) = report.unwrap();
    if got_txn.as_deref() != Some(txn.as_str()) {
        println!(
            "warning: report transaction {} does not match request {txn}",
            got_txn.as_deref().unwrap_or("?")
        );
    }
    println!(
        "commitment {txn}: {} succeeded, {} failed",
        succeeded.len(),
        failed.len()
    );
    for (c, i) in &failed {
        println!("  FAILED {c} {i}");
    }
    if !failed.is_empty() {
        return Err(common::anyhowish::Error::msg(format!(
            "{} instance(s) not committed",
            failed.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyna_dcm::core::TransferSyntax;

    #[test]
    fn instance_pairs_parse() {
        assert_eq!(
            parse_instance_pair("1.2:1.3").unwrap(),
            ("1.2".to_string(), "1.3".to_string())
        );
        assert!(parse_instance_pair("1.2").is_err());
        assert!(parse_instance_pair(":1.3").is_err());
        assert!(parse_instance_pair("1.2:").is_err());
    }

    #[test]
    fn file_uids_come_from_meta() {
        let meta = vyna_dcm::FileMeta::new(
            "1.2.840.10008.5.1.4.1.1.2",
            "1.9.8.7",
            TransferSyntax::ExplicitVrLittleEndian,
        );
        let mut ds = vyna_dcm::DataSet::new();
        ds.put_uid(vyna_dcm::tags::SOP_INSTANCE_UID, "1.9.8.7".to_string());
        let bytes = vyna_dcm::write_part10(&meta, &ds).unwrap();
        let dir = std::env::temp_dir().join(format!("stgcmtscu-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.dcm");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            uids_from_file(&path).unwrap(),
            (
                "1.2.840.10008.5.1.4.1.1.2".to_string(),
                "1.9.8.7".to_string()
            )
        );
        std::fs::remove_file(&path).unwrap();
    }
}
