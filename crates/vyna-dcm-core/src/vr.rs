//! Value Representations (VR): the 34 two-letter DICOM data types.
//!
//! See NEMA PS3.5 §6.2. Explicit VR transfer syntaxes encode the VR on the
//! wire; implicit VR transfer syntaxes derive it from the data dictionary.

use std::fmt;
use std::str::FromStr;

use crate::Error;

/// A DICOM Value Representation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[allow(missing_docs)]
pub enum Vr {
    Ae,
    As,
    At,
    Cs,
    Da,
    Ds,
    Dt,
    Fl,
    Fd,
    Is,
    Lo,
    Lt,
    Ob,
    Od,
    Of,
    Ol,
    Ov,
    Ow,
    Pn,
    Sh,
    Si,
    Sl,
    Sq,
    Ss,
    St,
    Tm,
    Uc,
    Ui,
    Ul,
    Un,
    Ur,
    Us,
    Ut,
    Uv,
}

impl Vr {
    /// All 34 VRs in alphabetical order.
    pub const ALL: [Vr; 34] = [
        Vr::Ae,
        Vr::As,
        Vr::At,
        Vr::Cs,
        Vr::Da,
        Vr::Ds,
        Vr::Dt,
        Vr::Fl,
        Vr::Fd,
        Vr::Is,
        Vr::Lo,
        Vr::Lt,
        Vr::Ob,
        Vr::Od,
        Vr::Of,
        Vr::Ol,
        Vr::Ov,
        Vr::Ow,
        Vr::Pn,
        Vr::Sh,
        Vr::Si,
        Vr::Sl,
        Vr::Sq,
        Vr::Ss,
        Vr::St,
        Vr::Tm,
        Vr::Uc,
        Vr::Ui,
        Vr::Ul,
        Vr::Un,
        Vr::Ur,
        Vr::Us,
        Vr::Ut,
        Vr::Uv,
    ];

    /// Two-letter code, e.g. `"PN"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Vr::Ae => "AE",
            Vr::As => "AS",
            Vr::At => "AT",
            Vr::Cs => "CS",
            Vr::Da => "DA",
            Vr::Ds => "DS",
            Vr::Dt => "DT",
            Vr::Fl => "FL",
            Vr::Fd => "FD",
            Vr::Is => "IS",
            Vr::Lo => "LO",
            Vr::Lt => "LT",
            Vr::Ob => "OB",
            Vr::Od => "OD",
            Vr::Of => "OF",
            Vr::Ol => "OL",
            Vr::Ov => "OV",
            Vr::Ow => "OW",
            Vr::Pn => "PN",
            Vr::Sh => "SH",
            Vr::Si => "SI",
            Vr::Sl => "SL",
            Vr::Sq => "SQ",
            Vr::Ss => "SS",
            Vr::St => "ST",
            Vr::Tm => "TM",
            Vr::Uc => "UC",
            Vr::Ui => "UI",
            Vr::Ul => "UL",
            Vr::Un => "UN",
            Vr::Ur => "UR",
            Vr::Us => "US",
            Vr::Ut => "UT",
            Vr::Uv => "UV",
        }
    }

    /// Two-letter code as bytes.
    #[must_use]
    pub const fn code(self) -> [u8; 2] {
        let s = self.as_str();
        [s.as_bytes()[0], s.as_bytes()[1]]
    }

    /// Parse from two raw bytes; `None` for unknown codes.
    #[must_use]
    pub const fn from_bytes(b: [u8; 2]) -> Option<Vr> {
        // `const` compatible linear scan over the 34 codes.
        let mut i = 0;
        while i < Self::ALL.len() {
            let vr = Self::ALL[i];
            let c = vr.code();
            if c[0] == b[0] && c[1] == b[1] {
                return Some(vr);
            }
            i += 1;
        }
        None
    }

    /// Parse from a two-letter code; `None` for unknown codes.
    #[must_use]
    pub fn from_code(s: &str) -> Option<Vr> {
        let b = s.as_bytes();
        if b.len() != 2 {
            return None;
        }
        Self::from_bytes([b[0], b[1]])
    }

    /// Explicit VR form with two reserved bytes plus a 32-bit length
    /// (`OB OD OF OL OV OW SQ UC UR UT UN`), versus the 16-bit length form.
    #[must_use]
    pub const fn uses_long_form(self) -> bool {
        matches!(
            self,
            Vr::Ob
                | Vr::Od
                | Vr::Of
                | Vr::Ol
                | Vr::Ov
                | Vr::Ow
                | Vr::Sq
                | Vr::Uc
                | Vr::Ur
                | Vr::Ut
                | Vr::Un
        )
    }

    /// Character-string VRs (single or multi-valued text).
    #[must_use]
    pub const fn is_text(self) -> bool {
        matches!(
            self,
            Vr::Ae
                | Vr::As
                | Vr::Cs
                | Vr::Da
                | Vr::Ds
                | Vr::Dt
                | Vr::Is
                | Vr::Lo
                | Vr::Lt
                | Vr::Pn
                | Vr::Sh
                | Vr::St
                | Vr::Tm
                | Vr::Uc
                | Vr::Ui
                | Vr::Ur
                | Vr::Ut
        )
    }

    /// Padding byte for odd-length values: NUL for `UI`, space for other
    /// text VRs, zero for binary/numeric VRs.
    #[must_use]
    pub const fn padding_byte(self) -> u8 {
        match self {
            Vr::Ui => 0x00,
            v if v.is_text() => 0x20,
            _ => 0x00,
        }
    }

    /// Size in bytes of one numeric unit; `1` for text/byte VRs, `0` for `SQ`.
    #[must_use]
    pub const fn unit_size(self) -> usize {
        match self {
            Vr::At | Vr::Fl | Vr::Of | Vr::Sl | Vr::Ul => 4,
            Vr::Fd | Vr::Od | Vr::Ov | Vr::Uv => 8,
            Vr::Ow | Vr::Ss | Vr::Us => 2,
            Vr::Ol | Vr::Si => 4,
            Vr::Sq => 0,
            _ => 1,
        }
    }
}

impl fmt::Display for Vr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Vr {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_code(s.trim()).ok_or_else(|| Error::UnknownVr(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_vrs_round_trip() {
        assert_eq!(Vr::ALL.len(), 34);
        for vr in Vr::ALL {
            assert_eq!(Vr::from_bytes(vr.code()), Some(vr));
            assert_eq!(vr.to_string().parse::<Vr>().unwrap(), vr);
        }
        assert_eq!(Vr::from_code("XX"), None);
        assert!("XX".parse::<Vr>().is_err());
    }

    #[test]
    fn long_form_set_matches_ps35() {
        for code in [
            "OB", "OD", "OF", "OL", "OV", "OW", "SQ", "UC", "UR", "UT", "UN",
        ] {
            assert!(code.parse::<Vr>().unwrap().uses_long_form(), "{code}");
        }
        for code in [
            "AE", "AT", "CS", "DA", "DS", "FL", "IS", "LO", "PN", "SH", "US", "UV",
        ] {
            assert!(!code.parse::<Vr>().unwrap().uses_long_form(), "{code}");
        }
    }

    #[test]
    fn padding_rules() {
        assert_eq!(Vr::Ui.padding_byte(), 0x00);
        assert_eq!(Vr::Pn.padding_byte(), 0x20);
        assert_eq!(Vr::Ob.padding_byte(), 0x00);
        assert_eq!(Vr::Us.padding_byte(), 0x00);
    }
}
