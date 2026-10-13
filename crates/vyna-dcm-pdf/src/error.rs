//! `vyna-dcm-pdf` error type.

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors from Encapsulated PDF building and extraction.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No PDF bytes were supplied to the builder.
    #[error("no PDF content: Encapsulated Document would be an empty Type 1 attribute")]
    EmptyDocument,
    /// The bytes lack the `%PDF-` header: wrapping them would
    /// mislabel arbitrary data as `application/pdf`.
    #[error("not a PDF file: missing %PDF- header")]
    NotPdf,
    /// The PDF exceeds the 32-bit value length of Encapsulated Document.
    #[error("PDF is {0} bytes, exceeding the 4 GiB value limit")]
    DocumentTooLarge(u64),
    /// A required attribute is missing from the dataset.
    #[error("missing attribute {0}")]
    Missing(String),
    /// Encapsulated Document holds a value that is not bytes or fragments.
    #[error("Encapsulated Document has no extractable bytes")]
    NoDocumentBytes,
}
