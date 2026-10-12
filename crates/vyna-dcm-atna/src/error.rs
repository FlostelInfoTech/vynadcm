//! ATNA errors.

/// Audit emission failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The audit server URL is malformed.
    #[error("bad audit server URL {0:?}: want udp://host:port, tcp://host:port or tls://host:port")]
    BadUrl(String),
    /// TLS support was compiled out but a `tls://` server was given.
    #[error("TLS audit transport needs the `tls` feature")]
    TlsDisabled,
    /// Transport I/O failure.
    #[error("audit transport: {0}")]
    Transport(String),
    /// TLS handshake / certificate failure.
    #[error("audit TLS: {0}")]
    Tls(String),
}

/// Audit result.
pub type Result<T> = std::result::Result<T, Error>;
