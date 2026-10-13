//! DIMSE commands and statuses (NEMA PS3.7).
//!
//! [`Command`] is the parsed group-`0x0000` command set; [`DimseMessage`]
//! pairs it with an optional dataset. Commands always travel in Implicit
//! VR Little Endian with a leading Command Group Length element.

use vyna_dcm_codec::{decode_dataset, encode_dataset};
use vyna_dcm_core::{DataSet, Tag, TransferSyntax, tags};

use crate::{Error, Result};

/// Command Field (`0000,0100`) values.
pub mod command_field {
    /// C-STORE-RQ.
    pub const C_STORE_RQ: u16 = 0x0001;
    /// C-STORE-RSP.
    pub const C_STORE_RSP: u16 = 0x8001;
    /// C-GET-RQ.
    pub const C_GET_RQ: u16 = 0x0010;
    /// C-GET-RSP.
    pub const C_GET_RSP: u16 = 0x8010;
    /// C-FIND-RQ.
    pub const C_FIND_RQ: u16 = 0x0020;
    /// C-FIND-RSP.
    pub const C_FIND_RSP: u16 = 0x8020;
    /// C-MOVE-RQ.
    pub const C_MOVE_RQ: u16 = 0x0021;
    /// C-MOVE-RSP.
    pub const C_MOVE_RSP: u16 = 0x8021;
    /// C-ECHO-RQ.
    pub const C_ECHO_RQ: u16 = 0x0030;
    /// C-ECHO-RSP.
    pub const C_ECHO_RSP: u16 = 0x8030;
    /// N-EVENT-REPORT-RQ.
    pub const N_EVENT_REPORT_RQ: u16 = 0x0100;
    /// N-EVENT-REPORT-RSP.
    pub const N_EVENT_REPORT_RSP: u16 = 0x8100;
    /// N-GET-RQ.
    pub const N_GET_RQ: u16 = 0x0110;
    /// N-GET-RSP.
    pub const N_GET_RSP: u16 = 0x8110;
    /// N-SET-RQ.
    pub const N_SET_RQ: u16 = 0x0120;
    /// N-SET-RSP.
    pub const N_SET_RSP: u16 = 0x8120;
    /// N-ACTION-RQ.
    pub const N_ACTION_RQ: u16 = 0x0130;
    /// N-ACTION-RSP.
    pub const N_ACTION_RSP: u16 = 0x8130;
    /// N-CREATE-RQ.
    pub const N_CREATE_RQ: u16 = 0x0140;
    /// N-CREATE-RSP.
    pub const N_CREATE_RSP: u16 = 0x8140;
    /// N-DELETE-RQ.
    pub const N_DELETE_RQ: u16 = 0x0150;
    /// N-DELETE-RSP.
    pub const N_DELETE_RSP: u16 = 0x8150;
    /// C-CANCEL-RQ.
    pub const C_CANCEL_RQ: u16 = 0x0FFF;
}

/// DIMSE status codes (PS3.7 Annex C).
pub mod status {
    /// Success.
    pub const SUCCESS: u16 = 0x0000;
    /// Cancel.
    pub const CANCEL: u16 = 0xFE00;
    /// Pending: operation continuing.
    pub const PENDING: u16 = 0xFF00;
    /// Pending with warning.
    pub const PENDING_WARNING: u16 = 0xFF01;
    /// Warning: coercion of data elements.
    pub const COERCION: u16 = 0xB000;
    /// Warning: elements discarded.
    pub const ELEMENTS_DISCARDED: u16 = 0xB006;
    /// Warning: dataset does not match SOP class.
    pub const DATASET_MISMATCH_WARNING: u16 = 0xB007;
    /// Failure: processing failure.
    pub const PROCESSING_FAILURE: u16 = 0x0110;
    /// Failure: duplicate SOP instance.
    pub const DUPLICATE_SOP_INSTANCE: u16 = 0x0111;
    /// Failure: no such SOP instance.
    pub const NO_SUCH_SOP_INSTANCE: u16 = 0x0112;
    /// Failure: SOP class not supported.
    pub const SOP_CLASS_NOT_SUPPORTED: u16 = 0x0122;
    /// Failure: duplicate invocation.
    pub const DUPLICATE_INVOCATION: u16 = 0x0210;
    /// Failure: unrecognised operation.
    pub const UNRECOGNIZED_OPERATION: u16 = 0x0211;
    /// Failure: mistyped argument.
    pub const MISTYPED_ARGUMENT: u16 = 0x0212;
    /// Failure: resource limitation.
    pub const RESOURCE_LIMITATION: u16 = 0x0213;
    /// Failure: refused, out of resources.
    pub const OUT_OF_RESOURCES: u16 = 0xA700;
    /// Failure (C-MOVE/C-GET): out of resources, unable to calculate
    /// the number of matches.
    pub const UNABLE_TO_CALCULATE_MATCHES: u16 = 0xA701;
    /// Failure: dataset does not match SOP class.
    pub const DATASET_MISMATCH: u16 = 0xA900;
    /// Failure: move destination unknown.
    pub const MOVE_DESTINATION_UNKNOWN: u16 = 0xA801;

