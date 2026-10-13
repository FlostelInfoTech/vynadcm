//! AE titles: validated 16-character application entity names.

use std::fmt;

use crate::{Error, Result};

/// A validated DICOM Application Entity title (≤ 16 chars, no controls).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AeTitle(String);

impl AeTitle {
    /// Validate and store a title (surrounding spaces trimmed).
    pub fn new(title: impl Into<String>) -> Result<Self> {
        let t = title.into();
        let trimmed = t.trim();
        if trimmed.is_empty() {
            return Err(Error::BadAeTitle(t, "empty"));
        }
        if trimmed.len() > 16 {
            return Err(Error::BadAeTitle(t, "longer than 16 characters"));
        }
        if !trimmed.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
            return Err(Error::BadAeTitle(
                t,
                "only ASCII graphic characters and spaces allowed",
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// Title text without padding.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 16-byte space-padded wire form.
    #[must_use]
    pub fn to_wire(&self) -> [u8; 16] {
        let mut out = [b' '; 16];
        out[..self.0.len()].copy_from_slice(self.0.as_bytes());
        out
    }

    /// Parse space-padded wire bytes.
    pub fn from_wire(bytes: &[u8; 16]) -> Result<Self> {
        let end = bytes
            .iter()
            .rposition(|b| *b != b' ' && *b != 0)
            .map_or(0, |i| i + 1);
        let text = std::str::from_utf8(&bytes[..end])
            .map_err(|_| Error::BadAeTitle(format!("{bytes:?}"), "not ASCII"))?;
        Self::new(text)
    }
}

impl fmt::Display for AeTitle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_pads() {
        let ae = AeTitle::new("VYNA_SCP").unwrap();
        assert_eq!(ae.as_str(), "VYNA_SCP");
        let wire = ae.to_wire();
        assert_eq!(&wire[..8], b"VYNA_SCP");
        assert_eq!(&wire[8..], b"        ");
        assert_eq!(AeTitle::from_wire(&wire).unwrap(), ae);
        assert!(AeTitle::new("").is_err());
        assert!(AeTitle::new("0123456789ABCDEFG").is_err());
        assert!(AeTitle::new("BAD\nAE").is_err());
        // Interior controls, DEL, and non-ASCII are all rejected ...
        assert!(AeTitle::new("BAD\x01AE").is_err());
        assert!(AeTitle::new("BAD\x7FAE").is_err());
        assert!(AeTitle::new("CAFÉ").is_err());
        assert!(AeTitle::new("É").is_err());
        // ... while interior spaces and surrounding trims still pass.
        assert_eq!(AeTitle::new("MY AE").unwrap().as_str(), "MY AE");
        assert_eq!(AeTitle::new("  PADDED  ").unwrap().as_str(), "PADDED");
    }
}
