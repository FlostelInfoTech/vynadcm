//! Networking error type.

use thiserror::Error;

/// Error type for DICOM upper-layer and DIMSE operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Transport I/O failure.
    #[error("transport error: {0}")]
    Io(#[from] std::io::Error),

    /// Malformed PDU.
    #[error("malformed PDU: {0}")]
    BadPdu(String),

    /// Unexpected PDU in the current state.
    #[error("unexpected PDU: expected {expected}, got {got}")]
    UnexpectedPdu {
        /// What the state machine expected.
        expected: &'static str,
        /// What arrived.
        got: String,
    },

    /// Association rejected by the peer.
    #[error("association rejected: result={result} source={origin} reason={reason}")]
    Rejected {
        /// Reject result.
        result: u8,
        /// Reject source.
        origin: u8,
        /// Reject reason.
        reason: u8,
    },

    /// Peer aborted the association.
    #[error("association aborted by peer: source={origin} reason={reason}")]
    Aborted {
        /// Abort source.
        origin: u8,
        /// Abort reason.
        reason: u8,
    },

    /// Peer released the association while an operation was outstanding.
    #[error("association released by peer")]
    Released,

    /// No accepted presentation context for an abstract syntax.
    #[error("no accepted presentation context for {0}")]
    NoPresentationContext(String),

    /// Unknown presentation context ID on received data.
    #[error("unknown presentation context id {0}")]
    UnknownContext(u8),

    /// Malformed DIMSE command set.
    #[error("malformed DIMSE command: {0}")]
    BadCommand(String),

    /// DIMSE service failure status received or produced.
    #[error("DIMSE failure status {status:#06X} in {command}: {comment}")]
    DimseStatus {
        /// Failing command field name.
        command: String,
        /// Status code.
        status: u16,
        /// Optional error comment.
        comment: String,
    },

    /// Operation timed out.
    #[error("operation timed out: {0}")]
    Timeout(&'static str),

    /// A reassembled DIMSE command or dataset exceeded the size cap.
    #[error("DIMSE message exceeds {limit} bytes")]
    MessageTooLarge {
        /// Configured cap in bytes.
        limit: u64,
    },

    /// A collecting C-FIND exceeded the match-count cap: the peer sent
    /// more pending responses than [`MAX_FIND_MATCHES`](crate::MAX_FIND_MATCHES).
    /// The SCU drained to the final response, so the association stays usable.
    #[error("C-FIND matches exceeded {limit}")]
    TooManyMatches {
        /// Match-count cap.
        limit: usize,
    },

    /// A collecting C-FIND exceeded the aggregate byte budget: responses
    /// past [`MAX_FIND_BYTES`](crate::MAX_FIND_BYTES) were dropped.
    /// The SCU drained to the final response, so the association stays usable.
    #[error("C-FIND collected responses exceeded {limit} bytes")]
    MatchesTooLarge {
        /// Aggregate byte budget.
        limit: u64,
    },

    /// Invalid AE title.
    #[error("invalid AE title `{0}`: {1}")]
    BadAeTitle(String, &'static str),

    /// TLS configuration or handshake failure.
    #[error("TLS error: {0}")]
    Tls(String),

    /// Move destination cannot be resolved to an address.
    #[error("unknown move destination `{0}`")]
    UnknownMoveDestination(String),

    /// Codec failure passthrough.
    #[error(transparent)]
    Codec(#[from] vyna_dcm_codec::Error),

    /// Core data-model failure passthrough.
    #[error(transparent)]
    Core(#[from] vyna_dcm_core::Error),
}

/// Fallible result for networking operations.
pub type Result<T> = std::result::Result<T, Error>;
