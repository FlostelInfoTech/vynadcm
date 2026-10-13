//! Person Name (`PN`) value handling (NEMA PS3.5 §6.2).
//!
//! Format: `Family^Given^Middle^Prefix^Suffix`, with up to three component
//! groups (`Alphabetic=Ideographic=Phonetic`). Multi-valued names are
//! separated by backslash at the element level, not here.

use std::fmt;
use std::str::FromStr;

use crate::Error;

/// One component group of a person name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameComponents {
    /// Family name (component 1).
    pub family: String,
    /// Given name (component 2).
    pub given: String,
    /// Middle name (component 3).
    pub middle: String,
    /// Name prefix, e.g. `DR.` (component 4).
    pub prefix: String,
    /// Name suffix, e.g. `JR.` (component 5).
    pub suffix: String,
}

impl NameComponents {
    /// True when every component is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.family.is_empty()
            && self.given.is_empty()
            && self.middle.is_empty()
            && self.prefix.is_empty()
            && self.suffix.is_empty()
    }

    fn parse(group: &str) -> Self {
        // Lossy fallback for the decode path: keep the first five,
        // drop the rest (dirty files must still read).
        let mut parts = group.split('^');
        let take = |it: &mut std::str::Split<'_, char>| it.next().unwrap_or("").trim().to_string();
        Self {
            family: take(&mut parts),
            given: take(&mut parts),
            middle: take(&mut parts),
            prefix: take(&mut parts),
            suffix: take(&mut parts),
        }
    }

    fn try_parse(group: &str) -> Result<Self, Error> {
        let mut parts = group.split('^');
        let take = |it: &mut std::str::Split<'_, char>| it.next().unwrap_or("").trim().to_string();
        let out = Self {
            family: take(&mut parts),
            given: take(&mut parts),
            middle: take(&mut parts),
            prefix: take(&mut parts),
            suffix: take(&mut parts),
        };
        if parts.next().is_some() {
            return Err(Error::Parse("PN group has more than 5 components".into()));
        }
        Ok(out)
    }

    /// Encode one component group (`Family^Given^...`, trailing empty
    /// components dropped).
    #[must_use]
    pub fn to_dicom_string(&self) -> String {
        let mut out = String::new();
        self.push_to(&mut out);
        out
    }

    fn push_to(&self, out: &mut String) {
        // `^` separators are significant only up to the last non-empty part.
        let parts = [
            &self.family,
            &self.given,
            &self.middle,
            &self.prefix,
            &self.suffix,
        ];
        let mut end = 0;
        for (i, p) in parts.iter().enumerate() {
            if !p.is_empty() {
                end = i + 1;
            }
        }
        for (i, p) in parts.iter().take(end).enumerate() {
            if i > 0 {
                out.push('^');
            }
            out.push_str(p);
        }
    }
}

/// A DICOM person name with alphabetic, ideographic and phonetic groups.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonName {
    /// Alphabetic group (always present, possibly empty).
    pub alphabetic: NameComponents,
    /// Ideographic group, when supplied.
    pub ideographic: Option<NameComponents>,
    /// Phonetic group, when supplied.
    pub phonetic: Option<NameComponents>,
}

