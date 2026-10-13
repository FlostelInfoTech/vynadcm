//! `auditsend`: emit one ATNA audit message to a syslog ARR.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use vyna_dcm::{ActionCode, AuditMessage, Code, Outcome, SyslogSender, SyslogTarget};

/// ATNA audit sender.
#[derive(Parser, Debug)]
#[command(name = "auditsend", about = "Send one ATNA audit message")]
struct Cli {
    /// Audit server as udp://host:port, tcp://host:port, tls://host:port
    /// (ports default to 514, or 6514 for TLS).
    #[arg(long)]
    server: String,
    /// Audit Source ID (defaults to the sending application/AE).
    #[arg(long)]
    source_id: Option<String>,
    /// Syslog hostname field (defaults to the audit source ID).
    #[arg(long)]
    hostname: Option<String>,
    /// Extra CA certificate (PEM) for tls:// verification.
    #[arg(long)]
    ca: Option<PathBuf>,
    /// Connect/write timeout in seconds.
    #[arg(long, default_value_t = 10)]
    timeout_secs: u64,
    /// Event outcome: success, minor, serious, major.
    #[arg(long, default_value = "success")]
    outcome: String,
    /// Print the audit XML without sending it.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
    #[command(subcommand)]
    event: Event,
}

/// Source/destination flow shared by transfer-like events.
#[derive(Args, Debug)]
struct FlowArgs {
    /// Study Instance UID.
    #[arg(long)]
    study: String,
    /// Patient ID (omit when unknown).
    #[arg(long)]
    patient: Option<String>,
    /// Instance as CLASS_UID:INSTANCE_UID. Repeatable.
    #[arg(long = "sop")]
    sops: Vec<String>,
    /// Source AE title.
    #[arg(long)]
    source_ae: String,
    /// Source IP or host name.
    #[arg(long)]
    source_ip: Option<String>,
    /// Destination AE title.
    #[arg(long)]
    dest_ae: String,
    /// Destination IP or host name.
    #[arg(long)]
    dest_ip: Option<String>,
}

