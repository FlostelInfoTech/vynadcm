//! `vyna-dcm-web`: DICOMweb client and server (PS3.18 §8).
//!
//! - [`DicomWebClient`] — QIDO-RS, WADO-RS, STOW-RS client
//! - [`server::dispatch`] — framework-agnostic server core over any
//!   [`server::DicomWebStore`] (plus [`server::MemoryStore`])
//! - [`hyper_server`] — hyper HTTP/1.1 front end (feature `server`)
//! - [`multipart`] — from-scratch `multipart/related` codec
//!
//! # Example
//!
//! ```no_run
//! use vyna_dcm_web::DicomWebClient;
//!
//! # async fn run() -> vyna_dcm_web::Result<()> {
//! let client = DicomWebClient::new("https://pacs.example/dicom-web")?;
//! let studies = client.search_studies(&[("PatientID", "E123")]).await?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod client;
mod error;
#[cfg(feature = "server")]
pub mod hyper_server;
pub mod multipart;
pub mod server;

pub use client::{Auth, DicomWebClient, StowResponse};
pub use error::{Error, Result};
