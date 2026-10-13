//! VynaLink pattern: a C-STORE SCP that forwards every instance to a
//! downstream PACS (receive → store → forward with its own association).
//!
//! Run: `cargo run -p vyna-dcm --example link_route -- <bind> <dest-host:port> <dest-ae>`
//! Example: `cargo run -p vyna-dcm --example link_route -- 127.0.0.1:11112 127.0.0.1:11113 PACS`

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use vyna_dcm::net::service::{
    AcceptorOptions, Handlers, RequestContext, ScpOptions, accept, propose,
};
use vyna_dcm::net::{AeTitle, Association, AssociationOptions};
use vyna_dcm::{DataSet, TransferSyntax, tags};

struct Forwarder {
    dest_addr: String,
    dest_ae: AeTitle,
    calling_ae: AeTitle,
    opts: AssociationOptions,
}

impl Handlers for Forwarder {
    async fn on_store(&self, ctx: &RequestContext<'_>, dataset: DataSet) -> u16 {
        let class = ctx
            .command
            .affected_sop_class_uid
            .clone()
            .unwrap_or_default();
        let inst = ctx
            .command
            .affected_sop_instance_uid
            .clone()
            .unwrap_or_default();
        println!("route: received {inst}");

        // Open a fresh association per instance (simple + robust; a
        // production VynaLink route would pool these).
        let mut fwd = match Association::request(
            &self.opts,
            &self.calling_ae,
            &self.dest_ae,
            &self.dest_addr,
            propose(
                &[&class],
                &[
                    TransferSyntax::ExplicitVrLittleEndian,
                    TransferSyntax::ImplicitVrLittleEndian,
                ],
            )
            .unwrap(),
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                eprintln!("route: forward association failed: {e}");
                return 0xA700; // out of resources
            }
        };
        let status = fwd
            .store(&dataset, &class, &inst, 0)
            .await
            .unwrap_or(0x0110);
        fwd.release().await.ok();
        println!(
            "route: forwarded {inst} -> {} ({status:#06X})",
            self.dest_addr
        );
        // Report the downstream status upstream (or map to success once
        // durably queued — VynaLink policy decision).
        status
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: link_route <bind> <dest-host:port> <dest-ae>".into());
    }
    let mut fwd_opts = AssociationOptions::default();
    fwd_opts.read_timeout = Duration::from_secs(60);
    let handlers = Arc::new(Forwarder {
        dest_addr: args[2].clone(),
        dest_ae: AeTitle::new(args[3].clone()).map_err(|e| e.to_string())?,
        calling_ae: AeTitle::new("VYNALINK").map_err(|e| e.to_string())?,
        opts: fwd_opts,
    });
    let _ = tags::SOP_INSTANCE_UID; // (kept for reader orientation)
    let acceptor = AcceptorOptions::default();
    let listener = TcpListener::bind(&args[1])
        .await
        .map_err(|e| e.to_string())?;
    println!("link_route: {} -> {} ({})", args[1], args[2], args[3]);
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            eprintln!("link_route: accept failed (retrying in 100ms)");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        };
        let (acceptor, handlers) = (acceptor.clone(), handlers.clone());
        tokio::spawn(async move {
            match accept(tcp, &acceptor, &*handlers, &ScpOptions::default()).await {
                Ok(()) => println!("link_route: {peer} released"),
                Err(e) => println!("link_route: {peer} ended: {e}"),
            }
        });
    }
}
