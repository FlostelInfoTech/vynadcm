//! DICOM audit messages (PS3.15 Annex A.5 / RFC 3881).
//!
//! Builders emit schema-shaped XML for the events this workspace
//! audits; [`AuditMessage::to_xml`] escapes every interpolated value.
//! Event/role code triples follow PS3.15 and the DCMR and are pinned
//! by tests — report any discrepancy against your ARR.

/// A coded value triple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    /// Code value (e.g. `110104`).
    pub code: String,
    /// Coding scheme (`DCM`, `RFC-3881`).
    pub system: String,
    /// Display name.
    pub display: String,
}

impl Code {
    /// Build a triple.
    pub fn new(
        code: impl Into<String>,
        system: impl Into<String>,
        display: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            system: system.into(),
            display: display.into(),
        }
    }

    /// DCM-coded triple.
    pub fn dcm(code: impl Into<String>, display: impl Into<String>) -> Self {
        Self::new(code, "DCM", display)
    }
}

/// Event action codes (RFC 3881 `EventActionCode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionCode {
    /// Create.
    Create,
    /// Read.
    Read,
    /// Update.
    Update,
    /// Delete.
    Delete,
    /// Execute.
    Execute,
}

impl ActionCode {
    fn letter(self) -> char {
        match self {
            ActionCode::Create => 'C',
            ActionCode::Read => 'R',
            ActionCode::Update => 'U',
            ActionCode::Delete => 'D',
            ActionCode::Execute => 'E',
        }
    }
}

/// Event outcome indicators (RFC 3881).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Full success.
    Success,
    /// Minor failure (recovered).
    MinorFailure,
    /// Serious failure.
    SeriousFailure,
    /// Major failure (unavailable).
    MajorFailure,
}

impl Outcome {
    fn code(self) -> u8 {
        match self {
            Outcome::Success => 0,
            Outcome::MinorFailure => 4,
            Outcome::SeriousFailure => 8,
            Outcome::MajorFailure => 12,
        }
    }
}

/// One active participant (person, process, or node).
#[derive(Clone, Debug)]
pub struct Participant {
    /// User ID (AE title, username, process name).
    pub user_id: String,
    /// Alternative ID (e.g. process ID).
    pub alt_user_id: Option<String>,
    /// True when this participant requested the action.
    pub is_requestor: bool,
    /// Role (application / source / destination).
    pub role: Code,
    /// Network access point: IP address or host name. Rendered with
    /// type code 2 (IP) when it parses as one, else 1 (name).
    pub network_id: Option<String>,
}

impl Participant {
    /// Application participant (role 110150).
    pub fn application(user_id: impl Into<String>, requestor: bool) -> Self {
        Self {
            user_id: user_id.into(),
            alt_user_id: None,
            is_requestor: requestor,
            role: Code::dcm("110150", "Application"),
            network_id: None,
        }
    }

    /// Source participant (role 110153, "Source Role ID" per PS3.16
    /// Annex D / IHE ITI TF convention).
    pub fn source(user_id: impl Into<String>, network_id: Option<String>, requestor: bool) -> Self {
        Self {
            user_id: user_id.into(),
            alt_user_id: None,
            is_requestor: requestor,
            role: Code::dcm("110153", "Source Role ID"),
            network_id,
        }
    }

    /// Destination participant (role 110152, "Destination Role ID" per
    /// PS3.16 Annex D / IHE ITI TF convention).
    pub fn destination(
        user_id: impl Into<String>,
        network_id: Option<String>,
        requestor: bool,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            alt_user_id: None,
            is_requestor: requestor,
            role: Code::dcm("110152", "Destination Role ID"),
            network_id,
        }
    }

    fn network_type_code(id: &str) -> u8 {
        if id.parse::<std::net::IpAddr>().is_ok() {
            2
        } else {
            1
        }
    }
}

/// One participant object (patient, study, ...).
#[derive(Clone, Debug)]
pub struct ParticipantObject {
    /// Object type code (1 person, 2 system, 3 organization, 4 other).
    pub type_code: u8,
    /// Type code role (1 patient, 20 job, ...).
    pub role: u8,
    /// ID type triple.
    pub id_type: Code,
    /// Object ID (patient ID, study UID, ...).
    pub id: String,
    /// `(type, raw bytes)` details; values render base64-encoded.
    pub details: Vec<(String, Vec<u8>)>,
}

impl ParticipantObject {
    /// Patient object: person/patient with an RFC-3881 patient number.
    pub fn patient(patient_id: impl Into<String>) -> Self {
        Self {
            type_code: 1,
            role: 1,
            id_type: Code::new("2", "RFC-3881", "Patient Number"),
            id: patient_id.into(),
            details: Vec::new(),
        }
    }

