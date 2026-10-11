//! Core error type shared by the `vyna-dcm-core` crate.

use thiserror::Error;

/// Error type for core DICOM data-model operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Unknown two-letter value representation code.
    #[error("unknown value representation `{0}`")]
    UnknownVr(String),

    /// Malformed `(GGGG,EEEE)` tag text.
    #[error("invalid tag `{0}`")]
    InvalidTag(String),

    /// Malformed UID value.
    #[error("invalid UID `{value}`: {reason}")]
    InvalidUid {
        /// The offending value.
        value: String,
        /// Why it is invalid.
        reason: &'static str,
    },

    /// A value does not match its attribute or VR.
    #[error("value error for {tag}: {message}")]
    Value {
        /// Attribute the value belongs to.
        tag: String,
        /// What went wrong.
        message: String,
    },

    /// A required attribute is absent from the dataset.
    #[error("missing required attribute {0}")]
    MissingAttribute(String),

    /// Generic parse failure.
    #[error("parse error: {0}")]
    Parse(String),
}

/// Fallible result for core operations.
pub type Result<T> = std::result::Result<T, Error>;
