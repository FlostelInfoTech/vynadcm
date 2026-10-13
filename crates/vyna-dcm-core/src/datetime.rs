//! Date and time value helpers for `DA`, `TM` and `DT` (NEMA PS3.5 §6.2).
//!
//! DICOM permits legacy dotted/colon formats (`YYYY.MM.DD`, `HH:MM:SS`);
//! these helpers accept both and normalise to the basic format on output.

/// A DICOM calendar date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Da {
    /// Full year, e.g. `2026`.
    pub year: u16,
    /// Month `1..=12`.
    pub month: u8,
    /// Day `1..=31`.
    pub day: u8,
}

impl Da {
    /// Parse `YYYYMMDD` or `YYYY.MM.DD`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() != 8 || (t.len() != 8 && t.len() != 10) {
            return None;
        }
        if t.len() == 10 {
            let b = t.as_bytes();
            if b[4] != b'.' || b[7] != b'.' {
                return None;
            }
        }
        let year = digits[0..4].parse().ok()?;
        let month: u8 = digits[4..6].parse().ok()?;
        let day: u8 = digits[6..8].parse().ok()?;
        if year == 0 || month == 0 || month > 12 || day == 0 || day > 31 {
            return None;
        }
        Some(Self { year, month, day })
    }

    /// True when `s` is a valid `DA` value.
    #[must_use]
    pub fn is_valid(s: &str) -> bool {
        Self::parse(s).is_some()
    }

    /// Format as `YYYYMMDD`.
    #[must_use]
    pub fn format(self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }
}

/// A DICOM time of day with microsecond precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tm {
    /// Hour `0..=23`.
    pub hour: u8,
    /// Minute `0..=59` (absent if the value only carries hours).
    pub minute: Option<u8>,
    /// Second `0..=60` with optional fractional part.
    pub second: Option<u8>,
    /// Microseconds `0..999999`.
    pub micros: u32,
}

impl Tm {
    /// Parse `HH[MM[SS[.FFFFFF]]]` with optional colons.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().trim_end_matches(':');
        let (main, frac) = match t.split_once('.') {
            Some((m, f)) => (m, Some(f)),
            None => (t, None),
        };
        let digits: String = main.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 2 || digits.len() > 6 || !digits.len().is_multiple_of(2) {
            // Odd counts only valid with colons; reject malformed input.
            if digits.len() != main.len() && !digits.len().is_multiple_of(2) {
                return None;
            }
            if !digits.len().is_multiple_of(2) {
                return None;
            }
        }
        if main.contains(':') {
            let parts: Vec<&str> = main.split(':').collect();
            if parts.len() > 3 || parts.iter().any(|p| p.len() != 2) {
                return None;
            }
        } else if !matches!(digits.len(), 2 | 4 | 6) {
            return None;
        }
        // Colon groups are shape-checked above but may carry no digits
        // at all ("ab:cd"); the hour slice needs at least two.
        if digits.len() < 2 {
            return None;
        }
        let hour: u8 = digits[0..2].parse().ok()?;
        if hour > 23 {
            return None;
        }
        let minute = digits.get(2..4).and_then(|m| m.parse::<u8>().ok());
        let second = digits.get(4..6).and_then(|m| m.parse::<u8>().ok());
        if minute.is_some_and(|m| m > 59) || second.is_some_and(|s| s > 60) {
            return None;
        }
        let mut micros = 0u32;
        if let Some(f) = frac {
            if f.is_empty() || f.len() > 6 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let scale = 10u32.pow(6 - f.len() as u32);
            micros = f.parse::<u32>().ok()? * scale;
        }
        Some(Self {
            hour,
            minute,
            second,
            micros,
        })
    }

    /// True when `s` is a valid `TM` value.
    #[must_use]
    pub fn is_valid(s: &str) -> bool {
        Self::parse(s).is_some()
    }

    /// Format as `HHMMSS.FFFFFF`, trimming trailing fraction zeroes.
    #[must_use]
    pub fn format(self) -> String {
        let mut out = format!("{:02}", self.hour);
        if let Some(m) = self.minute {
            out.push_str(&format!("{m:02}"));
        }
        if let Some(s) = self.second {
            out.push_str(&format!("{s:02}"));
            if self.micros > 0 {
                let mut frac = format!("{:06}", self.micros);
                while frac.ends_with('0') {
                    frac.pop();
                }
                out.push('.');
                out.push_str(&frac);
            }
        }
        out
    }
}

