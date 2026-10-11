//! `storescu`: C-STORE Part-10 files.

#[path = "common.rs"]
mod common;

use std::path::PathBuf;

use clap::Parser;
use common::{ConnArgs, anyhowish::Result};
use vyna_dcm::{TransferSyntax, read_part10, tags};

/// C-STORE SCU.
#[derive(Parser, Debug)]
#[command(name = "storescu", about = "C-STORE Part-10 files to a peer")]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
    /// Part-10 files to send.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// DIMSE priority (0 medium, 1 high, 2 low).
    #[arg(long, default_value_t = 0)]
    priority: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // Read everything first so one set of presentation contexts covers it.
    let mut jobs: Vec<(PathBuf, String, String, vyna_dcm::DataSet)> = Vec::new();
    let mut classes: Vec<String> = Vec::new();
    let mut syntaxes: Vec<TransferSyntax> = ConnArgs::default_syntaxes();
    for file in &args.files {
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let (meta, ds) = read_part10(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        let ts = meta.transfer_syntax().map_err(|e| e.to_string())?;
        if !syntaxes.contains(&ts) {
            syntaxes.push(ts);
        }
        let class = ds
            .string(tags::SOP_CLASS_UID)
            .ok_or(format!("{}: no SOP Class UID", file.display()))?
            .to_string();
        let inst = ds
            .string(tags::SOP_INSTANCE_UID)
            .ok_or(format!("{}: no SOP Instance UID", file.display()))?
            .to_string();
        if !classes.contains(&class) {
            classes.push(class.clone());
        }
        jobs.push((file.clone(), class, inst, ds));
    }

    let class_refs: Vec<&str> = classes.iter().map(String::as_str).collect();
    let mut assoc = args.conn.connect(&class_refs, &syntaxes).await?;
    let mut failed = 0;
    for (file, class, inst, ds) in &jobs {
        // The association re-encodes into the negotiated transfer syntax;
        // compressed pixel data without a matching context fails loudly.
        match assoc.store(ds, class, inst, args.priority).await {
            Ok(status) => {
                println!("{}: C-STORE-RSP {status:#06X}", file.display());
                if status != 0 {
                    failed += 1;
                }
            }
            Err(e) => {
                println!("{}: C-STORE failed: {e}", file.display());
                failed += 1;
            }
        }
    }
    assoc.release().await.map_err(|e| e.to_string())?;
    if failed > 0 {
        return Err(common::anyhowish::Error::msg(format!(
            "{failed} store(s) failed"
        )));
    }
    Ok(())
}