impl PersonName {
    /// Parse one `PN` component value (no backslash splitting).
    ///
    /// Lossy by design (decoder liberality for dirty files): components
    /// past the 5th and groups past the 3rd are silently dropped, and a
    /// backslash keeps only the first value (embedding it would corrupt
    /// value arity on re-encode). Use [`try_parse`](Self::try_parse)
    /// for user input, where truncation must fail instead.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        // Lossy fallback for the decode path (see above).
        let s = s.split('\\').next().unwrap_or("");
        let mut groups = s.split('=');
        let alpha = NameComponents::parse(groups.next().unwrap_or(""));
        let opt = |g: Option<&str>| match g {
            Some(t) => {
                let c = NameComponents::parse(t);
                (!c.is_empty()).then_some(c)
            }
            None => None,
        };
        Self {
            alphabetic: alpha,
            ideographic: opt(groups.next()),
            phonetic: opt(groups.next()),
        }
    }

    /// Strict parse: rejects backslashes (split multi-valued names
    /// first), groups past the 3rd, and components past the 5th.
    pub fn try_parse(s: &str) -> Result<Self, Error> {
        if s.contains('\\') {
            return Err(Error::Parse(
                "multi-valued PN must be split on backslash first".into(),
            ));
        }
        let mut groups = s.split('=');
        let alpha = NameComponents::try_parse(groups.next().unwrap_or(""))?;
        let opt = |g: Option<&str>| match g {
            Some(t) => {
                let c = NameComponents::try_parse(t)?;
                Ok::<_, Error>((!c.is_empty()).then_some(c))
            }
            None => Ok(None),
        };
        let out = Self {
            alphabetic: alpha,
            ideographic: opt(groups.next())?,
            phonetic: opt(groups.next())?,
        };
        if groups.next().is_some() {
            return Err(Error::Parse("PN has more than 3 groups".into()));
        }
        Ok(out)
    }

    /// Encode back to a DICOM `PN` string.
    #[must_use]
    pub fn to_dicom_string(&self) -> String {
        let mut out = String::new();
        self.alphabetic.push_to(&mut out);
        if self.ideographic.is_some() || self.phonetic.is_some() {
            out.push('=');
            if let Some(g) = &self.ideographic {
                g.push_to(&mut out);
            }
        }
        if let Some(g) = &self.phonetic {
            out.push('=');
            g.push_to(&mut out);
        }
        out
    }

    /// Short display form: `"Family, Given"`, falling back gracefully.
    #[must_use]
    pub fn family_given(&self) -> String {
        match (
            self.alphabetic.family.as_str(),
            self.alphabetic.given.as_str(),
        ) {
            ("", "") => String::new(),
            (f, "") => f.to_string(),
            ("", g) => g.to_string(),
            (f, g) => format!("{f}, {g}"),
        }
    }

    /// Build from family/given names.
    #[must_use]
    pub fn from_parts(family: &str, given: &str) -> Self {
        Self {
            alphabetic: NameComponents {
                family: family.trim().to_string(),
                given: given.trim().to_string(),
                ..NameComponents::default()
            },
            ..Self::default()
        }
    }
}

impl fmt::Display for PersonName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_dicom_string())
    }
}

impl FromStr for PersonName {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_five_components() {
        let pn = PersonName::parse("Adams^John^Robert^Dr^Jr");
        assert_eq!(pn.alphabetic.family, "Adams");
        assert_eq!(pn.alphabetic.suffix, "Jr");
        assert_eq!(pn.to_dicom_string(), "Adams^John^Robert^Dr^Jr");
        assert_eq!(pn.family_given(), "Adams, John");
    }

    #[test]
    fn try_parse_rejects_overlong_and_backslash() {
        assert!(PersonName::try_parse("A^B^C^D^E^F").is_err());
        assert!(PersonName::try_parse("a=b=c=d").is_err());
        assert!(PersonName::try_parse("A\\B").is_err());
        assert!(PersonName::try_parse("A^B^C^D^E=a=b").is_ok());
        // Lossy parse keeps its documented truncation (decode path).
        let pn = PersonName::parse("A^B^C^D^E^F=a=b=c=d");
        assert_eq!(pn.to_dicom_string(), "A^B^C^D^E=a=b");
        // ... including backslash: first value wins, arity preserved.
        let pn = PersonName::parse("Doe^Jane\\Smith^John");
        assert_eq!(pn.to_dicom_string(), "Doe^Jane");
        assert!(!pn.to_dicom_string().contains('\\'));
    }

    #[test]
    fn round_trips_groups_and_trims_padding() {
        let pn = PersonName::parse("Yamada^Taro== ");
        assert_eq!(pn.to_dicom_string(), "Yamada^Taro");
        let full = PersonName::parse("Hong^Gildong=Hong^Gildong=Hong^Gildong");
        assert_eq!(
            full.to_dicom_string(),
            "Hong^Gildong=Hong^Gildong=Hong^Gildong"
        );
    }

    #[test]
    fn rejects_backslash_values() {
        assert!("A^B\\C^D".parse::<PersonName>().is_err());
    }
}
