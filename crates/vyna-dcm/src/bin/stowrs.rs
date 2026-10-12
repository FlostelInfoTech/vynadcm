//! `stowrs`: STOW-RS store Part-10 files.

#[path = "webcommon.rs"]
mod webcommon;

use std::path::PathBuf;

use clap::Parser;
use webcommon::WebArgs;

/// STOW-RS store tool.
#[derive(Parser, Debug)]
#[command(name = "stowrs", about = "STOW-RS store Part-10 files")]
struct Args {
    #[command(flatten)]
    web: WebArgs,
    /// Study Instance UID for `POST /studies/{uid}` (must match files).
    #[arg(long)]
    study: Option<String>,
    /// Part-10 files to store.
    files: Vec<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match run(&args).await {
        Ok(()) => {}
        Err(e) => {
            eprintln!("stowrs: {e}");
            std::process::exit(1);
        }
    }
}

async fn run(args: &Args) -> Result<(), String> {
    if args.files.is_empty() {
        return Err("need at least one file".to_string());
    }
    let client = args.web.client()?;
    let mut bodies = Vec::with_capacity(args.files.len());
    for file in &args.files {
        bodies.push(std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?);
    }
    let resp = client
        .stow(args.study.as_deref(), &bodies)
        .await
        .map_err(|e| e.to_string())?;
    println!("STOW complete: status {}", resp.status);
    if let Some(url) = resp.retrieve_url {
        println!("RetrieveURL: {url}");
    }
    Ok(())
}
