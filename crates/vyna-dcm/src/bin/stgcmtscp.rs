//! `stgcmtscp`: Storage Commitment (Push) SCP over a directory.
//!
//! Commits instances the directory index holds; anything else fails
//! with 0x0112. The verdict ships as an N-EVENT-REPORT on the same
//! association (see `vyna_dcm::net::qrdb::StgCommitScp`).

#[path = "common.rs"]
mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use common::{
    AuditArgs, ListenArgs, MAX_SCP_ASSOCIATIONS, SenderWithId, anyhowish::Result, emit_best_effort,
    host_of,
};
use tokio::net::TcpListener;
use vyna_dcm::DataSet;
use vyna_dcm::net::AeTitle;
use vyna_dcm::net::qrdb::{QrIndex, StgCommitScp};
use vyna_dcm::net::service::{
    AcceptorOptions, Handlers, RequestContext, ScpFollowUp, ScpOptions, accept,
};
use vyna_dcm::net::worklist::parse_commitment_result;

/// Storage Commitment SCP.
#[derive(Parser, Debug)]
#[command(
    name = "stgcmtscp",
    about = "Serve Storage Commitment N-ACTION from a directory"
)]
struct Args {
    #[command(flatten)]
    listen: ListenArgs,
    /// Directory of Part-10 files to commit from (recursive).
    directory: PathBuf,
    #[command(flatten)]
    audit: AuditArgs,
}

struct StgCommitScpServer {
    inner: StgCommitScp,
    audit: Option<SenderWithId>,
    our_ip: String,
}

impl StgCommitScpServer {
    fn audit_report(
        &self,
        ctx: &RequestContext<'_>,
        status: u16,
        report: &DataSet,
        audit: &SenderWithId,
    ) {
        let (_, ok, failed) = parse_commitment_result(report);
        let mut skipped = 0u32;
        for (list, failed_list) in [(&ok, false), (&failed, true)] {
            for (class, inst) in list {
                match self.inner.study_uid_for(inst) {
                    Some(study) => {
                        let mut msg = vyna_dcm::AuditMessage::instances_transferred(
                            None,
                            &study,
                            &[(class.as_str(), inst.as_str())],
                            ctx.association.peer_ae.as_str(),
                            None,
                            ctx.association.local_ae.as_str(),
                            Some(&self.our_ip),
                            &audit.source,
                        );
                        msg.outcome = if failed_list {
                            vyna_dcm::Outcome::MajorFailure
                        } else {
                            common::outcome_for_status(status)
                        };
                        emit_best_effort(Some(&audit.sender), &msg, "stgcmtscp");
                    }
                    None => skipped += 1,
                }
            }
        }
        if skipped > 0 {
            eprintln!("stgcmtscp: skipping audit for {skipped} instance(s): no Study UID");
        }
    }
}

impl Handlers for StgCommitScpServer {
    async fn on_n_action(
        &self,
        ctx: &RequestContext<'_>,
        dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>, Vec<ScpFollowUp>) {
        let (status, rsp, reports) = self.inner.on_n_action(ctx, dataset).await;
        let (mut ok_n, mut failed_n) = (0, 0);
        for f in &reports {
            if let ScpFollowUp::EventReport {
                dataset: Some(ds), ..
            } = f
            {
                let (_, ok, failed) = parse_commitment_result(ds);
                ok_n += ok.len();
                failed_n += failed.len();
                if let Some(audit) = &self.audit {
                    self.audit_report(ctx, status, ds, audit);
                }
            }
        }
        println!(
            "commitment from {}: {ok_n} committed, {failed_n} failed, status {status:#06X}",
            ctx.association.peer_ae.as_str()
        );
        (status, rsp, reports)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let audit = args.audit.sender(&args.listen.ae_title)?;
    let our_ip = host_of(&args.listen.bind).to_string();
    let index = QrIndex::scan(&args.directory).map_err(|e| e.to_string())?;
    println!(
        "stgcmtscp indexed {} instance(s) ({} skipped) from {}",
        index.records().len(),
        index.skipped,
        args.directory.display()
    );
    let listener = TcpListener::bind(&args.listen.bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("stgcmtscp listening on {}", args.listen.bind);
    if let Some(audit) = &audit {
        let msg = vyna_dcm::AuditMessage::actor_activity(
            &args.listen.ae_title,
            Some(&our_ip),
            &audit.source,
            true,
        );
        emit_best_effort(Some(&audit.sender), &msg, "stgcmtscp");
    }

    let tls = args.listen.tls_acceptor()?;

    let mut acceptor = AcceptorOptions::default();
    acceptor.ae_title = AeTitle::new(args.listen.ae_title.clone()).map_err(|e| e.to_string())?;
    acceptor.association = args.listen.assoc_options();
    acceptor.check_called_ae = args.listen.strict_ae;
    let handlers = Arc::new(StgCommitScpServer {
        inner: StgCommitScp::new(index),
        audit,
        our_ip,
    });

    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_SCP_ASSOCIATIONS));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            // Back off: a persistent accept failure (e.g. EMFILE)
            // must not spin the loop at full speed.
            eprintln!("stgcmtscp: accept failed (retrying in 100ms)");
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            eprintln!(
                "stgcmtscp: association from {peer} dropped: over {MAX_SCP_ASSOCIATIONS} concurrent"
            );
            continue;
        };
        // DIMSE is small-write chatty; Nagle would stall each PDU on the
        // peer's delayed ACK (~40ms).
        tcp.set_nodelay(true).ok();
        let (acceptor, tls, handlers) = (acceptor.clone(), tls.clone(), handlers.clone());
        tokio::spawn(async move {
            let _permit = permit;
            println!("association from {peer}");
            let out = match tls {
                Some(tls) => match tls.accept(tcp).await {
                    Ok(stream) => {
                        accept(stream, &acceptor, &*handlers, &ScpOptions::default()).await
                    }
                    Err(e) => Err(e),
                },
                None => accept(tcp, &acceptor, &*handlers, &ScpOptions::default()).await,
            };
            match out {
                Ok(()) => println!("association from {peer} released"),
                Err(e) => println!("association from {peer} ended: {e}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_flags_parse() {
        let args = Args::try_parse_from([
            "stgcmtscp",
            "/tmp/commit",
            "--audit-server",
            "udp://127.0.0.1:514",
        ])
        .unwrap();
        assert!(args.audit.sender("VYNA_SCP").unwrap().is_some());
    }
}
