//! Shared CLI plumbing for the `vyna-dcm` network tools.
#![allow(dead_code)] // each binary uses a subset

use std::path::PathBuf;
use std::time::Duration;

use vyna_dcm::TransferSyntax;
use vyna_dcm::core::{Tag, lookup_keyword};

/// Maximum concurrent inbound associations per SCP tool, mirroring
/// the DICOMweb server's connection cap: past this, the accept loop
/// drops new connections with a warning instead of spawning
/// unbounded tasks (slow-loris on the association count).
pub const MAX_SCP_ASSOCIATIONS: usize = 1024;
use vyna_dcm::net::pdu::ExtendedNegotiation;
use vyna_dcm::net::service::propose;
use vyna_dcm::net::{AeTitle, Association, AssociationOptions};
use vyna_dcm::{DataSet, Vr};

/// Connection + association arguments shared by all SCU tools.
#[derive(clap::Args, Debug)]
pub struct ConnArgs {
    /// Peer address as `host:port`.
    #[arg(long)]
    pub connect: String,
    /// Called (destination) AE title.
    #[arg(long, default_value = "VYNA_SCP")]
    pub called: String,
    /// Calling (our) AE title.
    #[arg(long, default_value = "VYNA_SCU")]
    pub calling: String,
    /// Our max receive PDU length.
    #[arg(long, default_value_t = 256 * 1024)]
    pub max_pdu: u32,
    /// Per-read timeout in seconds.
    #[arg(long, default_value_t = 30)]
    pub timeout_secs: u64,
    /// Use DICOM TLS (PS3.15). The server is verified against the
    /// Mozilla root bundle (plus `--tls-ca`), not the OS store.
    #[arg(long, default_value_t = false)]
    pub tls: bool,
    /// Extra CA certificate (PEM) for TLS server verification, in
    /// addition to the Mozilla roots.
    #[arg(long)]
    pub tls_ca: Option<PathBuf>,
    /// Client certificate (PEM) for mutual TLS.
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,
    /// Client private key (PEM) for mutual TLS.
    #[arg(long)]
    pub tls_key: Option<PathBuf>,
    /// TLS server name (defaults to the connection host).
    #[arg(long)]
    pub tls_server_name: Option<String>,
}

/// Listener arguments shared by all SCP tools.
#[derive(clap::Args, Debug)]
pub struct ListenArgs {
    /// Bind address as `host:port`.
    #[arg(long, default_value = "127.0.0.1:11112")]
    pub bind: String,
    /// Our AE title.
    #[arg(long, default_value = "VYNA_SCP")]
    pub ae_title: String,
    /// Our max receive PDU length.
    #[arg(long, default_value_t = 256 * 1024)]
    pub max_pdu: u32,
    /// Per-read timeout in seconds.
    #[arg(long, default_value_t = 300)]
    pub timeout_secs: u64,
    /// Require a matching Called-AE title (recommended: without
    /// it, any peer calling any AE title is accepted).
    #[arg(long, default_value_t = false)]
    pub strict_ae: bool,
    /// TLS server certificate chain (PEM); enables TLS when present.
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,
    /// TLS server private key (PEM).
    #[arg(long)]
    pub tls_key: Option<PathBuf>,
    /// Client CA (PEM) for mutual TLS. Client certificates verify
    /// against this CA alone (no Mozilla roots on this path).
    #[arg(long)]
    pub tls_client_ca: Option<PathBuf>,
}

impl ConnArgs {
    /// Build association options from CLI flags.
    #[must_use]
    pub fn assoc_options(&self) -> AssociationOptions {
        let mut o = AssociationOptions::default();
        o.max_pdu_len = self.max_pdu;
        o.read_timeout = Duration::from_secs(self.timeout_secs);
        o
    }

    /// Default uncompressed transfer syntaxes for proposals.
    #[must_use]
    pub fn default_syntaxes() -> Vec<TransferSyntax> {
        vec![
            TransferSyntax::ExplicitVrLittleEndian,
            TransferSyntax::ImplicitVrLittleEndian,
        ]
    }

