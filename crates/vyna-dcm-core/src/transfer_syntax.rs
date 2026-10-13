//! Transfer Syntaxes (NEMA PS3.5 §10 / PS3.6 Annex A).
//!
//! Each variant maps 1:1 to a Transfer Syntax UID and exposes the wire
//! properties the codec and the association negotiator need.

/// A DICOM transfer syntax.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransferSyntax {
    /// Implicit VR Little Endian (`1.2.840.10008.1.2`): the DICOM
    /// default transfer syntax, hence this enum's [`Default`].
    #[default]
    ImplicitVrLittleEndian,
    /// Explicit VR Little Endian (`1.2.840.10008.1.2.1`).
    ExplicitVrLittleEndian,
    /// Deflated Explicit VR Little Endian (`1.2.840.10008.1.2.1.99`).
    DeflatedExplicitVrLittleEndian,
    /// Explicit VR Big Endian, retired (`1.2.840.10008.1.2.2`).
    ExplicitVrBigEndian,
    /// JPEG Baseline 8-bit (`1.2.840.10008.1.2.4.50`).
    JpegBaseline8Bit,
    /// JPEG Extended 12-bit (`1.2.840.10008.1.2.4.51`).
    JpegExtended12Bit,
    /// JPEG Lossless, hierarchical (`1.2.840.10008.1.2.4.70`).
    JpegLossless,
    /// JPEG-LS Lossless (`1.2.840.10008.1.2.4.80`).
    JpegLsLossless,
    /// JPEG-LS Near-lossless (`1.2.840.10008.1.2.4.81`).
    JpegLsNearLossless,
    /// JPEG 2000 Lossless (`1.2.840.10008.1.2.4.90`).
    Jpeg2000Lossless,
    /// JPEG 2000 (`1.2.840.10008.1.2.4.91`).
    Jpeg2000,
    /// JPEG 2000 Part 2 Lossless (`1.2.840.10008.1.2.4.92`).
    Jpeg2000Part2Lossless,
    /// JPEG 2000 Part 2 (`1.2.840.10008.1.2.4.93`).
    Jpeg2000Part2,
    /// High-Throughput JPEG 2000 Lossless (`1.2.840.10008.1.2.4.201`).
    HtJpeg2000Lossless,
    /// High-Throughput JPEG 2000 (`1.2.840.10008.1.2.4.202`).
    HtJpeg2000,
    /// RLE Lossless (`1.2.840.10008.1.2.5`).
    RleLossless,
}

impl TransferSyntax {
    /// All transfer syntaxes this library understands.
    pub const ALL: [TransferSyntax; 16] = [
        TransferSyntax::ImplicitVrLittleEndian,
        TransferSyntax::ExplicitVrLittleEndian,
        TransferSyntax::DeflatedExplicitVrLittleEndian,
        TransferSyntax::ExplicitVrBigEndian,
        TransferSyntax::JpegBaseline8Bit,
        TransferSyntax::JpegExtended12Bit,
        TransferSyntax::JpegLossless,
        TransferSyntax::JpegLsLossless,
        TransferSyntax::JpegLsNearLossless,
        TransferSyntax::Jpeg2000Lossless,
        TransferSyntax::Jpeg2000,
        TransferSyntax::Jpeg2000Part2Lossless,
        TransferSyntax::Jpeg2000Part2,
        TransferSyntax::HtJpeg2000Lossless,
        TransferSyntax::HtJpeg2000,
        TransferSyntax::RleLossless,
        // NOTE: keep `ALL` in sync with `from_uid`/`uid`.
    ];

