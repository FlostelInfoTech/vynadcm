//! `wadorsd`: serve a DICOMweb store (QIDO/WADO/STOW) from a directory.
//!
//! Seeds an in-memory store from `*.dcm` files; STOW uploads land in
//! memory too (a stub archive, not durable storage).

#[path = "webcommon.rs"]
mod webcommon;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use vyna_dcm::web::hyper_server::{ServerAuth, serve_forever_with_auth};
use vyna_dcm::web::server::{DicomWebStore, MemoryStore};

/// DICOMweb stub server.
///
/// Plain HTTP without auth by default: keep the loopback bind or put
/// a TLS-terminating proxy in front (PHI on the wire otherwise).
#[derive(Parser, Debug)]
#[command(name = "wadorsd", about = "Serve QIDO/WADO/STOW from a directory")]
struct Args {
    /// Bind address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,
    /// Directory of Part-10 files to serve (recursive).
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    /// Public base URL for BulkDataURIs (defaults to `http://{bind}/`).
    #[arg(long)]
    public_base: Option<String>,
    /// Require `Authorization: Bearer <token>` on every request.
    #[arg(long)]
    bearer_token: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match run(&args).await {
        Ok(()) => {}
        Err(e) => {
            eprintln!("wadorsd: {e}");
            std::process::exit(1);
        }
    }
}

async fn run(args: &Args) -> Result<(), String> {
    let addr: std::net::SocketAddr = args
        .bind
        .parse()
        .map_err(|_| format!("bad --bind {:?}", args.bind))?;
    let store = Arc::new(MemoryStore::new());
    let mut skipped = 0usize;
    for file in webcommon::collect_dcm_files(&args.dir) {
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        match store.store_instance(&bytes, None) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("wadorsd: skipping {}: {e}", file.display());
                skipped += 1;
            }
        }
    }
    println!(
        "wadorsd serving {} instance(s) ({} skipped) from {} on http://{}/",
        store.len(),
        skipped,
        args.dir.display(),
        addr
    );
    let base = args
        .public_base
        .clone()
        .unwrap_or_else(|| format!("http://{addr}/"));
    let auth = ServerAuth {
        bearer_token: args.bearer_token.clone(),
    };
    serve_forever_with_auth(store, base, addr, auth)
        .await
        .map_err(|e| e.to_string())
}
