//! `mppsscp`: MPPS SCP (N-CREATE/N-SET) with file-backed steps.
//!
//! Steps persist as Part-10 files named by MPPS UID; N-SET merges
//! over the stored step, and final states reject further updates
//! (see `vyna_dcm::net::worklist::MppsScp`).

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
use vyna_dcm::net::AeTitle;
use vyna_dcm::net::service::{AcceptorOptions, Handlers, RequestContext, ScpOptions, accept};
use vyna_dcm::net::worklist::MppsScp;
use vyna_dcm::{DataSet, tags};

/// MPPS SCP.
#[derive(Parser, Debug)]
#[command(
    name = "mppsscp",
    about = "Receive MPPS N-CREATE/N-SET into a directory"
)]
struct Args {
    #[command(flatten)]
    listen: ListenArgs,
    /// Directory for MPPS step files (created; existing steps reload).
    directory: PathBuf,
    #[command(flatten)]
    audit: AuditArgs,
}

/// Study UID for audits: top-level, else the scheduled-step item.
fn mpps_study_uid(ds: &DataSet) -> Option<&str> {
    if let Some(u) = ds.string(tags::STUDY_INSTANCE_UID) {
        return Some(u);
    }
    ds.sequence(tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE)?
        .first()?
        .string(tags::STUDY_INSTANCE_UID)
}

struct MppsScpServer {
    inner: MppsScp,
    audit: Option<SenderWithId>,
    our_ip: String,
}

impl MppsScpServer {
    async fn audit_mpps(
        &self,
        ctx: &RequestContext<'_>,
        uid: &str,
        status: u16,
        ds: Option<&DataSet>,
    ) {
        let Some(audit) = &self.audit else {
            return;
        };
        let study = ds.and_then(mpps_study_uid);
        match study {
            Some(study) => {
                let mut msg = vyna_dcm::AuditMessage::begin_transferring(
                    None,
                    study,
                    &[],
                    ctx.association.peer_ae.as_str(),
                    None,
                    ctx.association.local_ae.as_str(),
                    Some(&self.our_ip),
                    &audit.source,
                );
                msg.outcome = common::outcome_for_status(status);
                emit_best_effort(Some(&audit.sender), &msg, "mppsscp").await;
            }
            None => eprintln!("mppsscp: skipping audit for {uid}: no Study UID"),
        }
    }
}

impl Handlers for MppsScpServer {
    async fn on_n_create(
        &self,
        ctx: &RequestContext<'_>,
        dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>) {
        let (status, rsp) = self.inner.on_n_create(ctx, dataset.clone()).await;
        let uid = ctx
            .command
            .affected_sop_instance_uid
            .as_deref()
            .unwrap_or("?");
        println!(
            "mpps N-CREATE {uid} from {}: {status:#06X}",
            ctx.association.peer_ae.as_str()
        );
        self.audit_mpps(ctx, uid, status, dataset.as_ref()).await;
        (status, rsp)
    }

    async fn on_n_set(
        &self,
        ctx: &RequestContext<'_>,
        dataset: Option<DataSet>,
    ) -> (u16, Option<DataSet>) {
        let (status, rsp) = self.inner.on_n_set(ctx, dataset.clone()).await;
        let uid = ctx
            .command
            .affected_sop_instance_uid
            .as_deref()
            .unwrap_or("?");
        println!(
            "mpps N-SET {uid} from {}: {status:#06X}",
            ctx.association.peer_ae.as_str()
        );
        // Audit the merged step (carries the scheduled-step linkage).
        let merged = self.inner.get(uid);
        self.audit_mpps(ctx, uid, status, merged.as_ref()).await;
        (status, rsp)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let audit = args.audit.sender(&args.listen.ae_title)?;
    let our_ip = host_of(&args.listen.bind).to_string();
    std::fs::create_dir_all(&args.directory).map_err(|e| e.to_string())?;
    let (inner, skipped) = MppsScp::load(&args.directory);
    println!(
        "mppsscp loaded {} step(s) ({} skipped) from {}",
        inner.len(),
        skipped,
        args.directory.display()
    );
    let listener = TcpListener::bind(&args.listen.bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("mppsscp listening on {}", args.listen.bind);
    if let Some(audit) = &audit {
        let msg = vyna_dcm::AuditMessage::actor_activity(
            &args.listen.ae_title,
            Some(&our_ip),
            &audit.source,
            true,
        );
        emit_best_effort(Some(&audit.sender), &msg, "mppsscp").await;
    }

    let tls = args.listen.tls_acceptor()?;

    let mut acceptor = AcceptorOptions::default();
    acceptor.ae_title = AeTitle::new(args.listen.ae_title.clone()).map_err(|e| e.to_string())?;
    acceptor.association = args.listen.assoc_options();
    acceptor.check_called_ae = args.listen.strict_ae;
    let handlers = Arc::new(MppsScpServer {
        inner,
        audit,
        our_ip,
    });

    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_SCP_ASSOCIATIONS));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            // Back off: a persistent accept failure (e.g. EMFILE)
            // must not spin the loop at full speed.
            eprintln!("mppsscp: accept failed (retrying in 100ms)");
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            eprintln!(
                "mppsscp: association from {peer} dropped: over {MAX_SCP_ASSOCIATIONS} concurrent"
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
    fn study_uid_prefers_top_level() {
        let mut ds = DataSet::new();
        assert_eq!(mpps_study_uid(&ds), None);
        let mut item = DataSet::new();
        item.put_uid(tags::STUDY_INSTANCE_UID, "1.2.9");
        ds.put_sequence(tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE, vec![item]);
        assert_eq!(mpps_study_uid(&ds), Some("1.2.9"));
        ds.put_uid(tags::STUDY_INSTANCE_UID, "1.2.3");
        assert_eq!(mpps_study_uid(&ds), Some("1.2.3"));
    }

    #[test]
    fn audit_flags_parse() {
        let args = Args::try_parse_from([
            "mppsscp",
            "/tmp/mpps",
            "--audit-server",
            "udp://127.0.0.1:514",
        ])
        .unwrap();
        assert!(args.audit.sender("VYNA_SCP").unwrap().is_some());
    }
}