    /// Transfer Syntax UID.
    #[must_use]
    pub const fn uid(self) -> &'static str {
        match self {
            TransferSyntax::ImplicitVrLittleEndian => "1.2.840.10008.1.2",
            TransferSyntax::ExplicitVrLittleEndian => "1.2.840.10008.1.2.1",
            TransferSyntax::DeflatedExplicitVrLittleEndian => "1.2.840.10008.1.2.1.99",
            TransferSyntax::ExplicitVrBigEndian => "1.2.840.10008.1.2.2",
            TransferSyntax::JpegBaseline8Bit => "1.2.840.10008.1.2.4.50",
            TransferSyntax::JpegExtended12Bit => "1.2.840.10008.1.2.4.51",
            TransferSyntax::JpegLossless => "1.2.840.10008.1.2.4.70",
            TransferSyntax::JpegLsLossless => "1.2.840.10008.1.2.4.80",
            TransferSyntax::JpegLsNearLossless => "1.2.840.10008.1.2.4.81",
            TransferSyntax::Jpeg2000Lossless => "1.2.840.10008.1.2.4.90",
            TransferSyntax::Jpeg2000 => "1.2.840.10008.1.2.4.91",
            TransferSyntax::Jpeg2000Part2Lossless => "1.2.840.10008.1.2.4.92",
            TransferSyntax::Jpeg2000Part2 => "1.2.840.10008.1.2.4.93",
            TransferSyntax::HtJpeg2000Lossless => "1.2.840.10008.1.2.4.201",
            TransferSyntax::HtJpeg2000 => "1.2.840.10008.1.2.4.202",
            TransferSyntax::RleLossless => "1.2.840.10008.1.2.5",
        }
    }

    /// Short human name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            TransferSyntax::ImplicitVrLittleEndian => "Implicit VR Little Endian",
            TransferSyntax::ExplicitVrLittleEndian => "Explicit VR Little Endian",
            TransferSyntax::DeflatedExplicitVrLittleEndian => "Deflated Explicit VR Little Endian",
            TransferSyntax::ExplicitVrBigEndian => "Explicit VR Big Endian (retired)",
            TransferSyntax::JpegBaseline8Bit => "JPEG Baseline (8-bit)",
            TransferSyntax::JpegExtended12Bit => "JPEG Extended (12-bit)",
            TransferSyntax::JpegLossless => "JPEG Lossless",
            TransferSyntax::JpegLsLossless => "JPEG-LS Lossless",
            TransferSyntax::JpegLsNearLossless => "JPEG-LS Near-lossless",
            TransferSyntax::Jpeg2000Lossless => "JPEG 2000 Lossless",
            TransferSyntax::Jpeg2000 => "JPEG 2000",
            TransferSyntax::Jpeg2000Part2Lossless => "JPEG 2000 Part 2 Lossless",
            TransferSyntax::Jpeg2000Part2 => "JPEG 2000 Part 2",
            TransferSyntax::HtJpeg2000Lossless => "HT JPEG 2000 Lossless",
            TransferSyntax::HtJpeg2000 => "HT JPEG 2000",
            TransferSyntax::RleLossless => "RLE Lossless",
        }
    }

    /// Parse from a UID; `None` for unknown/private syntaxes.
    #[must_use]
    pub fn from_uid(uid: &str) -> Option<Self> {
        let uid = uid.trim().trim_end_matches('\0').trim();
        Self::ALL.into_iter().find(|ts| ts.uid() == uid)
    }

    /// VR is carried on the wire (false only for Implicit VR LE).
    #[must_use]
    pub const fn is_explicit_vr(self) -> bool {
        !matches!(self, TransferSyntax::ImplicitVrLittleEndian)
    }

    /// Multi-byte values are little-endian (false only for Explicit VR BE).
    #[must_use]
    pub const fn is_little_endian(self) -> bool {
        !matches!(self, TransferSyntax::ExplicitVrBigEndian)
    }

    /// Dataset is zlib-deflated after the file meta (deflated TS only).
    #[must_use]
    pub const fn is_deflated(self) -> bool {
        matches!(self, TransferSyntax::DeflatedExplicitVrLittleEndian)
    }

    /// Pixel data may have undergone lossy compression.
    ///
    /// True for the lossy JPEG/JPEG-LS/JPEG 2000/HTJ2K syntaxes. Part 2
    /// reports true: a Part-2 codestream can be lossy and the transfer
    /// syntax alone cannot prove otherwise, so guarded attributes (e.g.
    /// Lossy Image Compression) are treated as required.
    #[must_use]
    pub const fn is_lossy(self) -> bool {
        matches!(
            self,
            TransferSyntax::JpegBaseline8Bit
                | TransferSyntax::JpegExtended12Bit
                | TransferSyntax::JpegLsNearLossless
                | TransferSyntax::Jpeg2000
                | TransferSyntax::Jpeg2000Part2
                | TransferSyntax::HtJpeg2000
        )
    }

    /// Pixel/waveform data is encapsulated in fragments.
    #[must_use]
    pub const fn is_encapsulated(self) -> bool {
        matches!(
            self,
            TransferSyntax::JpegBaseline8Bit
                | TransferSyntax::JpegExtended12Bit
                | TransferSyntax::JpegLossless
                | TransferSyntax::JpegLsLossless
                | TransferSyntax::JpegLsNearLossless
                | TransferSyntax::Jpeg2000Lossless
                | TransferSyntax::Jpeg2000
                | TransferSyntax::Jpeg2000Part2Lossless
                | TransferSyntax::Jpeg2000Part2
                | TransferSyntax::HtJpeg2000Lossless
                | TransferSyntax::HtJpeg2000
                | TransferSyntax::RleLossless
        )
    }

    /// Native pixel decode without a plugged-in codec: uncompressed,
    /// deflated and RLE syntaxes.
    #[must_use]
    pub const fn is_natively_decodable(self) -> bool {
        matches!(
            self,
            TransferSyntax::ImplicitVrLittleEndian
                | TransferSyntax::ExplicitVrLittleEndian
                | TransferSyntax::DeflatedExplicitVrLittleEndian
                | TransferSyntax::ExplicitVrBigEndian
                | TransferSyntax::RleLossless
        )
    }

    /// Uncompressed syntaxes (native pixel data, no fragments).
    #[must_use]
    pub const fn is_uncompressed(self) -> bool {
        matches!(
            self,
            TransferSyntax::ImplicitVrLittleEndian
                | TransferSyntax::ExplicitVrLittleEndian
                | TransferSyntax::DeflatedExplicitVrLittleEndian
                | TransferSyntax::ExplicitVrBigEndian
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_table_is_consistent() {
        assert_eq!(TransferSyntax::ALL.len(), 16);
        for ts in TransferSyntax::ALL {
            assert_eq!(TransferSyntax::from_uid(ts.uid()), Some(ts), "{}", ts.uid());
            assert!(crate::uid::is_valid_uid(ts.uid()));
        }
        assert_eq!(TransferSyntax::from_uid("1.2.3.4"), None);
        // Trailing NUL padding (as read from a UI value) is tolerated.
        assert_eq!(
            TransferSyntax::from_uid("1.2.840.10008.1.2.1\0"),
            Some(TransferSyntax::ExplicitVrLittleEndian)
        );
    }

    #[test]
    fn wire_properties() {
        assert!(!TransferSyntax::ImplicitVrLittleEndian.is_explicit_vr());
        assert!(!TransferSyntax::ExplicitVrBigEndian.is_little_endian());
        assert!(TransferSyntax::DeflatedExplicitVrLittleEndian.is_deflated());
        assert!(TransferSyntax::RleLossless.is_encapsulated());
        assert!(TransferSyntax::RleLossless.is_natively_decodable());
        assert!(!TransferSyntax::JpegBaseline8Bit.is_natively_decodable());
        assert!(TransferSyntax::ExplicitVrLittleEndian.is_uncompressed());
        assert!(TransferSyntax::JpegBaseline8Bit.is_lossy());
        assert!(TransferSyntax::JpegLsNearLossless.is_lossy());
        assert!(TransferSyntax::Jpeg2000.is_lossy());
        assert!(TransferSyntax::HtJpeg2000.is_lossy());
        assert!(TransferSyntax::Jpeg2000Part2.is_lossy());
        assert!(!TransferSyntax::JpegLossless.is_lossy());
        assert!(!TransferSyntax::JpegLsLossless.is_lossy());
        assert!(!TransferSyntax::Jpeg2000Lossless.is_lossy());
        assert!(!TransferSyntax::Jpeg2000Part2Lossless.is_lossy());
        assert!(!TransferSyntax::HtJpeg2000Lossless.is_lossy());
        assert!(!TransferSyntax::RleLossless.is_lossy());
        assert!(!TransferSyntax::ExplicitVrLittleEndian.is_lossy());
    }
}