/// True when `s` is a valid `DT` value (`YYYYMMDDHHMMSS.FFFFFF±ZZXX`).
#[must_use]
pub fn is_valid_dt(s: &str) -> bool {
    let t = s.trim();
    // Split optional UTC suffix: `+ZZXX`, `-ZZXX`, `+ZZ:XX`, `Z`-less.
    let (head, suffix) = match t.rfind(['+', '-']) {
        Some(i) if i >= 8 => t.split_at(i),
        _ => (t, ""),
    };
    if !suffix.is_empty() {
        let z: String = suffix[1..].chars().filter(|c| c.is_ascii_digit()).collect();
        if z.len() != 4 {
            return false;
        }
        let zh: u32 = z[0..2].parse().unwrap_or(99);
        let zm: u32 = z[2..4].parse().unwrap_or(99);
        if zh > 23 || zm > 59 {
            return false;
        }
    }
    let (date_time, frac) = match head.split_once('.') {
        Some((h, f)) => (h, Some(f)),
        None => (head, None),
    };
    if date_time.len() < 4 || date_time.len() > 14 || !date_time.bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    if let Some(f) = frac
        && (f.is_empty() || f.len() > 6 || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    // Validate the date/time prefixes that are present.
    if date_time.len() >= 8 && Da::parse(&date_time[0..8]).is_none() {
        return false;
    }
    if date_time.len() > 8 {
        let rest = &date_time[8..];
        if !rest.len().is_multiple_of(2) {
            return false;
        }
        let mut tm = rest.to_string();
        if let Some(f) = frac {
            tm.push('.');
            tm.push_str(f);
        }
        if Tm::parse(&tm).is_none() {
            return false;
        }
    }
    true
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
#[must_use]
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date for days since 1970-01-01: `(year, month, day)`.
#[must_use]
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Current UTC time as RFC 3339 with milliseconds (`…SS.mmmZ`).
#[must_use]
pub fn utc_now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let (y, m, d) = civil_from_days((now.as_secs() / 86400) as i64);
    let sod = now.as_secs() % 86400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60,
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn da_accepts_both_formats() {
        assert_eq!(
            Da::parse("20261011"),
            Some(Da {
                year: 2026,
                month: 10,
                day: 11
            })
        );
        assert_eq!(Da::parse("2026.10.11").unwrap().format(), "20261011");
        assert!(Da::parse("20261301").is_none());
        assert!(Da::parse("2026101").is_none());
        assert!(Da::parse("").is_none());
    }

    #[test]
    fn tm_handles_partial_and_fraction() {
        let t = Tm::parse("134523.5").unwrap();
        assert_eq!(
            (t.hour, t.minute, t.second, t.micros),
            (13, Some(45), Some(23), 500_000)
        );
        assert_eq!(t.format(), "134523.5");
        assert_eq!(Tm::parse("13").unwrap().format(), "13");
        assert_eq!(Tm::parse("13:45:23").unwrap().format(), "134523");
        assert!(Tm::parse("246000").is_none());
        assert!(Tm::parse("136").is_none());
    }

    #[test]
    fn civil_math_round_trips() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
        for days in [0, 1, -1, 19723, 730485, -719468] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn rfc3339_clock_has_shape() {
        let s = utc_now_rfc3339();
        assert_eq!(s.len(), 24);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[10..11], "T");
        assert!(s[..10].bytes().filter(|b| *b == b'-').count() == 2);
    }

    #[test]
    fn dt_validates_suffix_and_prefix() {
        assert!(is_valid_dt("20261011134523.5+0800"));
        assert!(is_valid_dt("20261011134523"));
        assert!(is_valid_dt("20261011"));
        assert!(is_valid_dt("2026"));
        assert!(!is_valid_dt("20261301134523"));
        assert!(!is_valid_dt("20261011134523+2500"));
        assert!(!is_valid_dt("not-a-date"));
    }

    #[test]
    fn tm_digitless_colon_groups_are_none() {
        // Shape-valid colons with no digits must not panic (#0..2 slice).
        assert_eq!(Tm::parse("ab:cd"), None);
        assert_eq!(Tm::parse("ab:cd:ef"), None);
        assert_eq!(Tm::parse("::"), None);
        assert!(Tm::parse("12:34").is_some());
    }
}
