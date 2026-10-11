//! ECG recording model: leads, samples and physical-unit conversion.
//!
//! Physical values follow PS3.3 C.10.9:
//!
//! ```text
//! physical = sample × sensitivity × correction_factor + baseline
//! ```
//!
//! where sensitivity/correction/baseline come from the Channel Definition
//! Sequence and the units (usually millivolts) from the Channel Sensitivity
//! Units Sequence.

/// Waveform sample interpretation (PS3.3 C.10.9.1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SampleInterpretation {
    /// Signed 8/16-bit (`SB`/`SS`).
    #[default]
    Signed,
    /// Unsigned 8/16-bit (`UB`/`US`).
    Unsigned,
}

impl SampleInterpretation {
    /// DICOM code for 16-bit data (`SS`/`US`).
    #[must_use]
    pub const fn code_16(self) -> &'static str {
        match self {
            SampleInterpretation::Signed => "SS",
            SampleInterpretation::Unsigned => "US",
        }
    }

    /// DICOM code for 8-bit data (`SB`/`UB`).
    #[must_use]
    pub const fn code_8(self) -> &'static str {
        match self {
            SampleInterpretation::Signed => "SB",
            SampleInterpretation::Unsigned => "UB",
        }
    }
}

/// One ECG lead (channel).
#[derive(Clone, Debug)]
pub struct EcgLead {
    /// Channel label, e.g. `"II"`, `"V5"`.
    pub label: String,
    /// Raw ADC samples, one per sampling instant (`i32` holds 8/16-bit
    /// signed and unsigned raw values exactly).
    pub samples: Vec<i32>,
    /// Physical units per LSB (e.g. `0.001` for 1 µV/LSB in mV units).
    pub sensitivity: f64,
    /// Sensitivity units (UCUM, e.g. `"mV"`).
    pub units: String,
    /// Calibration multiplier (usually `1.0`).
    pub correction_factor: f64,
    /// Physical value at encoded sample 0 (usually `0.0`).
    pub baseline: f64,
    /// MDC code value (`"partition:code"`, e.g. `"2:2"` for lead II).
    pub mdc_code: Option<String>,
    /// Low/high/notch filter frequencies in Hz, when known.
    pub filter_low_hz: Option<f64>,
    /// High-frequency filter in Hz.
    pub filter_high_hz: Option<f64>,
    /// Notch filter frequency in Hz.
    pub notch_hz: Option<f64>,
}

impl EcgLead {
    /// Build a lead with VynaECG defaults (mV, 1.0 correction, 0 baseline).
    pub fn new(label: impl Into<String>, samples: Vec<i32>, sensitivity: f64) -> Self {
        Self {
            label: label.into(),
            samples,
            sensitivity,
            units: "mV".into(),
            correction_factor: 1.0,
            baseline: 0.0,
            mdc_code: None,
            filter_low_hz: None,
            filter_high_hz: None,
            notch_hz: None,
        }
    }

    /// Convert raw samples to physical units.
    #[must_use]
    pub fn physical_samples(&self) -> Vec<f64> {
        let gain = self.sensitivity * self.correction_factor;
        self.samples
            .iter()
            .map(|s| f64::from(*s) * gain + self.baseline)
            .collect()
    }

    /// Convert raw samples from `i16` storage.
    #[must_use]
    pub fn from_i16(label: impl Into<String>, samples: &[i16], sensitivity: f64) -> Self {
        Self::new(
            label,
            samples.iter().map(|s| i32::from(*s)).collect(),
            sensitivity,
        )
    }

