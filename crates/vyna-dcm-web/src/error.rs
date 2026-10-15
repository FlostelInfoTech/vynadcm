//! DICOMweb error type.

use thiserror::Error;

/// Error type for DICOMweb client/server operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// HTTP transport failure.
    #[error("HTTP error: {0}")]
    Http(String),

    /// Request timed out (retryable for idempotent operations such
    /// as QIDO/WADO reads; STOW retries may duplicate instances).
    #[error("request timed out: {0}")]
    Timeout(String),

    /// Connection-level failure: refused, reset, unreachable host,
    /// or DNS failure (no TLS handshake was completed).
    #[error("connection failed: {0}")]
    Connect(String),

    /// TLS handshake or certificate-verification failure.
    #[error("TLS error: {0}")]
    Tls(String),

    /// Non-successful HTTP status.
    #[error("DICOMweb status {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body snippet.
        body: String,
    },

    /// Malformed multipart/related body.
    #[error("multipart error: {0}")]
    Multipart(String),

    /// Malformed DICOM JSON.
    #[error("JSON error: {0}")]
    Json(String),

    /// Bad request path or parameters.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Codec failure passthrough.
    #[error(transparent)]
    Codec(#[from] vyna_dcm_codec::Error),

    /// Core data-model failure passthrough.
    #[error(transparent)]
    Core(#[from] vyna_dcm_core::Error),
}

/// Fallible result for DICOMweb operations.
pub type Result<T> = std::result::Result<T, Error>;
