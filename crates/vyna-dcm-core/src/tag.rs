//! DICOM data element tag `(GGGG,EEEE)`.
//!
//! A tag identifies one attribute: a 16-bit group number plus a 16-bit
//! element number (NEMA PS3.5 §6.4).

use std::fmt;
use std::str::FromStr;

use crate::Error;

/// A DICOM data element tag.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Tag(pub u16, pub u16);

impl Tag {
    /// Build a tag from group and element numbers.
    #[must_use]
    pub const fn new(group: u16, element: u16) -> Self {
        Self(group, element)
    }

    /// Group number (high 16 bits).
    #[must_use]
    pub const fn group(self) -> u16 {
        self.0
    }

    /// Element number (low 16 bits).
    #[must_use]
    pub const fn element(self) -> u16 {
        self.1
    }

    /// Pack into a single `u32` (`group << 16 | element`).
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        ((self.0 as u32) << 16) | (self.1 as u32)
    }

    /// Unpack from a `u32`.
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        Self((v >> 16) as u16, v as u16)
    }

    /// Odd group number: a private attribute (PS3.5 §7.8).
    #[must_use]
    pub const fn is_private(self) -> bool {
        self.0 % 2 == 1
    }

    /// Private creator element: odd group, element `0x0010..=0x00FF`.
    #[must_use]
    pub const fn is_private_creator(self) -> bool {
        self.is_private() && self.1 >= 0x0010 && self.1 <= 0x00FF
    }

    /// Private reservation block (`0x10..=0xFF`) for data elements in odd
    /// groups (element `>= 0x1000`); `None` otherwise.
    #[must_use]
    pub const fn private_block(self) -> Option<u8> {
        if self.is_private() && self.1 >= 0x1000 {
            Some((self.1 >> 8) as u8)
        } else {
            None
        }
    }

    /// Private Creator element holding this data element's creator ID.
    #[must_use]
    pub const fn private_creator_element(self) -> Option<Tag> {
        match self.private_block() {
            Some(block) => Some(Tag::new(self.0, block as u16)),
            None => None,
        }
    }

    /// Suffix byte (low byte of the element) for private data elements;
    /// the key into [`crate::lookup_private`]. `None` otherwise.
    #[must_use]
    pub const fn private_suffix(self) -> Option<u8> {
        if self.is_private() && self.1 >= 0x1000 {
            Some((self.1 & 0xFF) as u8)
        } else {
            None
        }
    }

    /// Group length element (`EEEE == 0x0000`, retired but still seen on wire).
    #[must_use]
    pub const fn is_group_length(self) -> bool {
        self.1 == 0x0000
    }

    /// Command group element (`GGGG == 0x0000`).
    #[must_use]
    pub const fn is_command(self) -> bool {
        self.0 == 0x0000
    }

    /// File meta group element (`GGGG == 0x0002`).
    #[must_use]
    pub const fn is_file_meta(self) -> bool {
        self.0 == 0x0002
    }

    /// Item, item-delimitation or sequence-delimitation tag.
    #[must_use]
    pub const fn is_delimiter(self) -> bool {
        self.0 == 0xFFFE && matches!(self.1, 0xE000 | 0xE00D | 0xE0DD)
    }
}

impl From<(u16, u16)> for Tag {
    fn from(v: (u16, u16)) -> Self {
        Self(v.0, v.1)
    }
}

impl From<u32> for Tag {
    fn from(v: u32) -> Self {
        Self::from_u32(v)
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({:04X},{:04X})", self.0, self.1)
    }
}

impl FromStr for Tag {
    type Err = Error;

    /// Parse `(GGGG,EEEE)` with optional whitespace, e.g. `"(0010,0010)"`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let fail = || Error::InvalidTag(s.to_string());
        let s = s.trim();
        let inner = s
            .strip_prefix('(')
            .and_then(|t| t.strip_suffix(')'))
            .ok_or_else(fail)?;
        let (g, e) = inner.split_once(',').ok_or_else(fail)?;
        let group = u16::from_str_radix(g.trim(), 16).map_err(|_| fail())?;
        let element = u16::from_str_radix(e.trim(), 16).map_err(|_| fail())?;
        Ok(Self(group, element))
    }
}

/// Delimiter tags used by sequences and encapsulated data.
pub mod delimiter {
    use super::Tag;

    /// Item tag `(FFFE,E000)`.
    pub const ITEM: Tag = Tag::new(0xFFFE, 0xE000);
    /// Item delimitation item `(FFFE,E00D)`.
    pub const ITEM_DELIMITATION: Tag = Tag::new(0xFFFE, 0xE00D);
    /// Sequence delimitation item `(FFFE,E0DD)`.
    pub const SEQUENCE_DELIMITATION: Tag = Tag::new(0xFFFE, 0xE0DD);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_reservation_mechanics() {
        let t = Tag::new(0x0019, 0x1021);
        assert_eq!(t.private_block(), Some(0x10));
        assert_eq!(t.private_creator_element(), Some(Tag::new(0x0019, 0x0010)));
        assert_eq!(t.private_suffix(), Some(0x21));
        // Even groups, creator elements, and low elements have no block.
        assert_eq!(Tag::new(0x0018, 0x1021).private_block(), None);
        assert_eq!(Tag::new(0x0019, 0x0010).private_block(), None);
        assert_eq!(Tag::new(0x0019, 0x0001).private_suffix(), None);
        assert_eq!(Tag::new(0x0010, 0x0010).private_creator_element(), None);
    }

    #[test]
    fn display_and_parse_round_trip() {
        let tag = Tag::new(0x0010, 0x0010);
        assert_eq!(tag.to_string(), "(0010,0010)");
        assert_eq!("(0010,0010)".parse::<Tag>().unwrap(), tag);
        assert_eq!("( 0010 , 0010 )".parse::<Tag>().unwrap(), tag);
        assert_eq!(Tag::from_u32(0x0010_0010), tag);
        assert_eq!(tag.to_u32(), 0x0010_0010);
    }

    #[test]
    fn rejects_malformed_tags() {
        for bad in [
            "",
            "0010,0010",
            "(0010)",
            "(ZZZZ,0010)",
            "(0010,0010",
            "(10,10,10)",
        ] {
            assert!(bad.parse::<Tag>().is_err(), "{bad}");
        }
    }

    #[test]
    fn classifies_special_tags() {
        assert!(Tag::new(0x0009, 0x0010).is_private_creator());
        assert!(!Tag::new(0x0009, 0x1001).is_private_creator());
        assert!(Tag::new(0x0009, 0x1001).is_private());
        assert!(!Tag::new(0x0008, 0x0010).is_private());
        assert!(Tag::new(0x0002, 0x0000).is_group_length());
        assert!(Tag::new(0x0000, 0x0100).is_command());
        assert!(Tag::new(0x0002, 0x0010).is_file_meta());
        assert!(delimiter::ITEM.is_delimiter());
        assert!(!Tag::new(0x0008, 0x0018).is_delimiter());
    }
}
