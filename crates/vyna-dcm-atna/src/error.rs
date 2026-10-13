//! ATNA errors.

/// Audit emission failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The audit server URL is malformed.
    #[error("bad audit server URL {0:?}: want udp://host:port, tcp://host:port or tls://host:port")]
    BadUrl(String),
    /// TLS support was compiled out but a `tls://` server was given.
    #[error("TLS audit transport needs the `tls` feature")]
    TlsDisabled,
    /// Transport I/O failure (connect, DNS, read, write, flush):
    /// the [`std::io::Error`] — and its [`Kind`](std::io::ErrorKind),
    /// so callers can tell refused from timeout — is preserved.
    #[error("audit transport I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Transport failure that is not plain I/O (oversize datagram,
    /// no resolved addresses).
    #[error("audit transport: {0}")]
    Transport(String),
    /// TLS configuration failure (bad PEM, bad server name,
    /// handshake construction). I/O on an established TLS stream is
    /// [`Error::Io`](Self::Io), with rustls's reason in the message.
    #[error("audit TLS: {0}")]
    Tls(String),
}

/// Audit result.
pub type Result<T> = std::result::Result<T, Error>;