    /// Study object: system/job keyed by Study Instance UID, with one
    /// `(SOP Class UID, SOP Instance UID)` detail pair per instance.
    pub fn study(study_uid: impl Into<String>, instances: &[(&str, &str)]) -> Self {
        let mut details = Vec::with_capacity(instances.len() * 2);
        for (class_uid, inst_uid) in instances {
            details.push(("SOP Class UID".to_string(), class_uid.as_bytes().to_vec()));
            details.push(("SOP Instance UID".to_string(), inst_uid.as_bytes().to_vec()));
        }
        Self {
            type_code: 2,
            role: 20,
            id_type: Code::dcm("110180", "Study Instance UID"),
            id: study_uid.into(),
            details,
        }
    }
}

/// A complete audit message.
#[derive(Clone, Debug)]
pub struct AuditMessage {
    /// Event ID triple.
    pub event: Code,
    /// Event type triple, when the event splits further (e.g. IHE
    /// Application Start/Stop refines DCM 110100).
    pub event_type: Option<Code>,
    /// Event action.
    pub action: ActionCode,
    /// Event outcome.
    pub outcome: Outcome,
    /// Event timestamp (RFC 3339); defaults to now in builders.
    pub timestamp: String,
    /// Active participants.
    pub participants: Vec<Participant>,
    /// Audit source ID (sending process / node).
    pub source_id: String,
    /// Audit source type code (4 = application server process).
    pub source_code: u8,
    /// Participant objects.
    pub objects: Vec<ParticipantObject>,
}

impl AuditMessage {
    /// Minimal message; participants/objects default empty, outcome
    /// success, source code 4.
    pub fn new(event: Code, action: ActionCode, source_id: impl Into<String>) -> Self {
        Self {
            event,
            event_type: None,
            action,
            outcome: Outcome::Success,
            timestamp: vyna_dcm_core::utc_now_rfc3339(),
            participants: Vec::new(),
            source_id: source_id.into(),
            source_code: 4,
            objects: Vec::new(),
        }
    }

    /// Application Activity start/stop (DCM 110100, Execute), refined
    /// by the IHE EventTypeCode (DCM 110120 start / 110121 stop).
    pub fn actor_activity(
        app: &str,
        network_id: Option<&str>,
        source_id: &str,
        started: bool,
    ) -> Self {
        let mut participant = Participant::application(app, true);
        participant.network_id = network_id.map(str::to_string);
        let mut msg = Self::new(
            Code::dcm("110100", "Application Activity"),
            ActionCode::Execute,
            source_id,
        );
        msg.event_type = Some(if started {
            Code::dcm("110120", "Application Start")
        } else {
            Code::dcm("110121", "Application Stop")
        });
        msg.participants.push(participant);
        msg
    }

    /// Instances transferred (DCM 110104, Read): `instances` are
    /// `(sop_class_uid, sop_instance_uid)` pairs of one study.
    #[allow(clippy::too_many_arguments)]
    pub fn instances_transferred(
        patient_id: Option<&str>,
        study_uid: &str,
        instances: &[(&str, &str)],
        source_ae: &str,
        source_ip: Option<&str>,
        dest_ae: &str,
        dest_ip: Option<&str>,
        source_id: &str,
    ) -> Self {
        let mut msg = Self::new(
            Code::dcm("110104", "DICOM Instances Transferred"),
            ActionCode::Read,
            source_id,
        );
        msg.participants.push(Participant::source(
            source_ae,
            source_ip.map(str::to_string),
            true,
        ));
        msg.participants.push(Participant::destination(
            dest_ae,
            dest_ip.map(str::to_string),
            false,
        ));
        if let Some(patient) = patient_id {
            msg.objects.push(ParticipantObject::patient(patient));
        }
        msg.objects
            .push(ParticipantObject::study(study_uid, instances));
        msg
    }

    /// Begin transferring instances (DCM 110102, Execute); same shape
    /// as [`AuditMessage::instances_transferred`].
    #[allow(clippy::too_many_arguments)]
    pub fn begin_transferring(
        patient_id: Option<&str>,
        study_uid: &str,
        instances: &[(&str, &str)],
        source_ae: &str,
        source_ip: Option<&str>,
        dest_ae: &str,
        dest_ip: Option<&str>,
        source_id: &str,
    ) -> Self {
        let mut msg = Self::instances_transferred(
            patient_id, study_uid, instances, source_ae, source_ip, dest_ae, dest_ip, source_id,
        );
        msg.event = Code::dcm("110102", "Begin Transferring DICOM Instances");
        msg.action = ActionCode::Execute;
        msg
    }