    /// `0x0000`.
    #[must_use]
    pub const fn is_success(s: u16) -> bool {
        s == SUCCESS
    }

    /// `0x0001` or `0xB000..=0xBFFF`.
    #[must_use]
    pub const fn is_warning(s: u16) -> bool {
        s == 0x0001 || (s >= 0xB000 && s <= 0xBFFF)
    }

    /// `0xFF00` / `0xFF01`.
    #[must_use]
    pub const fn is_pending(s: u16) -> bool {
        s == PENDING || s == PENDING_WARNING
    }

    /// `0xFE00`.
    #[must_use]
    pub const fn is_cancel(s: u16) -> bool {
        s == CANCEL
    }

    /// Anything that is not success, warning, pending or cancel.
    #[must_use]
    pub const fn is_failure(s: u16) -> bool {
        !(is_success(s) || is_warning(s) || is_pending(s) || is_cancel(s))
    }
}

/// DIMSE priority levels (`0000,0700`).
pub mod priority {
    /// Medium (default).
    pub const MEDIUM: u16 = 0;
    /// High.
    pub const HIGH: u16 = 1;
    /// Low.
    pub const LOW: u16 = 2;
}

/// Command Data Set Type: null = no dataset follows.
pub const DATA_SET_NULL: u16 = 0x0101;
/// Command Data Set Type: any other value = dataset follows.
pub const DATA_SET_PRESENT: u16 = 0x0001;

/// A parsed DIMSE command set (group `0x0000`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// Command field (`0000,0100`).
    pub field: u16,
    /// Message ID (`0000,0110`), requests only.
    pub message_id: Option<u16>,
    /// Message ID Being Responded To (`0000,0120`), responses/cancel.
    pub message_id_rsp: Option<u16>,
    /// Affected SOP Class UID (`0000,0002`).
    pub affected_sop_class_uid: Option<String>,
    /// Affected SOP Instance UID (`0000,1000`).
    pub affected_sop_instance_uid: Option<String>,
    /// Requested SOP Class UID (`0000,0003`), N-GET only.
    pub requested_sop_class_uid: Option<String>,
    /// Requested SOP Instance UID (`0000,1001`), N-GET only.
    pub requested_sop_instance_uid: Option<String>,
    /// Move Destination AE (`0000,0600`).
    pub move_destination: Option<String>,
    /// Priority (`0000,0700`).
    pub priority: u16,
    /// Command Data Set Type (`0000,0800`).
    pub data_set_type: u16,
    /// Status (`0000,0900`), responses only.
    pub status: Option<u16>,
    /// Error Comment (`0000,0902`).
    pub error_comment: Option<String>,
    /// Event Type ID (`0000,1002`).
    pub event_type_id: Option<u16>,
    /// Action Type ID (`0000,1008`).
    pub action_type_id: Option<u16>,
    /// Attribute Identifier List (`0000,1005`).
    pub attribute_identifiers: Vec<Tag>,
    /// Sub-operation counts (`0000,1020..1023`).
    pub remaining: Option<u16>,
    /// Completed sub-operations.
    pub completed: Option<u16>,
    /// Failed sub-operations.
    pub failed: Option<u16>,
    /// Warning sub-operations.
    pub warning: Option<u16>,
    /// Move Originator AE (`0000,1030`).
    pub move_originator_ae: Option<String>,
    /// Move Originator Message ID (`0000,1031`).
    pub move_originator_id: Option<u16>,
}