    /// Connect (plain or TLS) and request an association.
    ///
    /// `extended` carries SOP-class extended negotiations (`0x56`,
    /// e.g. relational-queries proposals for QR SOP classes).
    pub async fn connect(
        &self,
        sop_classes: &[&str],
        syntaxes: &[TransferSyntax],
        extended: Vec<ExtendedNegotiation>,
    ) -> anyhowish::Result<Association> {
        let mut opts = self.assoc_options();
        opts.extended_negotiation = extended;
        let calling = AeTitle::new(self.calling.clone())?;
        let called = AeTitle::new(self.called.clone())?;
        let proposed = propose(sop_classes, syntaxes)?;
        if !self.tls {
            // TLS material without --tls would silently run plaintext.
            if self.tls_ca.is_some()
                || self.tls_cert.is_some()
                || self.tls_key.is_some()
                || self.tls_server_name.is_some()
            {
                return Err(anyhowish::Error::msg(
                    "--tls-ca/--tls-cert/--tls-key/--tls-server-name need --tls",
                ));
            }
            return Ok(
                Association::request(&opts, &calling, &called, &self.connect, proposed).await?,
            );
        }
        // host_of (not a naive first-':' split) so IPv6 literals
        // yield a usable TLS server name.
        let host = host_of(&self.connect).to_string();
        let server_name = self.tls_server_name.clone().unwrap_or(host);
        let ca = read_opt(&self.tls_ca)?;
        let identity = match (&self.tls_cert, &self.tls_key) {
            (Some(c), Some(k)) => Some((std::fs::read(c)?, std::fs::read(k)?)),
            (None, None) => None,
            _ => {
                return Err(anyhowish::Error::msg(
                    "--tls-cert and --tls-key go together",
                ));
            }
        };
        let connector = vyna_dcm::net::tls::TlsConnector::new(
            server_name,
            ca.as_deref(),
            identity.as_ref().map(|(c, k)| (c.as_slice(), k.as_slice())),
        )?;
        let stream = connector.connect_to(&self.connect).await?;
        Ok(Association::request_over(&opts, &calling, &called, stream, proposed).await?)
    }
}

impl ListenArgs {
    /// Build association options from listener flags.
    #[must_use]
    pub fn assoc_options(&self) -> AssociationOptions {
        let mut o = AssociationOptions::default();
        o.max_pdu_len = self.max_pdu;
        o.read_timeout = Duration::from_secs(self.timeout_secs);
        o
    }

    /// TLS acceptor from `--tls-cert/--tls-key/--tls-client-ca` (or
    /// `None` for plaintext). Errors when the flags disagree: half a
    /// server identity, or a client CA with no server identity (which
    /// would otherwise be silently ignored).
    pub fn tls_acceptor(
        &self,
    ) -> anyhowish::Result<Option<std::sync::Arc<vyna_dcm::net::tls::TlsAcceptor>>> {
        match (&self.tls_cert, &self.tls_key) {
            (Some(c), Some(k)) => {
                let cert = std::fs::read(c).map_err(|e| e.to_string())?;
                let key = std::fs::read(k).map_err(|e| e.to_string())?;
                let ca = self
                    .tls_client_ca
                    .as_ref()
                    .map(std::fs::read)
                    .transpose()
                    .map_err(|e| e.to_string())?;
                let acc = vyna_dcm::net::tls::TlsAcceptor::new(&cert, &key, ca.as_deref())
                    .map_err(|e| e.to_string())?;
                Ok(Some(std::sync::Arc::new(acc)))
            }
            (None, None) => {
                if self.tls_client_ca.is_some() {
                    return Err(anyhowish::Error::msg(
                        "--tls-client-ca needs --tls-cert and --tls-key",
                    ));
                }
                Ok(None)
            }
            _ => Err(anyhowish::Error::msg(
                "--tls-cert and --tls-key go together",
            )),
        }
    }
}

/// Parse a query-key name (keyword or `GGGGEEEE` tag) into a [`Tag`].
pub fn parse_tag_key(name: &str) -> anyhowish::Result<Tag> {
    if let Some(e) = lookup_keyword(name) {
        return Ok(e.tag);
    }
    if let Some(e) = vyna_dcm::core::ENTRIES
        .iter()
        .find(|e| e.keyword.eq_ignore_ascii_case(name))
    {
        return Ok(e.tag);
    }
    let hex: String = name.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() == 8 {
        let g = u16::from_str_radix(&hex[0..4], 16).unwrap();
        let e = u16::from_str_radix(&hex[4..8], 16).unwrap();
        return Ok(Tag::new(g, e));
    }
    Err(anyhowish::Error::msg(format!("unknown query key {name:?}")))
}

