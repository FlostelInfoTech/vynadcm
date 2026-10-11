//! Codec error type.

use thiserror::Error;
use vyna_dcm_core::{Tag, Vr};

/// Error type for encoding/decoding operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Input ended mid-element.
    #[error("truncated data at offset {offset}: {what}")]
    Truncated {
        /// Byte offset where input ran out.
        offset: usize,
        /// What was being read.
        what: &'static str,
    },

    /// Unknown VR code on the wire (explicit VR syntaxes).
    #[error("unknown VR {code:?} for {tag} at offset {offset}")]
    UnknownVr {
        /// Attribute tag.
        tag: Tag,
        /// Raw two-byte code.
        code: String,
        /// Byte offset of the element.
        offset: usize,
    },

    /// VR needed from the dictionary but the tag is unknown/private.
    #[error("no dictionary VR for {tag} in implicit VR syntax at offset {offset}")]
    MissingDictionaryVr {
        /// Attribute tag.
        tag: Tag,
        /// Byte offset of the element.
        offset: usize,
    },

    /// Undefined length used with a VR that forbids it.
    #[error("undefined length not allowed for {vr} ({tag}) at offset {offset}")]
    BadUndefinedLength {
        /// Attribute tag.
        tag: Tag,
        /// Value representation.
        vr: Vr,
        /// Byte offset of the element.
        offset: usize,
    },

    /// Malformed sequence/item structure.
    #[error("malformed sequence at offset {offset}: {message}")]
    BadSequence {
        /// Byte offset.
        offset: usize,
        /// What went wrong.
        message: String,
    },

    /// Malformed encapsulated (fragment) data.
    #[error("malformed fragments at offset {offset}: {message}")]
    BadFragments {
        /// Byte offset.
        offset: usize,
        /// What went wrong.
        message: String,
    },

    /// Value bytes do not match the VR.
    #[error("bad value for {tag} ({vr}): {message}")]
    BadValue {
        /// Attribute tag.
        tag: Tag,
        /// Value representation.
        vr: Vr,
        /// What went wrong.
        message: String,
    },

    /// In-memory value cannot be encoded with the element's VR.
    #[error("cannot encode {tag} as {vr}: {message}")]
    Unencodable {
        /// Attribute tag.
        tag: Tag,
        /// Value representation.
        vr: Vr,
        /// What went wrong.
        message: String,
    },

    /// Not a Part-10 file (preamble/`DICM` missing).
    #[error("not a Part-10 file: {0}")]
    NotPart10(&'static str),

    /// File meta is incomplete or inconsistent.
    #[error("bad file meta: {0}")]
    BadFileMeta(String),

    /// Transfer syntax this library cannot process.
    #[error("unsupported transfer syntax {0}")]
    UnsupportedTransferSyntax(String),

    /// RLE compression error.
    #[error("RLE error: {0}")]
    Rle(String),

    /// Deflate (zlib) error.
    #[error("deflate error: {0}")]
    Deflate(String),

    /// DICOM JSON model error.
    #[error("JSON error: {0}")]
    Json(String),

    /// Base64 error inside JSON `InlineBinary`.
    #[error("base64 error: {0}")]
    Base64(String),

    /// Core data-model error passthrough.
    #[error(transparent)]
    Core(#[from] vyna_dcm_core::Error),
}

/// Fallible result for codec operations.
pub type Result<T> = std::result::Result<T, Error>;
