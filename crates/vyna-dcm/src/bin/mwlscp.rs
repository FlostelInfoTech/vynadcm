//! `mwlscp`: Modality Worklist SCP (C-FIND) over a directory.
//!
//! Indexes worklist Part-10 files at startup and serves MWL queries
//! (top-level keys plus Scheduled Procedure Step sequence matching).

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
use vyna_dcm::net::qrdb::{MwlScp, QrIndex};
use vyna_dcm::net::service::{AcceptorOptions, Handlers, RequestContext, ScpOptions, accept};
use vyna_dcm::{DataSet, tags};

/// Modality Worklist SCP.
#[derive(Parser, Debug)]
#[command(name = "mwlscp", about = "Serve MWL C-FIND from a directory")]
struct Args {
    #[command(flatten)]
    listen: ListenArgs,
    /// Directory of worklist Part-10 files to index (recursive).
    directory: PathBuf,
    #[command(flatten)]
    audit: AuditArgs,
}

struct MwlScpServer {
    inner: MwlScp,
    audit: Option<SenderWithId>,
    our_ip: String,
}

impl Handlers for MwlScpServer {
    async fn on_find(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, Vec<DataSet>) {
        let (status, matches) = self.inner.on_find(ctx, query).await;
        println!(
            "mwl find from {}: {} match(es), status {status:#06X}",
            ctx.association.peer_ae.as_str(),
            matches.len()
        );
        if let Some(audit) = &self.audit {
            let mut msg = vyna_dcm::AuditMessage::query(
                query.string(tags::PATIENT_ID),
                None,
                ctx.association.peer_ae.as_str(),
                None,
                ctx.association.local_ae.as_str(),
                Some(&self.our_ip),
                &audit.source,
            );
            msg.outcome = common::outcome_for_status(status);
            emit_best_effort(Some(&audit.sender), &msg, "mwlscp");
        }
        (status, matches)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let audit = args.audit.sender(&args.listen.ae_title)?;
    let our_ip = host_of(&args.listen.bind).to_string();
    let index = QrIndex::scan(&args.directory).map_err(|e| e.to_string())?;
    println!(
        "mwlscp indexed {} worklist item(s) ({} skipped) from {}",
        index.records().len(),
        index.skipped,
        args.directory.display()
    );
    let listener = TcpListener::bind(&args.listen.bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("mwlscp listening on {}", args.listen.bind);
    if let Some(audit) = &audit {
        let msg = vyna_dcm::AuditMessage::actor_activity(
            &args.listen.ae_title,
            Some(&our_ip),
            &audit.source,
            true,
        );
        emit_best_effort(Some(&audit.sender), &msg, "mwlscp");
    }

    let tls = args.listen.tls_acceptor()?;

    let mut acceptor = AcceptorOptions::default();
    acceptor.ae_title = AeTitle::new(args.listen.ae_title.clone()).map_err(|e| e.to_string())?;
    acceptor.association = args.listen.assoc_options();
    acceptor.check_called_ae = args.listen.strict_ae;
    let handlers = Arc::new(MwlScpServer {
        inner: MwlScp::new(index),
        audit,
        our_ip,
    });

    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_SCP_ASSOCIATIONS));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            // Back off: a persistent accept failure (e.g. EMFILE)
            // must not spin the loop at full speed.
            eprintln!("mwlscp: accept failed (retrying in 100ms)");
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            eprintln!(
                "mwlscp: association from {peer} dropped: over {MAX_SCP_ASSOCIATIONS} concurrent"
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
            "mwlscp",
            "/tmp/mwl",
            "--audit-server",
            "udp://127.0.0.1:514",
        ])
        .unwrap();
        assert!(args.audit.sender("VYNA_SCP").unwrap().is_some());
    }
}