/// Build a C-FIND/C-MOVE/C-GET identifier from `-k KEY=value` pairs.
///
/// Q/R models get a derived Query/Retrieve Level unless the caller sets
/// one; MWL identifiers must not carry it.
pub fn build_qr_query(keys: &[String], is_mwl: bool) -> anyhowish::Result<DataSet> {
    let mut ds = DataSet::new();
    for key in keys {
        let (name, value) = key
            .split_once('=')
            .ok_or_else(|| format!("key {key:?} must be KEY=value"))?;
        let tag = parse_tag_key(name)?;
        // Look up the VR from the dictionary; default to LO for unknowns.
        let vr = vyna_dcm::core::dictionary_vr(tag).unwrap_or(Vr::Lo);
        if tag == vyna_dcm::tags::PATIENT_NAME {
            ds.put_person_name(tag, vec![value.parse().map_err(|e| format!("{e:?}"))?]);
        } else {
            ds.put_text(tag, vr, value);
        }
    }
    if !is_mwl && !ds.contains(vyna_dcm::tags::QUERY_RETRIEVE_LEVEL) {
        use vyna_dcm::tags as t;
        let level = if ds.contains(t::SOP_INSTANCE_UID) {
            "IMAGE"
        } else if ds.contains(t::SERIES_INSTANCE_UID) {
            "SERIES"
        } else if ds.contains(t::STUDY_INSTANCE_UID) {
            "STUDY"
        } else {
            "PATIENT"
        };
        ds.put_text(t::QUERY_RETRIEVE_LEVEL, Vr::Cs, level);
    }
    Ok(ds)
}