    /// Study deleted (DCM 110105, Delete).
    pub fn study_deleted(
        patient_id: Option<&str>,
        study_uid: &str,
        app: &str,
        source_id: &str,
    ) -> Self {
        let mut msg = Self::new(
            Code::dcm("110105", "DICOM Study Deleted"),
            ActionCode::Delete,
            source_id,
        );
        msg.participants.push(Participant::application(app, true));
        if let Some(patient) = patient_id {
            msg.objects.push(ParticipantObject::patient(patient));
        }
        msg.objects.push(ParticipantObject::study(study_uid, &[]));
        msg
    }

    /// Export (DCM 110106, Read) / Import (DCM 110107, Create) of one
    /// study; `export` selects which.
    #[allow(clippy::too_many_arguments)]
    pub fn export_import(
        export: bool,
        patient_id: Option<&str>,
        study_uid: &str,
        instances: &[(&str, &str)],
        source_ae: &str,
        source_ip: Option<&str>,
        dest_ae: &str,
        dest_ip: Option<&str>,
        source_id: &str,
    ) -> Self {
        let mut msg = Self::instances_transferred(
            patient_id, study_uid, instances, source_ae, source_ip, dest_ae, dest_ip, source_id,
        );
        (msg.event, msg.action) = if export {
            (Code::dcm("110106", "Export"), ActionCode::Read)
        } else {
            (Code::dcm("110107", "Import"), ActionCode::Create)
        };
        msg
    }

    /// Query (DCM 110112, Execute) against a peer.
    #[allow(clippy::too_many_arguments)]
    pub fn query(
        patient_id: Option<&str>,
        study_uid: Option<&str>,
        source_ae: &str,
        source_ip: Option<&str>,
        dest_ae: &str,
        dest_ip: Option<&str>,
        source_id: &str,
    ) -> Self {
        let mut msg = Self::new(Code::dcm("110112", "Query"), ActionCode::Execute, source_id);
        msg.participants.push(Participant::source(
            source_ae,
            source_ip.map(str::to_string),
            true,
        ));
        msg.participants.push(Participant::destination(
            dest_ae,
            dest_ip.map(str::to_string),
            false,
        ));
        if let Some(patient) = patient_id {
            msg.objects.push(ParticipantObject::patient(patient));
        }
        if let Some(study) = study_uid {
            msg.objects.push(ParticipantObject::study(study, &[]));
        }
        msg
    }

    /// Render RFC 3881 XML; every interpolated value is escaped.
    #[must_use]
    pub fn to_xml(&self) -> String {
        use base64::Engine as _;
        let mut out = String::from(
            "<AuditMessage xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xsi:noNamespaceSchemaLocation=\"healthcare-security-audit.xsd\">\n",
        );
        out.push_str(&format!(
            "<EventIdentification EventActionCode=\"{}\" EventDateTime=\"{}\" EventOutcomeIndicator=\"{}\">\n",
            self.action.letter(),
            escape(&self.timestamp),
            self.outcome.code()
        ));
        out.push_str(&format!("{}\n", code_xml("EventID", &self.event)));
        if let Some(event_type) = &self.event_type {
            out.push_str(&format!("{}\n", code_xml("EventTypeCode", event_type)));
        }
        out.push_str("</EventIdentification>\n");
        for p in &self.participants {
            out.push_str(&format!(
                "<ActiveParticipant UserID=\"{}\"{} UserIsRequestor=\"{}\"{}>\n",
                escape(&p.user_id),
                p.alt_user_id
                    .as_ref()
                    .map(|a| format!(" AlternativeUserID=\"{}\"", escape(a)))
                    .unwrap_or_default(),
                p.is_requestor,
                p.network_id
                    .as_ref()
                    .map(|n| format!(
                        " NetworkAccessPointTypeCode=\"{}\" NetworkAccessPointID=\"{}\"",
                        Participant::network_type_code(n),
                        escape(n)
                    ))
                    .unwrap_or_default()
            ));
            out.push_str(&format!("{}\n", code_xml("RoleIDCode", &p.role)));
            out.push_str("</ActiveParticipant>\n");
        }
        out.push_str(&format!(
            "<AuditSourceIdentification AuditSourceID=\"{}\">\n{}\n</AuditSourceIdentification>\n",
            escape(&self.source_id),
            code_xml(
                "AuditSourceTypeCode",
                &Code::new(
                    self.source_code.to_string(),
                    "RFC-3881",
                    audit_source_type_display(self.source_code),
                ),
            ),
        ));
        for o in &self.objects {
            out.push_str(&format!(
                "<ParticipantObjectIdentification ParticipantObjectTypeCode=\"{}\" \
                 ParticipantObjectTypeCodeRole=\"{}\" ParticipantObjectID=\"{}\">\n",
                o.type_code,
                o.role,
                escape(&o.id)
            ));
            out.push_str(&format!(
                "{}\n",
                code_xml("ParticipantObjectIDTypeCode", &o.id_type)
            ));
            for (detail_type, value) in &o.details {
                out.push_str(&format!(
                    "<ParticipantObjectDetail type=\"{}\" value=\"{}\"/>\n",
                    escape(detail_type),
                    base64::engine::general_purpose::STANDARD.encode(value)
                ));
            }
            out.push_str("</ParticipantObjectIdentification>\n");
        }
        out.push_str("</AuditMessage>");
        out
    }
}

