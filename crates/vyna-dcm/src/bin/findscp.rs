//! `findscp`: Query/Retrieve SCP (C-FIND/C-MOVE/C-GET) over a directory.
//!
//! Indexes Part-10 files at startup and serves Patient Root, Study Root,
//! and Patient/Study Only queries. C-MOVE needs `--route` entries mapping
//! destination AE titles to `host:port`.

#[path = "common.rs"]
mod common;

use std::collections::HashMap;
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
use vyna_dcm::net::qrdb::{QrIndex, QrScp};
use vyna_dcm::net::service::{
    AcceptorOptions, Handlers, HashMapDirectory, RequestContext, RetrievePlan, ScpMoveConfig,
    ScpOptions, accept,
};
use vyna_dcm::{DataSet, tags};

/// Query/Retrieve SCP.
#[derive(Parser, Debug)]
#[command(name = "findscp", about = "Serve C-FIND/C-MOVE/C-GET from a directory")]
struct Args {
    #[command(flatten)]
    listen: ListenArgs,
    /// Directory of Part-10 files to index (recursive).
    directory: PathBuf,
    /// C-MOVE route as AE=host:port. Repeatable; without routes,
    /// C-MOVE fails cleanly (C-FIND/C-GET still work).
    #[arg(long = "route")]
    routes: Vec<String>,
    #[command(flatten)]
    audit: AuditArgs,
}

struct FindScp {
    inner: QrScp,
    audit: Option<SenderWithId>,
    our_ip: String,
}

impl Handlers for FindScp {
    async fn on_find(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, Vec<DataSet>) {
        let (status, matches) = self.inner.on_find(ctx, query).await;
        println!(
            "find from {}: {} match(es), status {status:#06X}",
            ctx.association.peer_ae.as_str(),
            matches.len()
        );
        if let Some(audit) = &self.audit {
            let mut msg = vyna_dcm::AuditMessage::query(
                query.string(tags::PATIENT_ID),
                query.string(tags::STUDY_INSTANCE_UID),
                ctx.association.peer_ae.as_str(),
                None,
                ctx.association.local_ae.as_str(),
                Some(&self.our_ip),
                &audit.source,
            );
            msg.outcome = common::outcome_for_status(status);
            emit_best_effort(Some(&audit.sender), &msg, "findscp").await;
        }
        (status, matches)
    }

    async fn on_move_plan(
        &self,
        ctx: &RequestContext<'_>,
        query: &DataSet,
        destination: &str,
    ) -> (u16, RetrievePlan) {
        // Delivery itself is audited at the receiving storescp.
        self.inner.on_move_plan(ctx, query, destination).await
    }

    async fn on_get_plan(&self, ctx: &RequestContext<'_>, query: &DataSet) -> (u16, RetrievePlan) {
        self.inner.on_get_plan(ctx, query).await
    }
}

fn parse_routes(routes: &[String]) -> Result<HashMap<String, (String, u16)>> {
    let mut map = HashMap::new();
    for r in routes {
        let Some((ae, addr)) = r.split_once('=') else {
            return Err(common::anyhowish::Error::msg(format!(
                "bad --route {r:?}, want AE=host:port"
            )));
        };
        let Some((host, port)) = addr.rsplit_once(':') else {
            return Err(common::anyhowish::Error::msg(format!(
                "bad --route {r:?}, want AE=host:port"
            )));
        };
        let port: u16 = port
            .parse()
            .map_err(|_| common::anyhowish::Error::msg(format!("bad port in --route {r:?}")))?;
        map.insert(ae.trim().to_string(), (host.to_string(), port));
    }
    Ok(map)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let audit = args.audit.sender(&args.listen.ae_title)?;
    let our_ip = host_of(&args.listen.bind).to_string();
    let index = QrIndex::scan(&args.directory).map_err(|e| e.to_string())?;
    println!(
        "findscp indexed {} file(s) ({} skipped) from {}",
        index.records().len(),
        index.skipped,
        args.directory.display()
    );
    let listener = TcpListener::bind(&args.listen.bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("findscp listening on {}", args.listen.bind);
    if let Some(audit) = &audit {
        let msg = vyna_dcm::AuditMessage::actor_activity(
            &args.listen.ae_title,
            Some(&our_ip),
            &audit.source,
            true,
        );
        emit_best_effort(Some(&audit.sender), &msg, "findscp").await;
    }

    let tls = args.listen.tls_acceptor()?;

    let mut acceptor = AcceptorOptions::default();
    acceptor.ae_title = AeTitle::new(args.listen.ae_title.clone()).map_err(|e| e.to_string())?;
    acceptor.association = args.listen.assoc_options();
    acceptor.check_called_ae = args.listen.strict_ae;
    let handlers = Arc::new(FindScp {
        inner: QrScp::new(index),
        audit,
        our_ip,
    });
    let mut scp_opts = ScpOptions::default();
    scp_opts.move_config = if args.routes.is_empty() {
        None
    } else {
        Some(ScpMoveConfig {
            directory: Arc::new(HashMapDirectory {
                map: parse_routes(&args.routes)?,
            }),
            assoc_options: args.listen.assoc_options(),
            calling_ae: AeTitle::new(args.listen.ae_title.clone()).map_err(|e| e.to_string())?,
        })
    };

    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_SCP_ASSOCIATIONS));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            // Back off: a persistent accept failure (e.g. EMFILE)
            // must not spin the loop at full speed.
            eprintln!("findscp: accept failed (retrying in 100ms)");
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            eprintln!(
                "findscp: association from {peer} dropped: over {MAX_SCP_ASSOCIATIONS} concurrent"
            );
            continue;
        };
        // DIMSE is small-write chatty; Nagle would stall each PDU on the
        // peer's delayed ACK (~40ms).
        tcp.set_nodelay(true).ok();
        let (acceptor, tls, handlers, scp_opts) = (
            acceptor.clone(),
            tls.clone(),
            handlers.clone(),
            scp_opts.clone(),
        );
        tokio::spawn(async move {
            let _permit = permit;
            println!("association from {peer}");
            let out = match tls {
                Some(tls) => match tls.accept(tcp).await {
                    Ok(stream) => accept(stream, &acceptor, &*handlers, &scp_opts).await,
                    Err(e) => Err(e),
                },
                None => accept(tcp, &acceptor, &*handlers, &scp_opts).await,
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
    fn routes_parse() {
        let map = parse_routes(&["DEST=127.0.0.1:11113".to_string()]).unwrap();
        assert_eq!(map.get("DEST"), Some(&("127.0.0.1".to_string(), 11113)));
        assert!(parse_routes(&["no-equals".to_string()]).is_err());
        assert!(parse_routes(&["AE=host".to_string()]).is_err());
        assert!(parse_routes(&["AE=host:xx".to_string()]).is_err());
    }

    #[test]
    fn audit_flags_parse() {
        let args = Args::try_parse_from([
            "findscp",
            "/tmp/qr",
            "--audit-server",
            "udp://127.0.0.1:514",
        ])
        .unwrap();
        assert!(args.audit.sender("VYNA_SCP").unwrap().is_some());
    }
}
