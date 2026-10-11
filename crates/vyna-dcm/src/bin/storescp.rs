//! `storescp`: receive C-STORE into Part-10 files.

#[path = "common.rs"]
mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use common::{ListenArgs, anyhowish::Result};
use tokio::net::TcpListener;
use vyna_dcm::net::AeTitle;
use vyna_dcm::net::service::{AcceptorOptions, Handlers, RequestContext, ScpOptions, accept};
use vyna_dcm::{DataSet, FileMeta, tags};

/// C-STORE SCP.
#[derive(Parser, Debug)]
#[command(name = "storescp", about = "Receive C-STORE into a directory")]
struct Args {
    #[command(flatten)]
    listen: ListenArgs,
    /// Output directory for received instances.
    #[arg(long, default_value = ".")]
    directory: PathBuf,
}

struct StoreScp {
    directory: PathBuf,
}

impl Handlers for StoreScp {
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
        let ts = ctx.context.transfer_syntax;
        let meta = FileMeta::new(&class, &inst, ts);
        match vyna_dcm::write_part10(&meta, &dataset) {
            Ok(bytes) => {
                let safe: String = inst
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '.' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                let path = self.directory.join(format!("{safe}.dcm"));
                match std::fs::write(&path, bytes) {
                    Ok(()) => {
                        let patient = dataset.string(tags::PATIENT_NAME).unwrap_or("?");
                        println!("stored {} ({patient}) -> {}", inst, path.display());
                        0x0000
                    }
                    Err(e) => {
                        println!("write failed for {inst}: {e}");
                        0xA700 // out of resources
                    }
                }
            }
            Err(e) => {
                println!("encode failed for {inst}: {e}");
                0xA900 // dataset does not match SOP class
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.directory).map_err(|e| e.to_string())?;
    let listener = TcpListener::bind(&args.listen.bind)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "storescp listening on {} -> {}",
        args.listen.bind,
        args.directory.display()
    );

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
    let handlers = Arc::new(StoreScp {
        directory: args.directory.clone(),
    });

    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        let (acceptor, tls, handlers) = (acceptor.clone(), tls.clone(), handlers.clone());
        tokio::spawn(async move {
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
