//! `movescu`: C-MOVE retrieve to a third-party destination AE.

#[path = "common.rs"]
mod common;

use clap::Parser;
use common::{
    AuditArgs, ConnArgs, anyhowish::Result, build_qr_query, emit_best_effort, host_of,
    outcome_for_status, relational_proposal,
};
use vyna_dcm::core::sop_class;

/// C-MOVE SCU.
#[derive(Parser, Debug)]
#[command(
    name = "movescu",
    about = "C-MOVE retrieve: the peer sends matches to --dest (run storescp there)"
)]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
    /// Query model: studyroot (default), patientroot, patientstudy.
    #[arg(long, default_value = "studyroot")]
    model: String,
    /// Move destination AE title (must be known to the peer).
    #[arg(long)]
    dest: String,
    /// Matching keys as KEY=value (keyword or GGGGEEEE tag). Repeatable.
    #[arg(short, long = "key")]
    keys: Vec<String>,
    /// Propose relational-queries extended negotiation (cross-level
    /// keys without unique parents).
    #[arg(long, default_value_t = false)]
    relational: bool,
    #[command(flatten)]
    audit: AuditArgs,
}

fn move_sop_for_model(model: &str) -> Result<&'static str> {
    match model.to_ascii_lowercase().as_str() {
        "studyroot" | "study" => Ok(sop_class::STUDY_ROOT_MOVE),
        "patientroot" | "patient" => Ok(sop_class::PATIENT_ROOT_MOVE),
        "patientstudy" => Ok(sop_class::PATIENT_STUDY_ONLY_MOVE),
        other => Err(common::anyhowish::Error::msg(format!(
            "unknown model {other:?}"
        ))),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let sop = move_sop_for_model(&args.model)?;
    let query = build_qr_query(&args.keys, false)?;

    let extended = relational_proposal(args.relational, sop);
    let mut assoc = args
        .conn
        .connect(&[sop], &ConnArgs::default_syntaxes(), extended)
        .await?;
    let r = assoc.move_to(sop, &query, &args.dest, 0).await?;
    println!(
        "C-MOVE complete: {} completed, {} failed, {} warning, status {:#06X}",
        r.completed, r.failed, r.warning, r.status
    );
    if let Some(audit) = args.audit.sender(&args.conn.calling)? {
        // C-MOVE responses carry counts, not instance UIDs, so the audit
        // names the query target; the transfer itself is peer -> --dest.
        let study = query
            .string(vyna_dcm::tags::STUDY_INSTANCE_UID)
            .unwrap_or("");
        let mut msg = vyna_dcm::AuditMessage::begin_transferring(
            query.string(vyna_dcm::tags::PATIENT_ID),
            study,
            &[],
            &args.conn.called,
            Some(host_of(&args.conn.connect)),
            &args.dest,
            None,
            &audit.source,
        );
        msg.outcome = if r.failed > 0 && r.status == 0 {
            vyna_dcm::Outcome::MinorFailure
        } else {
            outcome_for_status(r.status)
        };
        emit_best_effort(Some(&audit.sender), &msg, "movescu").await;
    }
    assoc.release().await?;
    if r.status != 0 {
        return Err(common::anyhowish::Error::msg(format!(
            "C-MOVE failed: {:#06X}",
            r.status
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_map_to_move_sops() {
        assert_eq!(
            move_sop_for_model("studyroot").unwrap(),
            sop_class::STUDY_ROOT_MOVE
        );
        assert_eq!(
            move_sop_for_model("patientroot").unwrap(),
            sop_class::PATIENT_ROOT_MOVE
        );
        assert_eq!(
            move_sop_for_model("patientstudy").unwrap(),
            sop_class::PATIENT_STUDY_ONLY_MOVE
        );
        assert!(move_sop_for_model("mwl").is_err());
    }

    #[test]
    fn audit_flags_parse() {
        let args = Args::try_parse_from([
            "movescu",
            "--connect",
            "127.0.0.1:11112",
            "--dest",
            "DEST_AE",
            "--audit-server",
            "udp://127.0.0.1:514",
        ])
        .unwrap();
        assert!(args.audit.sender("VYNA_SCU").unwrap().is_some());
    }
}
