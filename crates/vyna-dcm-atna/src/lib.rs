//! `vyna-dcm-atna`: ATNA audit-trail emission.
//!
//! - [`AuditMessage`] — DICOM audit message builders (PS3.15 A.5)
//! - [`SyslogSender`] / [`SyslogTarget`] — `udp://`, `tcp://`, and
//!   `tls://` syslog delivery (RFC 5424/5426/6587/5425)
//! - [`Outcome`] / [`ActionCode`] / [`Code`] — coded values
//!
//! # Example
//!
//! ```
//! use vyna_dcm_atna::{AuditMessage, SyslogTarget};
//!
//! let msg = AuditMessage::instances_transferred(
//!     Some("E123"),
//!     "1.2.3",
//!     &[("1.2.840.10008.5.1.4.1.1.2", "1.2.3.1")],
//!     "SOURCE_AE",
//!     Some("10.0.0.2"),
//!     "DEST_AE",
//!     None,
//!     "VYNA_SCP",
//! );
//! assert!(msg.to_xml().contains("code=\"110104\""));
//! let target: SyslogTarget = "udp://127.0.0.1:514".parse().unwrap();
//! assert_eq!(target.port, 514);
//! ```

#![warn(missing_docs)]

mod error;
mod message;
mod syslog;

pub use error::{Error, Result};
pub use message::{ActionCode, AuditMessage, Code, Outcome, Participant, ParticipantObject};
pub use syslog::{
    SyslogSender, SyslogTarget, Transport, octet_frame, pri_value, severity_for, syslog_frame,
};
