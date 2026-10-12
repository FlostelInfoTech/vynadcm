//! `echoscp`: answer C-ECHO (Verification SCP).

#[path = "common.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use common::{ListenArgs, anyhowish::Result};
use tokio::net::TcpListener;
use vyna_dcm::net::AeTitle;
use vyna_dcm::net::service::{AcceptorOptions, Handlers, ScpOptions, accept};

/// C-ECHO SCP.
#[derive(Parser, Debug)]
#[command(name = "echoscp", about = "Answer C-ECHO requests until stopped")]
struct Args {
    #[command(flatten)]
    listen: ListenArgs,
}

struct EchoScp;
impl Handlers for EchoScp {}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let listener = TcpListener::bind(&args.listen.bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("echoscp listening on {}", args.listen.bind);

    let tls = match (&args.listen.tls_cert, &args.listen.tls_key) {
        (Some(c), Some(k)) => Some(Arc::new(
            vyna_dcm::net::tls::TlsAcceptor::new(
                &std::fs::read(c).map_err(|e| e.to_string())?,
                &std::fs::read(k).map_err(|e| e.to_string())?,
                args.listen
                    .tls_client_ca
                    .as_ref()
                    .map(std::fs::read)
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .as_deref(),
            )
            .map_err(|e| e.to_string())?,
        )),
        (None, None) => None,
        _ => {
            return Err(common::anyhowish::Error::msg(
                "--tls-cert and --tls-key go together",
            ));
        }
    };

    let acceptor = AcceptorOptions {
        ae_title: AeTitle::new(args.listen.ae_title.clone()).map_err(|e| e.to_string())?,
        association: vyna_dcm::net::AssociationOptions {
            max_pdu_len: args.listen.max_pdu,
            read_timeout: Duration::from_secs(args.listen.timeout_secs),
            ..Default::default()
        },
        check_called_ae: args.listen.strict_ae,
        ..Default::default()
    };

    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        // DIMSE is small-write chatty; Nagle would stall each PDU on the
        // peer's delayed ACK (~40ms).
        tcp.set_nodelay(true).ok();
        let (acceptor, tls) = (acceptor.clone(), tls.clone());
        tokio::spawn(async move {
            println!("association from {peer}");
            let out = match tls {
                Some(tls) => match tls.accept(tcp).await {
                    Ok(stream) => accept(stream, &acceptor, &EchoScp, &ScpOptions::default()).await,
                    Err(e) => Err(e),
                },
                None => accept(tcp, &acceptor, &EchoScp, &ScpOptions::default()).await,
            };
            match out {
                Ok(()) => println!("association from {peer} released"),
                Err(e) => println!("association from {peer} ended: {e}"),
            }
        });
    }
}
