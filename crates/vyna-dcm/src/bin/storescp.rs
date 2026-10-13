//! `storescp`: receive C-STORE into Part-10 files.

#[path = "common.rs"]
mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use common::{
    AuditArgs, ListenArgs, SenderWithId, anyhowish::Result, emit_best_effort, host_of,
    outcome_for_status, safe_instance_filename,
};
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
    /// Run a command after each successful store (best-effort: hook
    /// failures warn, never fail the C-STORE). Whitespace-split into
    /// argv without a shell; `{file}` `{class}` `{instance}`
    /// `{patient}` `{study}` `{caller}` `{called}` expand per
    /// argument (values with spaces stay in one argv element).
    #[arg(long)]
    exec: Option<String>,
    /// Log patient names with each store (PHI: off by default).
    #[arg(long)]
    verbose: bool,
    #[command(flatten)]
    audit: AuditArgs,
}

/// A parsed `--exec` template: argv with `{token}` placeholders.
struct ExecHook {
    argv: Vec<String>,
}

/// Tokens expanded in `--exec` argv.
const EXEC_TOKENS: &[&str] = &[
    "file", "class", "instance", "patient", "study", "caller", "called",
];

/// Parse an `--exec` template; unknown `{tokens}`, unbalanced
/// braces, and empty commands fail fast at startup.
fn parse_exec_hook(cmd: &str) -> std::result::Result<ExecHook, String> {
    let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        return Err("--exec needs a command".to_string());
    }
    for arg in &argv {
        let mut rest = arg.as_str();
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                return Err(format!("--exec {arg:?}: unbalanced '{{'"));
            };
            let token = &after[..close];
            if !EXEC_TOKENS.contains(&token) {
                return Err(format!("--exec {arg:?}: unknown token {{{token}}}"));
            }
            rest = &after[close + 1..];
        }
    }
    Ok(ExecHook { argv })
}

/// Expand `{tokens}` from `vars` (names absent from `vars` become
/// empty, so validated templates never leak placeholders).
fn expand_hook(hook: &ExecHook, vars: &[(&str, &str)]) -> Vec<String> {
    hook.argv
        .iter()
        .map(|arg| {
            let mut out = arg.clone();
            for token in EXEC_TOKENS {
                let value = vars
                    .iter()
                    .find(|(name, _)| name == token)
                    .map(|(_, value)| *value)
                    .unwrap_or("");
                out = out.replace(&format!("{{{token}}}"), value);
            }
            out
        })
        .collect()
}

/// Run a hook to completion; `false` on spawn failure or nonzero
/// exit (the caller logs; the C-STORE already succeeded).
async fn run_hook(hook: &ExecHook, vars: &[(&str, &str)]) -> bool {
    let argv = expand_hook(hook, vars);
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    match cmd.status().await {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!("storescp: --exec failed with {status}");
            false
        }
        Err(e) => {
            eprintln!("storescp: --exec spawn failed: {e}");
            false
        }
    }
}

