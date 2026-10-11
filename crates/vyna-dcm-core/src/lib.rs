//! `vyna-dcm-core`: the VynaDCM data model.
//!
//! Dependency-light foundation shared by every other crate:
//!
//! - [`Tag`] — `(GGGG,EEEE)` attribute identifiers
//! - [`Vr`] — the 35 Value Representations (incl. retired `SI`)
//! - [`DataElement`] / [`Value`] / [`DataSet`] — the in-memory dataset
//! - [`dictionary`] — curated PS3.6 public dictionary + [`tags`] constants
//! - [`TransferSyntax`] — transfer syntax UIDs and wire properties
//! - [`uid`] — SOP Class UIDs, validation, UID generation
//! - [`PersonName`], [`Da`], [`Tm`] — structured text values
//!
//! # Example
//!
//! ```
//! use vyna_dcm_core::{tags, DataSet, Vr};
//!
//! let mut ds = DataSet::new();
//! ds.put_uid(tags::SOP_INSTANCE_UID, "1.2.3.4");
//! ds.put_text(tags::PATIENT_NAME, Vr::Pn, "Doe^Jane");
//! assert_eq!(ds.string(tags::PATIENT_NAME), Some("Doe^Jane"));
//! ```

#![warn(missing_docs)]

mod dataset;
mod datetime;
mod dictionary;
mod dictionary_generated;
mod error;
mod person_name;
mod private_generated;
mod tag;
mod transfer_syntax;
mod uid;
mod value;
mod vr;

pub use dataset::DataSet;
pub use datetime::{Da, Tm, is_valid_dt};
pub use dictionary::{
    DictEntry, ENTRIES, dictionary_vr, is_context_dependent_vr, lookup, lookup_keyword,
};
pub use error::{Error, Result};
pub use person_name::{NameComponents, PersonName};
pub use private_generated::{PrivateEntry, lookup_private};
pub use tag::{Tag, delimiter};
pub use transfer_syntax::TransferSyntax;
pub use uid::{
    APPLICATION_CONTEXT_NAME, VYNA_IMPLEMENTATION_CLASS_UID, VYNA_IMPLEMENTATION_VERSION_NAME,
    check_uid, is_valid_uid, new_uid,
};
pub use value::{DataElement, FragmentSequence, Value};
pub use vr::Vr;

/// Well-known tags; see [`dictionary::tags`].
pub use dictionary::tags;
/// SOP Class UIDs; see [`uid::sop_class`].
pub use uid::sop_class;
/// Well-known framing UIDs; see [`uid::well_known`].
pub use uid::well_known;
