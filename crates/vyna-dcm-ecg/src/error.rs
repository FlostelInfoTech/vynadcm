//! ECG error type.

use thiserror::Error;

/// Error type for ECG waveform helpers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Waveform structure problem.
    #[error("waveform error: {0}")]
    Waveform(String),

    /// Sample interpretation or bit depth not supported here.
    #[error("unsupported samples: {0}")]
    Samples(String),

    /// Required attribute missing.
    #[error("missing attribute {0}")]
    Missing(String),

    /// Caller-supplied metadata is not valid DICOM (bad UID syntax or
    /// an enumerated value outside its set).
    #[error("invalid metadata: {0}")]
    InvalidMeta(String),

    /// Codec failure passthrough.
    #[error(transparent)]
    Codec(#[from] vyna_dcm_codec::Error),

    /// Core failure passthrough.
    #[error(transparent)]
    Core(#[from] vyna_dcm_core::Error),
}

/// Fallible result for ECG operations.
pub type Result<T> = std::result::Result<T, Error>;
