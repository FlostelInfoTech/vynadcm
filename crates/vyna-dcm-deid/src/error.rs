//! De-identification errors.

/// De-identification failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Burned-in annotation is present: pixel data may embed PHI and this
    /// crate never alters pixels, so de-identification is refused.
    #[error("refusing to de-identify: Burned In Annotation is YES")]
    BurnedInAnnotation,
}

/// De-identification result.
pub type Result<T> = std::result::Result<T, Error>;
