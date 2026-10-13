//! `storescu`: C-STORE Part-10 files.

#[path = "common.rs"]
mod common;

use std::path::PathBuf;

use clap::Parser;
use common::{
    AuditArgs, ConnArgs, anyhowish::Result, emit_best_effort, host_of, outcome_for_status,
};
use vyna_dcm::{TransferSyntax, read_part10, tags};

/// C-STORE SCU.
#[derive(Parser, Debug)]
#[command(name = "storescu", about = "C-STORE Part-10 files to a peer")]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
    /// Part-10 files to send.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// DIMSE priority (0 medium, 1 high, 2 low).
    #[arg(long, default_value_t = 0, value_parser = parse_priority)]
    priority: u16,
    #[command(flatten)]
    audit: AuditArgs,
}

/// Clap parser: DIMSE priority must be 0, 1, or 2.
fn parse_priority(s: &str) -> std::result::Result<u16, String> {
    s.parse::<u16>()
        .map_err(|_| format!("priority must be 0, 1, or 2, got {s:?}"))
        .and_then(|p| {
            if p <= 2 {
                Ok(p)
            } else {
                Err(format!("priority must be 0, 1, or 2, got {p}"))
            }
        })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // Read everything first so one set of presentation contexts covers it.
    let mut jobs: Vec<(PathBuf, String, String, vyna_dcm::DataSet)> = Vec::new();
    let mut classes: Vec<String> = Vec::new();
    let mut syntaxes: Vec<TransferSyntax> = ConnArgs::default_syntaxes();
    for file in &args.files {
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let (meta, ds) = read_part10(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        let ts = meta.transfer_syntax().map_err(|e| e.to_string())?;
        if !syntaxes.contains(&ts) {
            syntaxes.push(ts);
        }
        let class = ds
            .string(tags::SOP_CLASS_UID)
            .ok_or(format!("{}: no SOP Class UID", file.display()))?
            .to_string();
        let inst = ds
            .string(tags::SOP_INSTANCE_UID)
            .ok_or(format!("{}: no SOP Instance UID", file.display()))?
            .to_string();
        if !classes.contains(&class) {
            classes.push(class.clone());
        }
        jobs.push((file.clone(), class, inst, ds));
    }

    let class_refs: Vec<&str> = classes.iter().map(String::as_str).collect();
    let audit = args.audit.sender(&args.conn.calling)?;
    let peer_ip = host_of(&args.conn.connect).to_string();
    let mut assoc = args.conn.connect(&class_refs, &syntaxes).await?;
    let mut failed = 0;
    for (file, class, inst, ds) in &jobs {
        // The association re-encodes into the negotiated transfer syntax;
        // compressed pixel data without a matching context fails loudly.
        let status = match assoc.store(ds, class, inst, args.priority).await {
            Ok(status) => {
                println!("{}: C-STORE-RSP {status:#06X}", file.display());
                if status != 0 {
                    failed += 1;
                }
                Some(status)
            }
            Err(e) => {
                println!("{}: C-STORE failed: {e}", file.display());
                failed += 1;
                None
            }
        };
        if let Some(audit) = &audit {
            match ds.string(tags::STUDY_INSTANCE_UID) {
                Some(study) => {
                    let mut msg = vyna_dcm::AuditMessage::instances_transferred(
                        ds.string(tags::PATIENT_ID),
                        study,
                        &[(class.as_str(), inst.as_str())],
                        &args.conn.calling,
                        None,
                        &args.conn.called,
                        Some(&peer_ip),
                        &audit.source,
                    );
                    msg.outcome = status
                        .map(outcome_for_status)
                        .unwrap_or(vyna_dcm::Outcome::MajorFailure);
                    emit_best_effort(Some(&audit.sender), &msg, "storescu");
                }
                None => eprintln!(
                    "storescu: skipping audit for {}: no Study UID",
                    file.display()
                ),
            }
        }
    }
    assoc.release().await.map_err(|e| e.to_string())?;
    if failed > 0 {
        return Err(common::anyhowish::Error::msg(format!(
            "{failed} store(s) failed"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_flags_parse() {
        let args = Args::try_parse_from([
            "storescu",
            "--connect",
            "127.0.0.1:11112",
            "--audit-server",
            "tcp://127.0.0.1:6514",
            "a.dcm",
        ])
        .unwrap();
        assert_eq!(
            args.audit.audit_server.as_deref(),
            Some("tcp://127.0.0.1:6514")
        );
        assert!(args.audit.sender("VYNA_SCU").unwrap().is_some());
    }
}
