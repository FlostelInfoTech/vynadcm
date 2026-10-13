//! De-identification errors.

/// De-identification failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Burned-in annotation is present: pixel data may embed PHI and this
    /// crate never alters pixels, so de-identification is refused.
    #[error("refusing to de-identify: Burned In Annotation is YES")]
    BurnedInAnnotation,
    /// Replacement name is not a valid single `PN` value (overlong
    /// group, too many components/groups, or backslash).
    #[error("invalid replacement name: {0}")]
    InvalidReplacementName(String),
}

/// De-identification result.
pub type Result<T> = std::result::Result<T, Error>;
