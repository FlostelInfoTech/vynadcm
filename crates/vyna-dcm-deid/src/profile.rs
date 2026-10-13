//! De-identification actions and dummy constants.
//!
//! Action letters follow PS3.15 Annex E semantics; the tag table is a
//! full transcription of Table E.1-1 (see [`BASIC_PROFILE`], generated
//! by `docs/generate-deid-profile.py` — do not hand-edit the table).

pub use crate::profile_generated::BASIC_PROFILE;

/// What to do with one data element.
///
/// Non-exhaustive: further actions may be added as the E.1 transcription
/// and its options grow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Replace with a VR-shaped dummy, never empty (safe for Type 1):
    /// the configured patient name for `PN`, [`DUMMY_DATE`] /
    /// [`DUMMY_DATETIME`] for `DA`/`DT`, [`DUMMY_TIME`] for `TM`,
    /// [`DUMMY_AGE`] for `AS`, a single empty item for sequences, the
    /// replacement-ID bytes for `OB`/`UN`/other binary VRs, a
    /// consistent remap for `UI`, and the replacement ID text
    /// otherwise.
    Dummy,
    /// Empty the value but keep the element (valid for Type 2/3,
    /// including sequences, which encode as zero items).
    Zero,
    /// Remove the element. Used only for Type 3 attributes, stale
    /// provenance, signatures (which de-identification invalidates),
    /// and pre-existing de-identification markers (re-stamped after).
    Remove,
    /// Replace each UID component with a consistent remap: the same
    /// input UID always maps to the same fresh UID within one
    /// [`Deidentifier`](crate::Deidentifier).
    RemapUid,
    /// Shift `DA`/`DT` calendar days by the configured offset
    /// (keeping longitudinal intervals); write the [`DUMMY_DATE`] /
    /// [`DUMMY_DATETIME`] when no offset is set. Never empty, hence
    /// safe for Type 1 dates (e.g. waveform Content Date).
    ShiftDate,
    /// Shift `DA`/`DT` like [`Action::ShiftDate`] when an offset is
    /// configured; empty the value when none is. Only ever targets
    /// Type 2/3 dates (the generator asserts this), so the empty
    /// branch stays valid.
    ShiftDateOrZero,
}

/// Dummy `DA` written when no date offset is configured.
pub const DUMMY_DATE: &str = "19000101";

/// Dummy `DT` written when no date offset is configured.
pub const DUMMY_DATETIME: &str = "19000101000000";

/// Dummy `TM` for [`Action::Dummy`] times (midnight, valid `TM`).
pub const DUMMY_TIME: &str = "000000";

/// Dummy `AS` for [`Action::Dummy`] ages (zero years, valid `AS`).
pub const DUMMY_AGE: &str = "000Y";