#[derive(Args, Debug)]
struct QueryArgs {
    /// Patient ID (omit when unknown).
    #[arg(long)]
    patient: Option<String>,
    /// Study Instance UID (omit when unknown).
    #[arg(long)]
    study: Option<String>,
    /// Requesting (our) AE title.
    #[arg(long)]
    source_ae: String,
    /// Requesting IP or host name.
    #[arg(long)]
    source_ip: Option<String>,
    /// Peer AE title.
    #[arg(long)]
    dest_ae: String,
    /// Peer IP or host name.
    #[arg(long)]
    dest_ip: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Event {
    /// Application Activity start.
    Start(AppArgs),
    /// Application Activity stop.
    Stop(AppArgs),
    /// DICOM Instances Transferred.
    Transferred(FlowArgs),
    /// Begin Transferring DICOM Instances.
    BeginTransfer(FlowArgs),
    /// DICOM Query.
    Query(QueryArgs),
    /// DICOM Study Deleted.
    Deleted(DeletedArgs),
    /// Export of a study.
    Export(FlowArgs),
    /// Import of a study.
    Import(FlowArgs),
    /// Fully custom event triple.
    Custom(CustomArgs),
}

/// Application identity for start/stop events.
#[derive(Args, Debug)]
struct AppArgs {
    /// Application / AE name.
    #[arg(long)]
    app: String,
    /// Host IP or name.
    #[arg(long)]
    host: Option<String>,
}

#[derive(Args, Debug)]
struct DeletedArgs {
    /// Study Instance UID.
    #[arg(long)]
    study: String,
    /// Patient ID (omit when unknown).
    #[arg(long)]
    patient: Option<String>,
    /// Deleting application / AE name.
    #[arg(long)]
    app: String,
}

#[derive(Args, Debug)]
struct CustomArgs {
    /// Event code value (e.g. 110112).
    #[arg(long)]
    code: String,
    /// Coding scheme (default DCM).
    #[arg(long, default_value = "DCM")]
    system: String,
    /// Display name.
    #[arg(long)]
    display: String,
    /// Action: C(reate) R(ead) U(pdate) D(elete) E(xecute).
    #[arg(long)]
    action: String,
    /// Patient ID (adds a patient object).
    #[arg(long)]
    patient: Option<String>,
    /// Study UID (adds a study object).
    #[arg(long)]
    study: Option<String>,
    /// Source participant AE (adds a source participant).
    #[arg(long)]
    source_ae: Option<String>,
    /// Source IP or host name.
    #[arg(long)]
    source_ip: Option<String>,
    /// Destination participant AE (adds a destination participant).
    #[arg(long)]
    dest_ae: Option<String>,
    /// Destination IP or host name.
    #[arg(long)]
    dest_ip: Option<String>,
}

fn parse_outcome(s: &str) -> Result<Outcome, String> {
    match s.to_ascii_lowercase().as_str() {
        "success" | "0" => Ok(Outcome::Success),
        "minor" | "4" => Ok(Outcome::MinorFailure),
        "serious" | "8" => Ok(Outcome::SeriousFailure),
        "major" | "12" => Ok(Outcome::MajorFailure),
        other => Err(format!(
            "bad outcome {other:?}: success|minor|serious|major"
        )),
    }
}

fn parse_action(s: &str) -> Result<ActionCode, String> {
    match s.to_ascii_uppercase().as_str() {
        "C" | "CREATE" => Ok(ActionCode::Create),
        "R" | "READ" => Ok(ActionCode::Read),
        "U" | "UPDATE" => Ok(ActionCode::Update),
        "D" | "DELETE" => Ok(ActionCode::Delete),
        "E" | "EXECUTE" => Ok(ActionCode::Execute),
        other => Err(format!("bad action {other:?}: C|R|U|D|E")),
    }
}

fn parse_sop(s: &str) -> Result<(String, String), String> {
    match s.split_once(':') {
        Some((c, i)) if !c.is_empty() && !i.is_empty() => Ok((c.to_string(), i.to_string())),
        _ => Err(format!("sop {s:?} must be CLASS_UID:INSTANCE_UID")),
    }
}

fn flow_pairs(sops: &[String]) -> Result<Vec<(String, String)>, String> {
    sops.iter().map(|s| parse_sop(s)).collect()
}

fn build_message(event: &Event, source_id: &str) -> Result<AuditMessage, String> {
    match event {
        Event::Start(a) => Ok(AuditMessage::actor_activity(
            &a.app,
            a.host.as_deref(),
            source_id,
            true,
        )),
        Event::Stop(a) => Ok(AuditMessage::actor_activity(
            &a.app,
            a.host.as_deref(),
            source_id,
            false,
        )),
        Event::Transferred(f) => flow_message(f, source_id, FlowKind::Transferred),
        Event::BeginTransfer(f) => flow_message(f, source_id, FlowKind::Begin),
        Event::Query(q) => Ok(AuditMessage::query(
            q.patient.as_deref(),
            q.study.as_deref(),
            &q.source_ae,
            q.source_ip.as_deref(),
            &q.dest_ae,
            q.dest_ip.as_deref(),
            source_id,
        )),
        Event::Deleted(d) => Ok(AuditMessage::study_deleted(
            d.patient.as_deref(),
            &d.study,
            &d.app,
            source_id,
        )),
        Event::Export(f) => flow_message(f, source_id, FlowKind::Export),
        Event::Import(f) => flow_message(f, source_id, FlowKind::Import),
        Event::Custom(c) => {
            let mut msg = AuditMessage::new(
                Code::new(c.code.clone(), c.system.clone(), c.display.clone()),
                parse_action(&c.action)?,
                source_id,
            );
            if let Some(ae) = &c.source_ae {
                msg.participants
                    .push(vyna_dcm::Participant::source(ae, c.source_ip.clone(), true));
            }
            if let Some(ae) = &c.dest_ae {
                msg.participants.push(vyna_dcm::Participant::destination(
                    ae,
                    c.dest_ip.clone(),
                    false,
                ));
            }
            if let Some(p) = &c.patient {
                msg.objects.push(vyna_dcm::ParticipantObject::patient(p));
            }
            if let Some(s) = &c.study {
                msg.objects.push(vyna_dcm::ParticipantObject::study(s, &[]));
            }
            Ok(msg)
        }
    }
}

#[derive(Clone, Copy)]
enum FlowKind {
    Transferred,
    Begin,
    Export,
    Import,
}

fn flow_message(f: &FlowArgs, source_id: &str, kind: FlowKind) -> Result<AuditMessage, String> {
    let pairs = flow_pairs(&f.sops)?;
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(c, i)| (c.as_str(), i.as_str()))
        .collect();
    let a = (
        f.patient.as_deref(),
        f.study.as_str(),
        refs.as_slice(),
        f.source_ae.as_str(),
        f.source_ip.as_deref(),
        f.dest_ae.as_str(),
        f.dest_ip.as_deref(),
        source_id,
    );
    Ok(match kind {
        FlowKind::Transferred => {
            AuditMessage::instances_transferred(a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7)
        }
        FlowKind::Begin => AuditMessage::begin_transferring(a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7),
        FlowKind::Export => {
            AuditMessage::export_import(true, a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7)
        }
        FlowKind::Import => {
            AuditMessage::export_import(false, a.0, a.1, a.2, a.3, a.4, a.5, a.6, a.7)
        }
    })
}