impl Command {
    /// Parse a decoded command dataset.
    pub fn from_dataset(ds: &DataSet) -> Result<Self> {
        let req_u16 = |tag| {
            ds.uint16(tag)
                .ok_or_else(|| Error::BadCommand(format!("missing {tag}")))
        };
        let opt_str = |tag| ds.string(tag).map(|s| s.trim_end_matches('\0').to_string());
        Ok(Self {
            field: req_u16(tags::COMMAND_FIELD)?,
            message_id: ds.uint16(tags::MESSAGE_ID),
            message_id_rsp: ds.uint16(tags::MESSAGE_ID_BEING_RESPONDED_TO),
            affected_sop_class_uid: opt_str(tags::AFFECTED_SOP_CLASS_UID),
            affected_sop_instance_uid: opt_str(tags::AFFECTED_SOP_INSTANCE_UID),
            requested_sop_class_uid: opt_str(tags::REQUESTED_SOP_CLASS_UID),
            requested_sop_instance_uid: opt_str(tags::REQUESTED_SOP_INSTANCE_UID),
            move_destination: ds
                .string(tags::MOVE_DESTINATION)
                .map(|s| s.trim().to_string()),
            priority: ds.uint16(tags::PRIORITY).unwrap_or(priority::MEDIUM),
            data_set_type: ds
                .uint16(tags::COMMAND_DATA_SET_TYPE)
                .unwrap_or(DATA_SET_NULL),
            status: ds.uint16(tags::STATUS),
            error_comment: opt_str(tags::ERROR_COMMENT),
            event_type_id: ds.uint16(tags::EVENT_TYPE_ID),
            action_type_id: ds.uint16(tags::ACTION_TYPE_ID),
            attribute_identifiers: match ds.get(tags::ATTRIBUTE_IDENTIFIER_LIST) {
                Some(el) => match &el.value {
                    vyna_dcm_core::Value::Tags(t) => t.clone(),
                    _ => {
                        return Err(Error::BadCommand("AttributeIdentifierList not AT".into()));
                    }
                },
                None => Vec::new(),
            },
            remaining: ds.uint16(tags::NUMBER_OF_REMAINING_SUBOPERATIONS),
            completed: ds.uint16(tags::NUMBER_OF_COMPLETED_SUBOPERATIONS),
            failed: ds.uint16(tags::NUMBER_OF_FAILED_SUBOPERATIONS),
            warning: ds.uint16(tags::NUMBER_OF_WARNING_SUBOPERATIONS),
            move_originator_ae: ds
                .string(tags::MOVE_ORIGINATOR_APPLICATION_ENTITY_TITLE)
                .map(|s| s.trim().to_string()),
            move_originator_id: ds.uint16(tags::MOVE_ORIGINATOR_MESSAGE_ID),
        })
    }