/// RFC 3881 audit-source-type display names.
fn audit_source_type_display(code: u8) -> &'static str {
    match code {
        1 => "End-user display device",
        2 => "Data acquisition device",
        3 => "Web server process",
        4 => "Application server process",
        5 => "Database server process",
        6 => "Security server",
        7 => "Network component",
        8 => "Operating software",
        9 => "External source",
        _ => "Unknown audit source type",
    }
}

fn code_xml(element: &str, code: &Code) -> String {
    format!(
        "<{element} code=\"{}\" codeSystemName=\"{}\" displayName=\"{}\"/>",
        escape(&code.code),
        escape(&code.system),
        escape(&code.display)
    )
}

/// Escape XML text/attribute content.
///
/// Control characters illegal in XML 1.0 become U+FFFD (AE titles
/// and hostnames arrive from the wire and may carry anything).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\u{09}' | '\u{0A}' | '\u{0D}' => out.push(c),
            '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}' => {
                out.push(c);
            }
            _ => out.push('\u{FFFD}'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_code_triples_are_pinned() {
        let t =
            AuditMessage::instances_transferred(None, "1.2.3", &[], "A", None, "B", None, "SRC");
        assert_eq!(
            (t.event.code.as_str(), t.event.display.as_str(), t.action),
            ("110104", "DICOM Instances Transferred", ActionCode::Read)
        );
        let b = AuditMessage::begin_transferring(None, "1.2.3", &[], "A", None, "B", None, "SRC");
        assert_eq!(
            (b.event.code.as_str(), b.action),
            ("110102", ActionCode::Execute)
        );
        let q = AuditMessage::query(None, None, "A", None, "B", None, "SRC");
        assert_eq!(
            (q.event.code.as_str(), q.action),
            ("110112", ActionCode::Execute)
        );
        let d = AuditMessage::study_deleted(None, "1.2.3", "APP", "SRC");
        assert_eq!(
            (d.event.code.as_str(), d.action),
            ("110105", ActionCode::Delete)
        );
        let e = AuditMessage::export_import(true, None, "1.2.3", &[], "A", None, "B", None, "SRC");
        assert_eq!(
            (e.event.code.as_str(), e.action),
            ("110106", ActionCode::Read)
        );
        let i = AuditMessage::export_import(false, None, "1.2.3", &[], "A", None, "B", None, "SRC");
        assert_eq!(
            (i.event.code.as_str(), i.action),
            ("110107", ActionCode::Create)
        );
        let a = AuditMessage::actor_activity("APP", None, "SRC", true);
        assert_eq!(
            (a.event.code.as_str(), a.action),
            ("110100", ActionCode::Execute)
        );
    }

    #[test]
    fn actor_activity_start_stop_are_distinguishable() {
        let start = AuditMessage::actor_activity("APP", None, "SRC", true);
        let stop = AuditMessage::actor_activity("APP", None, "SRC", false);
        let start_type = start.event_type.as_ref().unwrap();
        let stop_type = stop.event_type.as_ref().unwrap();
        assert_eq!(
            (start_type.code.as_str(), start_type.display.as_str()),
            ("110120", "Application Start")
        );
        assert_eq!(
            (stop_type.code.as_str(), stop_type.display.as_str()),
            ("110121", "Application Stop")
        );
        let xml = start.to_xml();
        assert!(
            xml.contains(
                "<EventTypeCode code=\"110120\" codeSystemName=\"DCM\" \
                 displayName=\"Application Start\"/>"
            ),
            "{xml}"
        );
        // Other builders carry no EventTypeCode.
        let plain = AuditMessage::study_deleted(None, "1.2.3", "APP", "SRC");
        assert!(plain.event_type.is_none());
        assert!(!plain.to_xml().contains("EventTypeCode"));
    }

    #[test]
    fn audit_source_renders_type_child() {
        let msg =
            AuditMessage::instances_transferred(None, "1.2.3", &[], "A", None, "B", None, "SRC");
        let xml = msg.to_xml();
        // RFC 3881 shape: AuditSourceID attribute plus an
        // AuditSourceTypeCode child — never a bare code attribute.
        assert!(
            xml.contains("<AuditSourceIdentification AuditSourceID=\"SRC\">"),
            "{xml}"
        );
        assert!(
            xml.contains(
                "<AuditSourceTypeCode code=\"4\" codeSystemName=\"RFC-3881\" \
                 displayName=\"Application server process\"/>"
            ),
            "{xml}"
        );
        assert!(!xml.contains("AuditSourceID=\"SRC\" code="), "{xml}");
    }

    #[test]
    fn transferred_xml_has_shape_and_roles() {
        let msg = AuditMessage::instances_transferred(
            Some("E123"),
            "1.2.3",
            &[("1.2.840.10008.5.1.4.1.1.2", "1.2.3.1")],
            "SOURCE_AE",
            Some("10.0.0.2"),
            "DEST_AE",
            Some("dest.example"),
            "VYNA_SCP",
        );
        let xml = msg.to_xml();
        assert!(xml.contains("EventActionCode=\"R\""), "{xml}");
        assert!(xml.contains("EventOutcomeIndicator=\"0\""), "{xml}");
        assert!(xml.contains("code=\"110153\""), "{xml}"); // source
        assert!(xml.contains("code=\"110152\""), "{xml}"); // destination
        assert!(xml.contains("NetworkAccessPointTypeCode=\"2\""), "{xml}"); // IP
        assert!(xml.contains("NetworkAccessPointTypeCode=\"1\""), "{xml}"); // name
        assert!(xml.contains("ParticipantObjectID=\"1.2.3\""), "{xml}");
        assert!(xml.contains("ParticipantObjectID=\"E123\""), "{xml}");
        // Detail values are base64 of the raw UIDs.
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode("1.2.3.1");
        assert!(xml.contains(&format!("value=\"{encoded}\"")), "{xml}");
    }

    #[test]
    fn values_escape() {
        let msg = AuditMessage::instances_transferred(
            Some("E&<123>\"'"),
            "1.2.3",
            &[],
            "A&B",
            None,
            "B",
            None,
            "S\"RC",
        );
        let xml = msg.to_xml();
        assert!(xml.contains("E&amp;&lt;123&gt;&quot;&apos;"), "{xml}");
        assert!(xml.contains("UserID=\"A&amp;B\""), "{xml}");
        assert!(xml.contains("AuditSourceID=\"S&quot;RC\""), "{xml}");
        assert!(!xml.contains("E&<123>"), "{xml}");
    }

    #[test]
    fn nul_and_controls_become_replacement_char() {
        // AE titles arrive from the wire and may carry NUL/controls,
        // which are illegal in XML 1.0 and must not reach the payload.
        let msg = AuditMessage::instances_transferred(
            Some("ID\0\x01\x08\x0B\x0C\x0E\x1F"),
            "1.2.3",
            &[],
            "AE\0TITLE",
            None,
            "B",
            None,
            "SRC",
        );
        let xml = msg.to_xml();
        assert!(!xml.contains('\0'), "{xml}");
        for c in ['\x01', '\x08', '\x0B', '\x0C', '\x0E', '\x1F'] {
            assert!(!xml.contains(c), "{xml}");
        }
        assert!(xml.contains('\u{FFFD}'), "{xml}");
        // Tab/LF/CR are legal XML and survive.
        assert_eq!(escape("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn network_types_classify() {
        assert_eq!(Participant::network_type_code("10.1.2.3"), 2);
        assert_eq!(Participant::network_type_code("::1"), 2);
        assert_eq!(Participant::network_type_code("pacs.example"), 1);
        assert_eq!(Participant::network_type_code("CALLING_AE"), 1);
    }

    #[test]
    fn escape_replaces_illegal_controls() {
        assert_eq!(escape("a\x00b\x07c\x1Fd"), "a\u{FFFD}b\u{FFFD}c\u{FFFD}d");
        assert_eq!(escape("tab\there\nnl\rok"), "tab\there\nnl\rok");
        assert_eq!(escape("王伟"), "王伟");
        assert_eq!(escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
    }
}
