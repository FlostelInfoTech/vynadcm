//! `echoscu`: C-ECHO the Verification SOP class.

#[path = "common.rs"]
mod common;

use clap::Parser;
use common::{ConnArgs, anyhowish::Result};
use vyna_dcm::core::sop_class;

/// C-ECHO SCU.
#[derive(Parser, Debug)]
#[command(name = "echoscu", about = "Send C-ECHO and print the status")]
struct Args {
    #[command(flatten)]
    conn: ConnArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut assoc = args
        .conn
        .connect(
            &[sop_class::VERIFICATION],
            &ConnArgs::default_syntaxes(),
            vec![],
        )
        .await?;
    let status = assoc.echo(sop_class::VERIFICATION).await?;
    println!("C-ECHO-RSP status {status:#06X}");
    assoc.release().await?;
    if status != 0 {
        return Err(common::anyhowish::Error::msg(format!(
            "C-ECHO failed: {status:#06X}"
        )));
    }
    Ok(())
}
