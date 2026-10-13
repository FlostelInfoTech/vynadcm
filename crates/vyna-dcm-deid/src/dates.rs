//! Calendar arithmetic for longitudinal date shifting (civil math lives
//! in `vyna_dcm_core`).

pub use vyna_dcm_core::{civil_from_days, days_from_civil};

/// Shift a `DA` value by `offset` days, returning `YYYYMMDD`.
///
/// Returns `None` when the input is not a valid date (the caller keeps the
/// original and records a warning) or the result falls outside years
/// `1..=9999`.
#[must_use]
pub fn shift_da(value: &str, offset: i64) -> Option<String> {
    let da = vyna_dcm_core::Da::parse(value)?;
    shift_ymd(da.year as i64, da.month as i64, da.day as i64, offset)
}

/// Shift the calendar part of a `DT` value by `offset` days, keeping the
/// time-of-day (and any timezone suffix) verbatim: whole-day offsets never
/// change the clock time.
///
/// Returns `None` when the leading `YYYYMMDD` is not a valid date or the
/// result falls outside years `1..=9999`.
#[must_use]
pub fn shift_dt(value: &str, offset: i64) -> Option<String> {
    if value.len() < 8
        || !value.as_bytes()[..8].iter().all(u8::is_ascii_digit)
        || !vyna_dcm_core::is_valid_dt(value)
    {
        return None;
    }
    let shifted = shift_ymd(
        value[0..4].parse().ok()?,
        value[4..6].parse().ok()?,
        value[6..8].parse().ok()?,
        offset,
    )?;
    Some(format!("{shifted}{}", &value[8..]))
}

fn shift_ymd(year: i64, month: i64, day: i64, offset: i64) -> Option<String> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let shifted_days = days.checked_add(offset)?;
    let (y, m, d) = civil_from_days(shifted_days);
    if !(1..=9999).contains(&y) {
        return None;
    }
    Some(format!("{y:04}{m:02}{d:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        for days in [0, 1, -1, 10957, 19723, 730485, -719468] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "days={days}");
        }
    }

    #[test]
    fn shifts_cross_month_year_and_leap_day() {
        assert_eq!(shift_da("20240101", 30).as_deref(), Some("20240131"));
        assert_eq!(shift_da("20240131", 1).as_deref(), Some("20240201"));
        assert_eq!(shift_da("20240228", 1).as_deref(), Some("20240229"));
        assert_eq!(shift_da("20240229", 1).as_deref(), Some("20240301"));
        assert_eq!(shift_da("20231231", 1).as_deref(), Some("20240101"));
        assert_eq!(shift_da("20240101", -1).as_deref(), Some("20231231"));
        assert_eq!(shift_da("2023.02.28", 1).as_deref(), Some("20230301"));
    }

    #[test]
    fn bad_dates_and_overflow_return_none() {
        assert_eq!(shift_da("not-a-date", 7), None);
        assert_eq!(shift_da("20241301", 7), None);
        assert_eq!(shift_da("00010101", -1), None);
        assert_eq!(shift_da("99991231", 1), None);
        // day-count arithmetic must not overflow on extreme offsets.
        assert_eq!(shift_da("20240101", i64::MAX), None);
        assert_eq!(shift_da("20240101", i64::MIN), None);
        assert_eq!(shift_dt("20240115120000", i64::MAX), None);
    }

    #[test]
    fn datetimes_shift_the_calendar_part_only() {
        assert_eq!(
            shift_dt("20240115120000", 30).as_deref(),
            Some("20240214120000")
        );
        assert_eq!(shift_dt("20240115", 1).as_deref(), Some("20240116"));
        assert_eq!(shift_dt("baddate120000", 7), None);
        assert_eq!(shift_dt("20241301120000", 7), None);
        assert_eq!(shift_dt("99991231120000", 1), None);
    }
}