    /// Build the command dataset (without group length; see [`encode_command`]).
    pub fn to_dataset(&self) -> DataSet {
        use vyna_dcm_core::{DataElement, Vr};
        let mut ds = DataSet::new();
        ds.put_u16_single(tags::COMMAND_FIELD, self.field);
        if let Some(id) = self.message_id {
            ds.put_u16_single(tags::MESSAGE_ID, id);
        }
        if let Some(id) = self.message_id_rsp {
            ds.put_u16_single(tags::MESSAGE_ID_BEING_RESPONDED_TO, id);
        }
        if let Some(u) = &self.affected_sop_class_uid {
            ds.put_uid(tags::AFFECTED_SOP_CLASS_UID, u.clone());
        }
        if let Some(u) = &self.affected_sop_instance_uid {
            ds.put_uid(tags::AFFECTED_SOP_INSTANCE_UID, u.clone());
        }
        if let Some(u) = &self.requested_sop_class_uid {
            ds.put_uid(tags::REQUESTED_SOP_CLASS_UID, u.clone());
        }
        if let Some(u) = &self.requested_sop_instance_uid {
            ds.put_uid(tags::REQUESTED_SOP_INSTANCE_UID, u.clone());
        }
        if let Some(d) = &self.move_destination {
            ds.put_text(tags::MOVE_DESTINATION, Vr::Ae, d.clone());
        }
        ds.put_u16_single(tags::PRIORITY, self.priority);
        ds.put_u16_single(tags::COMMAND_DATA_SET_TYPE, self.data_set_type);
        if let Some(s) = self.status {
            ds.put_u16_single(tags::STATUS, s);
        }
        if let Some(c) = &self.error_comment {
            ds.put_text(tags::ERROR_COMMENT, Vr::Lo, c.clone());
        }
        if let Some(e) = self.event_type_id {
            ds.put_u16_single(tags::EVENT_TYPE_ID, e);
        }
        if let Some(a) = self.action_type_id {
            ds.put_u16_single(tags::ACTION_TYPE_ID, a);
        }
        if !self.attribute_identifiers.is_empty() {
            ds.insert(DataElement::tags(
                tags::ATTRIBUTE_IDENTIFIER_LIST,
                self.attribute_identifiers.clone(),
            ));
        }
        if let Some(v) = self.remaining {
            ds.put_u16_single(tags::NUMBER_OF_REMAINING_SUBOPERATIONS, v);
        }
        if let Some(v) = self.completed {
            ds.put_u16_single(tags::NUMBER_OF_COMPLETED_SUBOPERATIONS, v);
        }
        if let Some(v) = self.failed {
            ds.put_u16_single(tags::NUMBER_OF_FAILED_SUBOPERATIONS, v);
        }
        if let Some(v) = self.warning {
            ds.put_u16_single(tags::NUMBER_OF_WARNING_SUBOPERATIONS, v);
        }
        if let Some(a) = &self.move_originator_ae {
            ds.put_text(
                tags::MOVE_ORIGINATOR_APPLICATION_ENTITY_TITLE,
                Vr::Ae,
                a.clone(),
            );
        }
        if let Some(id) = self.move_originator_id {
            ds.put_u16_single(tags::MOVE_ORIGINATOR_MESSAGE_ID, id);
        }
        ds
    }

    /// True when a dataset follows the command.
    #[must_use]
    pub fn has_dataset(&self) -> bool {
        self.data_set_type != DATA_SET_NULL
    }

    /// True for `*-RSP` and C-CANCEL-RQ (anything carrying `message_id_rsp`).
    #[must_use]
    pub fn is_response(&self) -> bool {
        self.message_id_rsp.is_some()
    }