struct StoreScp {
    directory: PathBuf,
    audit: Option<SenderWithId>,
    our_ip: String,
    exec: Option<ExecHook>,
    verbose: bool,
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
        let status = match vyna_dcm::write_part10(&meta, &dataset) {
            Ok(bytes) => {
                let safe = safe_instance_filename(&inst);
                let path = self.directory.join(format!("{safe}.dcm"));
                match std::fs::write(&path, bytes) {
                    Ok(()) => {
                        // Patient names are PHI: only log them with --verbose.
                        if self.verbose {
                            let patient = dataset.string(tags::PATIENT_NAME).unwrap_or("?");
                            println!("stored {} ({patient}) -> {}", inst, path.display());
                        } else {
                            println!("stored {} -> {}", inst, path.display());
                        }
                        if let Some(hook) = &self.exec {
                            let file = path.display().to_string();
                            run_hook(
                                hook,
                                &[
                                    ("file", file.as_str()),
                                    ("class", class.as_str()),
                                    ("instance", inst.as_str()),
                                    ("patient", dataset.string(tags::PATIENT_ID).unwrap_or("")),
                                    (
                                        "study",
                                        dataset.string(tags::STUDY_INSTANCE_UID).unwrap_or(""),
                                    ),
                                    ("caller", ctx.association.peer_ae.as_str()),
                                    ("called", ctx.association.local_ae.as_str()),
                                ],
                            )
                            .await;
                        }
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
        };
        // Peer socket IPs are not plumbed to handlers; audit AE titles.
        if let Some(audit) = &self.audit {
            match dataset.string(tags::STUDY_INSTANCE_UID) {
                Some(study) => {
                    let mut msg = vyna_dcm::AuditMessage::instances_transferred(
                        dataset.string(tags::PATIENT_ID),
                        study,
                        &[(&class, &inst)],
                        ctx.association.peer_ae.as_str(),
                        None,
                        ctx.association.local_ae.as_str(),
                        Some(&self.our_ip),
                        &audit.source,
                    );
                    msg.outcome = outcome_for_status(status);
                    emit_best_effort(Some(&audit.sender), &msg, "storescp");
                }
                None => eprintln!("storescp: skipping audit for {inst}: no Study UID"),
            }
        }
        status
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let audit = args.audit.sender(&args.listen.ae_title)?;
    let our_ip = host_of(&args.listen.bind).to_string();
    std::fs::create_dir_all(&args.directory).map_err(|e| e.to_string())?;
    let listener = TcpListener::bind(&args.listen.bind)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "storescp listening on {} -> {}",
        args.listen.bind,
        args.directory.display()
    );
    if let Some(audit) = &audit {
        let msg = vyna_dcm::AuditMessage::actor_activity(
            &args.listen.ae_title,
            Some(&our_ip),
            &audit.source,
            true,
        );
        emit_best_effort(Some(&audit.sender), &msg, "storescp");
    }

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
    let exec = args
        .exec
        .as_deref()
        .map(parse_exec_hook)
        .transpose()
        .map_err(|e| e.to_string())?;
    let handlers = Arc::new(StoreScp {
        directory: args.directory.clone(),
        audit,
        our_ip,
        exec,
        verbose: args.verbose,
    });

    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        // DIMSE is small-write chatty; Nagle would stall each PDU on the
        // peer's delayed ACK (~40ms).
        tcp.set_nodelay(true).ok();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_keeps_uid_chars() {
        assert_eq!(safe_instance_filename("1.2.840.10008.5"), "1.2.840.10008.5");
    }

    #[test]
    fn filename_sanitizes_separators() {
        assert_eq!(safe_instance_filename("../1.2/3\\4"), ".._1.2_3_4");
    }

    #[test]
    fn host_of_splits_addresses() {
        assert_eq!(host_of("127.0.0.1:11112"), "127.0.0.1");
        assert_eq!(host_of("pacs.example:104"), "pacs.example");
        assert_eq!(host_of("[::1]:11112"), "::1");
        assert_eq!(host_of("bare-host"), "bare-host");
    }

    #[test]
    fn exec_template_parses_and_expands() {
        let hook = parse_exec_hook("/usr/local/bin/route {file} {caller}").unwrap();
        let out = expand_hook(&hook, &[("file", "/store/1.2.dcm"), ("caller", "MODALITY")]);
        assert_eq!(
            out,
            vec!["/usr/local/bin/route", "/store/1.2.dcm", "MODALITY"]
        );
        // Unknown tokens and empty commands fail fast at startup.
        assert!(parse_exec_hook("route {bogus}").is_err());
        assert!(parse_exec_hook("route {file").is_err());
        assert!(parse_exec_hook("   ").is_err());
        // Values with spaces stay inside one argv element.
        let hook = parse_exec_hook("route {patient}").unwrap();
        assert_eq!(
            expand_hook(&hook, &[("patient", "Doe^Jane ^Suffix")]),
            vec!["route", "Doe^Jane ^Suffix"]
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn exec_hook_runs_and_reports_exit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("storescp-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // No shell interprets the template, so pipelines live in a
        // wrapper script (the documented pattern).
        let script = dir.join("hook.sh");
        let marker = dir.join("hooked.txt");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$1\" >> {}\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hook = parse_exec_hook(&format!("{} {{instance}}", script.display())).unwrap();
        assert!(run_hook(&hook, &[("instance", "9.9.9")]).await);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "9.9.9\n");
        // A failing hook reports false instead of failing the store.
        let hook = parse_exec_hook("/bin/false").unwrap();
        assert!(!run_hook(&hook, &[]).await);
        let hook = parse_exec_hook("/no/such/program").unwrap();
        assert!(!run_hook(&hook, &[]).await);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn audit_flags_parse() {
        let args = Args::try_parse_from([
            "storescp",
            "--audit-server",
            "udp://127.0.0.1:514",
            "--audit-source",
            "PACS",
        ])
        .unwrap();
        assert_eq!(
            args.audit.audit_server.as_deref(),
            Some("udp://127.0.0.1:514")
        );
        let wired = args.audit.sender("VYNA_SCP").unwrap().unwrap();
        assert_eq!(wired.source, "PACS");
        let off = Args::try_parse_from(["storescp"]).unwrap();
        assert!(off.audit.sender("VYNA_SCP").unwrap().is_none());
    }
}