/// Sanitize a SOP Instance UID into a safe file stem.
#[must_use]
pub fn safe_instance_filename(inst: &str) -> String {
    inst.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn read_opt(path: &Option<PathBuf>) -> anyhowish::Result<Option<Vec<u8>>> {
    path.as_ref()
        .map(std::fs::read)
        .transpose()
        .map_err(anyhowish::Error::from)
}

/// Audit emission arguments (ATNA) shared by the audited tools.
#[derive(clap::Args, Debug, Default)]
pub struct AuditArgs {
    /// Audit server as udp://host:port, tcp://host:port, tls://host:port.
    /// Audit emission is disabled when omitted.
    #[arg(long)]
    pub audit_server: Option<String>,
    /// Audit Source ID (defaults to our AE title).
    #[arg(long)]
    pub audit_source: Option<String>,
    /// Extra CA certificate (PEM) for tls:// audit verification,
    /// added to the Mozilla roots.
    #[arg(long)]
    pub audit_ca: Option<PathBuf>,
}

impl AuditArgs {
    /// Build a syslog sender, or `None` when auditing is disabled.
    pub fn sender(&self, default_source: &str) -> anyhowish::Result<Option<SenderWithId>> {
        let Some(url) = &self.audit_server else {
            return Ok(None);
        };
        let target: vyna_dcm::SyslogTarget = url
            .parse()
            .map_err(|e: vyna_dcm::atna::Error| e.to_string())?;
        let source = self
            .audit_source
            .clone()
            .unwrap_or_else(|| default_source.to_string());
        let mut sender = vyna_dcm::SyslogSender::new(target, source.clone());
        if let Some(ca) = &self.audit_ca {
            sender = sender
                .with_ca_pem(std::fs::read(ca).map_err(|e| format!("{}: {e}", ca.display()))?);
        }
        Ok(Some(SenderWithId { sender, source }))
    }
}

/// A syslog sender plus its audit source ID.
#[derive(Clone, Debug)]
pub struct SenderWithId {
    /// The sender.
    pub sender: vyna_dcm::SyslogSender,
    /// Audit Source ID for emitted messages.
    pub source: String,
}

/// Emit an audit message without ever failing the caller: transport
/// problems print a warning and the clinical path continues.
/// Best-effort audit emission that never blocks a runtime worker:
/// [`SyslogSender`](vyna_dcm::SyslogSender) delivery is blocking I/O
/// (TCP connect up to the sender timeout, unbounded DNS), so it runs
/// on the blocking pool while the caller awaits the outcome for its
/// warning.
pub async fn emit_best_effort(
    sender: Option<&vyna_dcm::SyslogSender>,
    msg: &vyna_dcm::AuditMessage,
    tool: &str,
) {
    let Some(sender) = sender else {
        return;
    };
    let sender = sender.clone();
    let msg = msg.clone();
    let tool = tool.to_string();
    match tokio::task::spawn_blocking(move || sender.emit(&msg)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("{tool}: audit emission failed (continuing): {e}"),
        Err(e) => eprintln!("{tool}: audit task failed (continuing): {e}"),
    }
}

/// Map a DIMSE status to an audit outcome.
#[must_use]
pub fn outcome_for_status(status: u16) -> vyna_dcm::Outcome {
    use vyna_dcm::net::dimse::status;
    if status::is_success(status) {
        vyna_dcm::Outcome::Success
    } else if status::is_warning(status) {
        vyna_dcm::Outcome::MinorFailure
    } else {
        vyna_dcm::Outcome::MajorFailure
    }
}

/// Host part of a `host:port` address (bracketed IPv6 handled).
#[must_use]
pub fn host_of(addr: &str) -> &str {
    if let Some(bracketed) = addr.strip_prefix('[') {
        return bracketed.split(']').next().unwrap_or(addr);
    }
    match addr.rsplit_once(':') {
        Some((host, _)) if !host.contains(':') => host,
        _ => addr,
    }
}

/// Minimal error-aggregation alias so the tools stay dependency-light.
/// (A tiny local `anyhow`-shaped helper; see below.)
pub mod anyhowish {
    /// Boxed error result.
    pub type Result<T> = std::result::Result<T, Error>;

    /// Boxed error.
    #[derive(Debug)]
    pub struct Error(pub String);

    impl Error {
        /// Build from a message.
        pub fn msg(s: impl Into<String>) -> Self {
            Self(s.into())
        }
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for Error {}

    impl From<std::io::Error> for Error {
        fn from(e: std::io::Error) -> Self {
            Self(e.to_string())
        }
    }

    impl From<vyna_dcm::net::Error> for Error {
        fn from(e: vyna_dcm::net::Error) -> Self {
            Self(e.to_string())
        }
    }

    impl From<vyna_dcm::core::Error> for Error {
        fn from(e: vyna_dcm::core::Error) -> Self {
            Self(e.to_string())
        }
    }

    impl From<vyna_dcm::codec::Error> for Error {
        fn from(e: vyna_dcm::codec::Error) -> Self {
            Self(e.to_string())
        }
    }

    impl From<String> for Error {
        fn from(s: String) -> Self {
            Self(s)
        }
    }
}

/// Relational-queries `0x56` proposal for `sop` when the caller
/// asked for it (QR tools' `--relational`); empty otherwise.
#[must_use]
pub fn relational_proposal(relational: bool, sop: &str) -> Vec<ExtendedNegotiation> {
    if relational {
        vec![ExtendedNegotiation {
            sop_class_uid: sop.to_string(),
            app_info: vec![1],
        }]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{host_of, relational_proposal};

    #[test]
    fn relational_proposal_builds_bit_set_entry() {
        let sop = "1.2.840.10008.5.1.4.1.2.2.1";
        let out = relational_proposal(true, sop);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sop_class_uid, sop);
        assert_eq!(out[0].app_info, vec![1]);
        assert!(relational_proposal(false, sop).is_empty());
    }

    #[test]
    fn host_parsing_covers_ipv6() {
        assert_eq!(host_of("pacs.example:11112"), "pacs.example");
        assert_eq!(host_of("pacs.example"), "pacs.example");
        assert_eq!(host_of("[::1]:11112"), "::1");
        assert_eq!(host_of("[2001:db8::1]:11112"), "2001:db8::1");
        assert_eq!(host_of("::1"), "::1");
    }
}