fn default_source_id(event: &Event) -> &str {
    match event {
        Event::Start(a) | Event::Stop(a) => &a.app,
        Event::Transferred(f) | Event::BeginTransfer(f) | Event::Export(f) | Event::Import(f) => {
            &f.dest_ae
        }
        Event::Query(q) => &q.source_ae,
        Event::Deleted(d) => &d.app,
        Event::Custom(c) => c
            .source_ae
            .as_deref()
            .or(c.dest_ae.as_deref())
            .unwrap_or("auditsend"),
    }
}

fn run() -> i32 {
    let cli = Cli::parse();
    let outcome = match parse_outcome(&cli.outcome) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("auditsend: {e}");
            return 1;
        }
    };
    let source_id = cli
        .source_id
        .as_deref()
        .unwrap_or_else(|| default_source_id(&cli.event));
    let mut msg = match build_message(&cli.event, source_id) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("auditsend: {e}");
            return 1;
        }
    };
    msg.outcome = outcome;
    if cli.dry_run {
        println!("{}", msg.to_xml());
        return 0;
    }
    let target: SyslogTarget = match cli.server.parse() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("auditsend: {e}");
            return 1;
        }
    };
    let hostname = cli
        .hostname
        .clone()
        .unwrap_or_else(|| source_id.to_string());
    let mut sender = SyslogSender::new(target, hostname)
        .with_timeout(std::time::Duration::from_secs(cli.timeout_secs));
    if let Some(ca) = &cli.ca {
        match std::fs::read(ca) {
            Ok(pem) => sender = sender.with_ca_pem(pem),
            Err(e) => {
                eprintln!("auditsend: {}: {e}", ca.display());
                return 1;
            }
        }
    }
    match sender.emit(&msg) {
        Ok(()) => {
            println!(
                "sent {} ({}) to {}",
                msg.event.display, msg.event.code, cli.server
            );
            0
        }
        Err(e) => {
            eprintln!("auditsend: {e}");
            1
        }
    }
}

fn main() {
    std::process::exit(run());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> FlowArgs {
        FlowArgs {
            study: "1.2.3".to_string(),
            patient: Some("E123".to_string()),
            sops: vec!["1.2.840.10008.5.1.4.1.1.2:1.2.3.1".to_string()],
            source_ae: "SRC".to_string(),
            source_ip: Some("10.0.0.2".to_string()),
            dest_ae: "DST".to_string(),
            dest_ip: None,
        }
    }

    #[test]
    fn values_parse() {
        assert_eq!(
            parse_sop("1.2:1.3").unwrap(),
            ("1.2".to_string(), "1.3".to_string())
        );
        assert!(parse_sop("1.2").is_err());
        assert_eq!(parse_outcome("major").unwrap(), Outcome::MajorFailure);
        assert!(parse_outcome("bogus").is_err());
        assert_eq!(parse_action("r").unwrap(), ActionCode::Read);
        assert!(parse_action("bogus").is_err());
    }

    #[test]
    fn flow_messages_carry_triples_and_parties() {
        let msg = flow_message(&flow(), "SRCID", FlowKind::Transferred).unwrap();
        assert_eq!(msg.event.code, "110104");
        assert_eq!(msg.participants.len(), 2);
        assert_eq!(msg.objects.len(), 2);
        let xml = msg.to_xml();
        assert!(xml.contains("UserID=\"SRC\""), "{xml}");
        assert!(xml.contains("ParticipantObjectID=\"1.2.3\""), "{xml}");
        let begin = flow_message(&flow(), "SRCID", FlowKind::Begin).unwrap();
        assert_eq!(begin.event.code, "110102");
        assert!(
            flow_message(
                &FlowArgs {
                    sops: vec!["bad".to_string()],
                    ..flow()
                },
                "SRCID",
                FlowKind::Transferred
            )
            .is_err()
        );
    }

    #[test]
    fn subcommands_parse() {
        let cli = Cli::try_parse_from([
            "auditsend",
            "--server",
            "udp://127.0.0.1:514",
            "transferred",
            "--study",
            "1.2.3",
            "--source-ae",
            "A",
            "--dest-ae",
            "B",
        ])
        .unwrap();
        assert!(matches!(cli.event, Event::Transferred(_)));
        assert_eq!(default_source_id(&cli.event), "B");
    }
}
