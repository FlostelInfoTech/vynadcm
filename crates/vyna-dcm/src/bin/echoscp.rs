//! `echoscp`: answer C-ECHO (Verification SCP).

#[path = "common.rs"]
mod common;

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

    let tls = args.listen.tls_acceptor()?;

    let mut acceptor = AcceptorOptions::default();
    acceptor.ae_title = AeTitle::new(args.listen.ae_title.clone()).map_err(|e| e.to_string())?;
    acceptor.association = args.listen.assoc_options();
    acceptor.check_called_ae = args.listen.strict_ae;

    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            // Back off: a persistent accept failure (e.g. EMFILE)
            // must not spin the loop at full speed.
            eprintln!("echoscp: accept failed (retrying in 100ms)");
            tokio::time::sleep(Duration::from_millis(100)).await;
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
