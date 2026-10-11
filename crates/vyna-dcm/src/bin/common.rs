//! Shared CLI plumbing for the `vyna-dcm` network tools.
#![allow(dead_code)] // each binary uses a subset

use std::path::PathBuf;
use std::time::Duration;

use vyna_dcm::TransferSyntax;
use vyna_dcm::net::service::propose;
use vyna_dcm::net::{AeTitle, Association, AssociationOptions};

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
    /// Use DICOM TLS (PS3.15).
    #[arg(long, default_value_t = false)]
    pub tls: bool,
    /// Extra CA certificate (PEM) for TLS server verification.
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
    /// Require a matching Called-AE title.
    #[arg(long, default_value_t = false)]
    pub strict_ae: bool,
    /// TLS server certificate chain (PEM); enables TLS when present.
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,
    /// TLS server private key (PEM).
    #[arg(long)]
    pub tls_key: Option<PathBuf>,
    /// Client CA (PEM) for mutual TLS.
    #[arg(long)]
    pub tls_client_ca: Option<PathBuf>,
}

impl ConnArgs {
    /// Build association options from CLI flags.
    #[must_use]
    pub fn assoc_options(&self) -> AssociationOptions {
        AssociationOptions {
            max_pdu_len: self.max_pdu,
            read_timeout: Duration::from_secs(self.timeout_secs),
            ..AssociationOptions::default()
        }
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
    pub async fn connect(
        &self,
        sop_classes: &[&str],
        syntaxes: &[TransferSyntax],
    ) -> anyhowish::Result<Association> {
        let opts = self.assoc_options();
        let calling = AeTitle::new(self.calling.clone())?;
        let called = AeTitle::new(self.called.clone())?;
        let proposed = propose(sop_classes, syntaxes);
        if !self.tls {
            return Ok(
                Association::request(&opts, &calling, &called, &self.connect, proposed).await?,
            );
        }
        let host = self
            .connect
            .split(':')
            .next()
            .unwrap_or("localhost")
            .to_string();
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

fn read_opt(path: &Option<PathBuf>) -> anyhowish::Result<Option<Vec<u8>>> {
    path.as_ref()
        .map(std::fs::read)
        .transpose()
        .map_err(anyhowish::Error::from)
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
