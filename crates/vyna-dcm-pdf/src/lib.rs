//! `vyna-dcm-pdf`: Encapsulated PDF helpers.
//!
//! - [`PdfInstanceMeta`] — identifying metadata for one SOP instance
//! - [`pdf_to_dataset`] — wrap PDF bytes in a complete, validatable
//!   Encapsulated PDF Storage dataset (C-STORE / Part-10 / STOW-RS ready)
//! - [`pdf_from_dataset`] — extract the bytes (plus title/MIME/SOP UID)
//!   back out of a dataset
//! - [`file_meta_for_dataset`] — Part-10 file meta for a built instance
//!
//! # Example
//!
//! ```
//! use vyna_dcm_core::PersonName;
//! use vyna_dcm_pdf::{PdfInstanceMeta, pdf_from_dataset, pdf_to_dataset};
//!
//! let pdf = b"%PDF-1.4\n%EOF-tail";
//! let meta = PdfInstanceMeta::new(PersonName::parse("Doe^Jane"), "P123");
//! let ds = pdf_to_dataset(pdf, &meta).unwrap();
//! let back = pdf_from_dataset(&ds).unwrap();
//! assert_eq!(back.bytes, pdf);
//! assert_eq!(back.mime_type.as_deref(), Some("application/pdf"));
//! ```

#![warn(missing_docs)]

mod document;
mod error;

pub use document::{
    CHARSET_UTF8, ConceptName, ExtractedPdf, MIME_TYPE_PDF, PdfInstanceMeta, file_meta_for_dataset,
    pdf_from_dataset, pdf_to_dataset,
};
pub use error::{Error, Result};
