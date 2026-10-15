//! `getscu`: C-GET retrieve into Part-10 files over the same association.

#[path = "common.rs"]
mod common;
#[path = "iocommon.rs"]
mod iocommon;

use std::path::PathBuf;

use clap::Parser;
use common::{
    AuditArgs, ConnArgs, anyhowish::Result, build_qr_query, emit_best_effort, host_of,
    outcome_for_status, relational_proposal, safe_instance_filename,
};
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
    /// Propose relational-queries extended negotiation (cross-level
    /// keys without unique parents).
    #[arg(long, default_value_t = false)]
    relational: bool,
    #[command(flatten)]
    audit: AuditArgs,
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

/// Build a per-arrival audit message (`None` when auditing is off
/// or the arrival carries no Study UID). Messages queue in the
/// store closure and flush after retrieval: emission is async and
/// the closure is sync.
#[allow(clippy::too_many_arguments)]
fn audit_arrival_msg(
    audit: &Option<common::SenderWithId>,
    patient: Option<&str>,
    study: Option<&str>,
    class: &str,
    inst: &str,
    conn: &ConnArgs,
    peer_ip: &str,
    outcome: vyna_dcm::Outcome,
) -> Option<vyna_dcm::AuditMessage> {
    if let (Some(audit), Some(study)) = (audit, study) {
        let mut msg = vyna_dcm::AuditMessage::instances_transferred(
            patient,
            study,
            &[(class, inst)],
            &conn.called,
            Some(peer_ip),
            &conn.calling,
            None,
            &audit.source,
        );
        msg.outcome = outcome;
        Some(msg)
    } else {
        None
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
    // Relational is proposed for the Q/R SOP only, never storage.
    let extended = relational_proposal(args.relational, sop);
    let mut assoc = args
        .conn
        .connect(&sops, &ConnArgs::default_syntaxes(), extended)
        .await?;

    let mut count = 0u32;
    let dir = &args.output_dir;
    let audit = args.audit.sender(&args.conn.calling)?;
    let peer_ip = host_of(&args.conn.connect).to_string();
    let mut pending: Vec<vyna_dcm::AuditMessage> = Vec::new();
    let r = assoc
        .get_with(sop, &query, 0, |class: String, inst: String, ds, ts| {
            let patient = ds.string(vyna_dcm::tags::PATIENT_ID).map(str::to_string);
            let study = ds
                .string(vyna_dcm::tags::STUDY_INSTANCE_UID)
                .map(str::to_string);
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
                        pending.extend(audit_arrival_msg(
                            &audit,
                            patient.as_deref(),
                            study.as_deref(),
                            &class,
                            &inst,
                            &args.conn,
                            &peer_ip,
                            vyna_dcm::Outcome::MajorFailure,
                        ));
                        return 0xA900; // dataset does not match SOP class
                    }
                }
            } else {
                ds
            };
            let meta = FileMeta::new(&class, &inst, TransferSyntax::ExplicitVrLittleEndian);
            let path = dir.join(format!("{}.dcm", safe_instance_filename(&inst)));
            let status = match vyna_dcm::write_part10(&meta, &ds) {
                Ok(bytes) => match iocommon::write_atomic(&path, &bytes) {
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
            };
            pending.extend(audit_arrival_msg(
                &audit,
                patient.as_deref(),
                study.as_deref(),
                &class,
                &inst,
                &args.conn,
                &peer_ip,
                outcome_for_status(status),
            ));
            status
        })
        .await?;
    for msg in &pending {
        emit_best_effort(audit.as_ref().map(|a| &a.sender), msg, "getscu").await;
    }
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

    #[test]
    fn audit_flags_parse() {
        let args = Args::try_parse_from([
            "getscu",
            "--connect",
            "127.0.0.1:11112",
            "--audit-server",
            "udp://127.0.0.1:514",
        ])
        .unwrap();
        assert!(args.audit.sender("VYNA_SCU").unwrap().is_some());
    }
}