    /// Short name, e.g. `"C-ECHO-RQ"`.
    #[must_use]
    pub fn name(&self) -> &'static str {
        use command_field::*;
        match self.field {
            C_STORE_RQ => "C-STORE-RQ",
            C_STORE_RSP => "C-STORE-RSP",
            C_GET_RQ => "C-GET-RQ",
            C_GET_RSP => "C-GET-RSP",
            C_FIND_RQ => "C-FIND-RQ",
            C_FIND_RSP => "C-FIND-RSP",
            C_MOVE_RQ => "C-MOVE-RQ",
            C_MOVE_RSP => "C-MOVE-RSP",
            C_ECHO_RQ => "C-ECHO-RQ",
            C_ECHO_RSP => "C-ECHO-RSP",
            N_EVENT_REPORT_RQ => "N-EVENT-REPORT-RQ",
            N_EVENT_REPORT_RSP => "N-EVENT-REPORT-RSP",
            N_GET_RQ => "N-GET-RQ",
            N_GET_RSP => "N-GET-RSP",
            N_SET_RQ => "N-SET-RQ",
            N_SET_RSP => "N-SET-RSP",
            N_ACTION_RQ => "N-ACTION-RQ",
            N_ACTION_RSP => "N-ACTION-RSP",
            N_CREATE_RQ => "N-CREATE-RQ",
            N_CREATE_RSP => "N-CREATE-RSP",
            N_DELETE_RQ => "N-DELETE-RQ",
            N_DELETE_RSP => "N-DELETE-RSP",
            C_CANCEL_RQ => "C-CANCEL-RQ",
            _ => "UNKNOWN",
        }
    }

    fn base(field: u16) -> Self {
        Self {
            field,
            message_id: None,
            message_id_rsp: None,
            affected_sop_class_uid: None,
            affected_sop_instance_uid: None,
            requested_sop_class_uid: None,
            requested_sop_instance_uid: None,
            move_destination: None,
            priority: priority::MEDIUM,
            data_set_type: DATA_SET_NULL,
            status: None,
            error_comment: None,
            event_type_id: None,
            action_type_id: None,
            attribute_identifiers: Vec::new(),
            remaining: None,
            completed: None,
            failed: None,
            warning: None,
            move_originator_ae: None,
            move_originator_id: None,
        }
    }

    /// C-ECHO-RQ.
    #[must_use]
    pub fn echo_rq(message_id: u16) -> Self {
        let mut c = Self::base(command_field::C_ECHO_RQ);
        c.message_id = Some(message_id);
        c
    }

    /// C-ECHO-RSP.
    #[must_use]
    pub fn echo_rsp(message_id_rsp: u16, status: u16) -> Self {
        let mut c = Self::base(command_field::C_ECHO_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.status = Some(status);
        c
    }

    /// C-STORE-RQ (dataset always follows).
    pub fn store_rq(
        message_id: u16,
        priority: u16,
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
    ) -> Self {
        let mut c = Self::base(command_field::C_STORE_RQ);
        c.message_id = Some(message_id);
        c.priority = priority;
        c.affected_sop_class_uid = Some(sop_class_uid.into());
        c.affected_sop_instance_uid = Some(sop_instance_uid.into());
        c.data_set_type = DATA_SET_PRESENT;
        c
    }

    /// C-STORE-RSP.
    pub fn store_rsp(
        message_id_rsp: u16,
        sop_class_uid: Option<String>,
        sop_instance_uid: Option<String>,
        status: u16,
    ) -> Self {
        let mut c = Self::base(command_field::C_STORE_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.affected_sop_class_uid = sop_class_uid;
        c.affected_sop_instance_uid = sop_instance_uid;
        c.status = Some(status);
        c
    }

    /// C-FIND-RQ (dataset always follows).
    pub fn find_rq(message_id: u16, priority: u16, sop_class_uid: impl Into<String>) -> Self {
        let mut c = Self::base(command_field::C_FIND_RQ);
        c.message_id = Some(message_id);
        c.priority = priority;
        c.affected_sop_class_uid = Some(sop_class_uid.into());
        c.data_set_type = DATA_SET_PRESENT;
        c
    }

    /// C-FIND-RSP; `with_dataset` for pending matches.
    pub fn find_rsp(
        message_id_rsp: u16,
        sop_class_uid: Option<String>,
        status: u16,
        with_dataset: bool,
    ) -> Self {
        let mut c = Self::base(command_field::C_FIND_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.affected_sop_class_uid = sop_class_uid;
        c.status = Some(status);
        c.data_set_type = if with_dataset {
            DATA_SET_PRESENT
        } else {
            DATA_SET_NULL
        };
        c
    }

    /// C-CANCEL-RQ.
    #[must_use]
    pub fn cancel_rq(message_id_rsp: u16) -> Self {
        let mut c = Self::base(command_field::C_CANCEL_RQ);
        c.message_id_rsp = Some(message_id_rsp);
        c
    }

    /// C-MOVE-RQ (dataset always follows).
    pub fn move_rq(
        message_id: u16,
        priority: u16,
        sop_class_uid: impl Into<String>,
        destination: impl Into<String>,
    ) -> Self {
        let mut c = Self::base(command_field::C_MOVE_RQ);
        c.message_id = Some(message_id);
        c.priority = priority;
        c.affected_sop_class_uid = Some(sop_class_uid.into());
        c.move_destination = Some(destination.into());
        c.data_set_type = DATA_SET_PRESENT;
        c
    }

    /// C-MOVE-RSP / C-GET-RSP with sub-operation counts.
    #[allow(clippy::too_many_arguments)]
    pub fn move_get_rsp(
        is_move: bool,
        message_id_rsp: u16,
        sop_class_uid: Option<String>,
        status: u16,
        remaining: u16,
        completed: u16,
        failed: u16,
        warning: u16,
    ) -> Self {
        let mut c = Self::base(if is_move {
            command_field::C_MOVE_RSP
        } else {
            command_field::C_GET_RSP
        });
        c.message_id_rsp = Some(message_id_rsp);
        c.affected_sop_class_uid = sop_class_uid;
        c.status = Some(status);
        c.remaining = Some(remaining);
        c.completed = Some(completed);
        c.failed = Some(failed);
        c.warning = Some(warning);
        c
    }

    /// C-GET-RQ (dataset always follows).
    pub fn get_rq(message_id: u16, priority: u16, sop_class_uid: impl Into<String>) -> Self {
        let mut c = Self::base(command_field::C_GET_RQ);
        c.message_id = Some(message_id);
        c.priority = priority;
        c.affected_sop_class_uid = Some(sop_class_uid.into());
        c.data_set_type = DATA_SET_PRESENT;
        c
    }

    /// N-CREATE-RQ.
    pub fn n_create_rq(
        message_id: u16,
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
        with_dataset: bool,
    ) -> Self {
        let mut c = Self::base(command_field::N_CREATE_RQ);
        c.message_id = Some(message_id);
        c.affected_sop_class_uid = Some(sop_class_uid.into());
        c.affected_sop_instance_uid = Some(sop_instance_uid.into());
        c.data_set_type = if with_dataset {
            DATA_SET_PRESENT
        } else {
            DATA_SET_NULL
        };
        c
    }

    /// N-CREATE-RSP.
    pub fn n_create_rsp(
        message_id_rsp: u16,
        sop_class_uid: Option<String>,
        sop_instance_uid: Option<String>,
        status: u16,
    ) -> Self {
        let mut c = Self::base(command_field::N_CREATE_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.affected_sop_class_uid = sop_class_uid;
        c.affected_sop_instance_uid = sop_instance_uid;
        c.status = Some(status);
        c
    }

    /// N-SET-RQ (dataset always follows).
    pub fn n_set_rq(
        message_id: u16,
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
    ) -> Self {
        let mut c = Self::base(command_field::N_SET_RQ);
        c.message_id = Some(message_id);
        c.affected_sop_class_uid = Some(sop_class_uid.into());
        c.affected_sop_instance_uid = Some(sop_instance_uid.into());
        c.data_set_type = DATA_SET_PRESENT;
        c
    }

    /// N-SET-RSP.
    pub fn n_set_rsp(
        message_id_rsp: u16,
        sop_class_uid: Option<String>,
        sop_instance_uid: Option<String>,
        status: u16,
    ) -> Self {
        let mut c = Self::base(command_field::N_SET_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.affected_sop_class_uid = sop_class_uid;
        c.affected_sop_instance_uid = sop_instance_uid;
        c.status = Some(status);
        c
    }

    /// N-GET-RQ.
    pub fn n_get_rq(
        message_id: u16,
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
        attribute_identifiers: Vec<Tag>,
    ) -> Self {
        let mut c = Self::base(command_field::N_GET_RQ);
        c.message_id = Some(message_id);
        c.requested_sop_class_uid = Some(sop_class_uid.into());
        c.requested_sop_instance_uid = Some(sop_instance_uid.into());
        c.attribute_identifiers = attribute_identifiers;
        c
    }

    /// N-GET-RSP.
    pub fn n_get_rsp(
        message_id_rsp: u16,
        sop_class_uid: Option<String>,
        sop_instance_uid: Option<String>,
        status: u16,
        with_dataset: bool,
    ) -> Self {
        let mut c = Self::base(command_field::N_GET_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.affected_sop_class_uid = sop_class_uid;
        c.affected_sop_instance_uid = sop_instance_uid;
        c.status = Some(status);
        c.data_set_type = if with_dataset {
            DATA_SET_PRESENT
        } else {
            DATA_SET_NULL
        };
        c
    }

    /// N-ACTION-RQ.
    pub fn n_action_rq(
        message_id: u16,
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
        action_type_id: u16,
        with_dataset: bool,
    ) -> Self {
        let mut c = Self::base(command_field::N_ACTION_RQ);
        c.message_id = Some(message_id);
        c.requested_sop_class_uid = Some(sop_class_uid.into());
        c.requested_sop_instance_uid = Some(sop_instance_uid.into());
        c.action_type_id = Some(action_type_id);
        c.data_set_type = if with_dataset {
            DATA_SET_PRESENT
        } else {
            DATA_SET_NULL
        };
        c
    }

    /// N-ACTION-RSP.
    pub fn n_action_rsp(message_id_rsp: u16, status: u16) -> Self {
        let mut c = Self::base(command_field::N_ACTION_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.status = Some(status);
        c
    }

    /// N-DELETE-RQ.
    pub fn n_delete_rq(
        message_id: u16,
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
    ) -> Self {
        let mut c = Self::base(command_field::N_DELETE_RQ);
        c.message_id = Some(message_id);
        c.requested_sop_class_uid = Some(sop_class_uid.into());
        c.requested_sop_instance_uid = Some(sop_instance_uid.into());
        c
    }

    /// N-DELETE-RSP.
    pub fn n_delete_rsp(message_id_rsp: u16, status: u16) -> Self {
        let mut c = Self::base(command_field::N_DELETE_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.status = Some(status);
        c
    }

    /// N-EVENT-REPORT-RQ.
    pub fn n_event_report_rq(
        message_id: u16,
        sop_class_uid: impl Into<String>,
        sop_instance_uid: impl Into<String>,
        event_type_id: u16,
        with_dataset: bool,
    ) -> Self {
        let mut c = Self::base(command_field::N_EVENT_REPORT_RQ);
        c.message_id = Some(message_id);
        c.affected_sop_class_uid = Some(sop_class_uid.into());
        c.affected_sop_instance_uid = Some(sop_instance_uid.into());
        c.event_type_id = Some(event_type_id);
        c.data_set_type = if with_dataset {
            DATA_SET_PRESENT
        } else {
            DATA_SET_NULL
        };
        c
    }

    /// N-EVENT-REPORT-RSP.
    pub fn n_event_report_rsp(message_id_rsp: u16, status: u16) -> Self {
        let mut c = Self::base(command_field::N_EVENT_REPORT_RSP);
        c.message_id_rsp = Some(message_id_rsp);
        c.status = Some(status);
        c
    }
}

/// Encode a command set: Implicit VR LE with leading group length.
pub fn encode_command(cmd: &Command) -> Result<Vec<u8>> {
    let mut body = encode_dataset(&cmd.to_dataset(), TransferSyntax::ImplicitVrLittleEndian)?;
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // (0000,0000)
    out.extend_from_slice(&[0x04, 0x00, 0x00, 0x00]); // UL, len 4
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.append(&mut body);
    Ok(out)
}

/// Decode a command set.
pub fn decode_command(bytes: &[u8]) -> Result<Command> {
    let ds = decode_dataset(bytes, TransferSyntax::ImplicitVrLittleEndian)?;
    Command::from_dataset(&ds)
}

/// One DIMSE message: presentation context + command + optional dataset.
#[derive(Clone, Debug)]
pub struct DimseMessage {
    /// Presentation context ID the message arrived on.
    pub context_id: u8,
    /// Parsed command set.
    pub command: Command,
    /// Accompanying dataset, when `command.has_dataset()`.
    pub dataset: Option<DataSet>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trips() {
        for cmd in [
            Command::echo_rq(7),
            Command::store_rq(1, priority::MEDIUM, "1.2.3", "1.2.3.4"),
            Command::find_rq(2, priority::HIGH, "1.2.840.10008.5.1.4.1.2.2.1"),
            Command::move_rq(3, priority::LOW, "1.2.3", "DEST_AE"),
            Command::n_create_rq(4, "1.2.3", "1.2.3.4", true),
            Command::n_event_report_rq(5, "1.2.3", "1.2.3.4", 1, false),
        ] {
            let bytes = encode_command(&cmd).unwrap();
            // Group length leads.
            assert_eq!(&bytes[0..4], &[0, 0, 0, 0]);
            let back = decode_command(&bytes).unwrap();
            assert_eq!(back, cmd);
        }
    }

    #[test]
    fn status_classes() {
        assert!(status::is_success(0x0000));
        assert!(status::is_warning(0xB000));
        assert!(status::is_pending(0xFF00));
        assert!(status::is_pending(0xFF01));
        assert!(status::is_cancel(0xFE00));
        assert!(status::is_failure(0x0110));
        assert!(status::is_failure(0xA801));
        assert!(!status::is_failure(0x0000));
    }

    #[test]
    fn names_and_flags() {
        let rq = Command::echo_rq(1);
        assert_eq!(rq.name(), "C-ECHO-RQ");
        assert!(!rq.has_dataset() && !rq.is_response());
        let rsp = Command::find_rsp(1, None, status::PENDING, true);
        assert!(rsp.has_dataset() && rsp.is_response());
        assert_eq!(rsp.name(), "C-FIND-RSP");
    }
}
