//! `vyna-dcm-net`: DICOM networking (upper layer + DIMSE services).
//!
//! - [`Association::request`] — SCU connect + A-ASSOCIATE
//! - [`service::accept`] — SCP accept + dispatch to [`service::Handlers`]
//! - SCU ops: [`Association::echo`], [`store`](Association::store),
//!   [`find_all`](Association::find_all), [`move_to`](Association::move_to),
//!   [`get_with`](Association::get_with), N-services
//! - [`worklist`] — MWL / MPPS / Storage Commitment dataset builders
//! - [`tls`] — TLS (feature `tls`, on by default)
//!
//! # Example
//!
//! ```no_run
//! use vyna_dcm_core::sop_class;
//! use vyna_dcm_net::{AeTitle, Association, AssociationOptions};
//! use vyna_dcm_net::service::propose;
//! use vyna_dcm_core::TransferSyntax;
//!
//! # async fn run() -> vyna_dcm_net::Result<()> {
//! let opts = AssociationOptions::default();
//! let mut assoc = Association::request(
//!     &opts,
//!     &AeTitle::new("VYNA_SCU")?,
//!     &AeTitle::new("PACS")?,
//!     "127.0.0.1:11112",
//!     propose(&[sop_class::VERIFICATION], &[TransferSyntax::ExplicitVrLittleEndian])?,
//! )
//! .await?;
//! let status = assoc.echo(sop_class::VERIFICATION).await?;
//! assoc.release().await?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod ae;
mod association;
pub mod dimse;
mod error;
pub mod pdu;
pub mod qrdb;
mod scu;
pub mod service;
#[cfg(feature = "tls")]
pub mod tls;
pub mod worklist;

pub use ae::AeTitle;
pub use association::{
    Association, AssociationInfo, AssociationOptions, AsyncReadWrite, DimseEvent, NegotiatedContext,
};
pub use dimse::{Command, DimseMessage, command_field, priority, status};
pub use error::{Error, Result};
pub use scu::MoveResult;