    /// Number of samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// A multi-lead ECG recording (one multiplex group).
#[derive(Clone, Debug)]
pub struct EcgRecording {
    /// Leads in channel order.
    pub leads: Vec<EcgLead>,
    /// Sampling frequency in Hz, shared by all leads.
    pub sampling_frequency_hz: f64,
    /// Multiplex group label (e.g. `"RESTING 12-LEAD"`).
    pub multiplex_label: Option<String>,
    /// Waveform originality (`ORIGINAL` or `DERIVED`).
    pub originality: String,
    /// Powerline frequency in Hz (50/60), when known.
    pub powerline_hz: Option<f64>,
    /// Amplifier type free text.
    pub amplifier_type: Option<String>,
    /// Sample interpretation shared by all channels.
    pub sample_interpretation: SampleInterpretation,
    /// Bits allocated per sample (8 or 16).
    pub bits_allocated: u16,
}

impl EcgRecording {
    /// Build a recording; all leads must share the sample count.
    /// Defaults: signed 16-bit samples.
    pub fn new(leads: Vec<EcgLead>, sampling_frequency_hz: f64) -> Result<Self, String> {
        if leads.is_empty() {
            return Err("recording without leads".into());
        }
        let n = leads[0].len();
        if leads.iter().any(|l| l.len() != n) {
            return Err("leads have unequal sample counts".into());
        }
        Ok(Self {
            leads,
            sampling_frequency_hz,
            multiplex_label: None,
            originality: "ORIGINAL".into(),
            powerline_hz: None,
            amplifier_type: None,
            sample_interpretation: SampleInterpretation::Signed,
            bits_allocated: 16,
        })
    }

    /// Samples per lead.
    #[must_use]
    pub fn samples_per_lead(&self) -> usize {
        self.leads.first().map_or(0, EcgLead::len)
    }

    /// Recording duration in seconds.
    #[must_use]
    pub fn duration_secs(&self) -> f64 {
        self.samples_per_lead() as f64 / self.sampling_frequency_hz
    }
}

/// Standard 12-lead order with MDC (partition 2) codes per
/// ISO/IEEE 11073-10101 / PS3.16 CID 3001:
///
/// I=2:1, II=2:2, III=2:61, aVR=2:62, aVL=2:63, aVF=2:64,
/// V1=2:3 … V6=2:8.
pub const STANDARD_12_LEAD: [(&str, &str); 12] = [
    ("I", "2:1"),
    ("II", "2:2"),
    ("III", "2:61"),
    ("aVR", "2:62"),
    ("aVL", "2:63"),
    ("aVF", "2:64"),
    ("V1", "2:3"),
    ("V2", "2:4"),
    ("V3", "2:5"),
    ("V4", "2:6"),
    ("V5", "2:7"),
    ("V6", "2:8"),
];

/// Look up the MDC code for a standard lead label.
#[must_use]
pub fn mdc_code_for_lead(label: &str) -> Option<&'static str> {
    STANDARD_12_LEAD
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_conversion() {
        let lead = EcgLead {
            baseline: 0.5,
            correction_factor: 2.0,
            ..EcgLead::new("II", vec![0, 1000, -1000], 0.001)
        };
        let phys = lead.physical_samples();
        assert_eq!(phys, vec![0.5, 2.5, -1.5]);
    }

    #[test]
    fn recording_validates_shape() {
        let a = EcgLead::new("I", vec![0; 10], 0.001);
        let b = EcgLead::new("II", vec![0; 9], 0.001);
        assert!(EcgRecording::new(vec![a.clone()], 500.0).is_ok());
        assert!(EcgRecording::new(vec![a, b], 500.0).is_err());
        assert!(EcgRecording::new(vec![], 500.0).is_err());
        let r = EcgRecording::new(vec![EcgLead::new("I", vec![0; 5000], 0.001)], 500.0).unwrap();
        assert_eq!(r.duration_secs(), 10.0);
    }

    #[test]
    fn lead_codes() {
        assert_eq!(mdc_code_for_lead("II"), Some("2:2"));
        assert_eq!(mdc_code_for_lead("aVR"), Some("2:62"));
        assert_eq!(mdc_code_for_lead("V6"), Some("2:8"));
        assert_eq!(mdc_code_for_lead("X"), None);
    }
}
