//! Converting between system time and the calendar fields a zip stores.
//!
//! # Why this is here rather than a dependency
//!
//! A zip records modification times as MS-DOS date and time fields — year,
//! month, day, hour, minute, second — so writing one means turning a
//! `SystemTime` into a calendar date. The `zip` crate can do this, but only
//! through an optional dependency on a full date-time library, and this needs
//! six integers rather than time zones, formatting and leap seconds.
//!
//! Everything here is UTC. A zip has no time zone field, so the alternative is
//! guessing one, and a file that shifts by an hour depending on where it was
//! unpacked is worse than one that is consistently UTC.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A calendar date and time, in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Splits a moment into calendar fields.
///
/// Returns `None` before 1980, which is the earliest a zip can record, and
/// after 2107, which is the latest.
pub fn civil_from(time: SystemTime) -> Option<Civil> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    civil_from_unix(seconds)
}

/// As [`civil_from`], from a count of seconds since the Unix epoch.
pub fn civil_from_unix(seconds: u64) -> Option<Civil> {
    let days = seconds / 86_400;
    let rest = seconds % 86_400;

    let (year, month, day) = civil_from_days(days);
    if !(1980..=2107).contains(&year) {
        return None;
    }

    Some(Civil {
        year,
        month,
        day,
        hour: (rest / 3600) as u8,
        minute: ((rest % 3600) / 60) as u8,
        second: (rest % 60) as u8,
    })
}

/// The reverse: a moment from calendar fields.
pub fn unix_from_civil(c: Civil) -> Option<SystemTime> {
    let days = days_from_civil(c.year, c.month, c.day)?;
    let seconds = days * 86_400
        + c.hour as u64 * 3600
        + c.minute as u64 * 60
        + c.second as u64;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// Days since 1970-01-01 to a civil date.
///
/// Howard Hinnant's `civil_from_days`, which is the standard way to do this
/// without a table: it shifts the era to start in March so that the leap day
/// falls at the end of a year and the month lengths become a simple formula.
fn civil_from_days(days: u64) -> (u16, u8, u8) {
    // Shift the epoch to 0000-03-01, which is 719_468 days before 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;

    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u8;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u8;
    let year = if month <= 2 { year + 1 } else { year };

    (year as u16, month, day)
}

/// The inverse of [`civil_from_days`].
fn days_from_civil(year: u16, month: u8, day: u8) -> Option<u64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    let y = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> Civil {
        civil_from_unix(seconds).expect("in range")
    }

    #[test]
    fn a_known_moment_splits_correctly() {
        // 2026-08-22T07:56:00Z
        let c = at(1_787_385_360);
        assert_eq!((c.year, c.month, c.day), (2026, 8, 22));
        assert_eq!((c.hour, c.minute, c.second), (7, 56, 0));
    }

    #[test]
    fn the_start_of_the_zip_era_is_representable() {
        let c = at(315_532_800); // 1980-01-01T00:00:00Z
        assert_eq!((c.year, c.month, c.day), (1980, 1, 1));
    }

    #[test]
    fn a_leap_day_is_a_real_date() {
        let c = at(1_709_164_800); // 2024-02-29T00:00:00Z
        assert_eq!((c.year, c.month, c.day), (2024, 2, 29));
    }

    #[test]
    fn a_century_that_is_not_a_leap_year_is_handled() {
        // 2100 is divisible by 4 but not a leap year, which is the case a
        // hand-rolled calendar gets wrong.
        let c = at(4_107_542_400); // 2100-03-01T00:00:00Z
        assert_eq!((c.year, c.month, c.day), (2100, 3, 1));
    }

    #[test]
    fn times_before_the_zip_era_are_refused() {
        assert!(civil_from_unix(0).is_none()); // 1970
    }

    #[test]
    fn times_beyond_the_zip_era_are_refused() {
        // Year 2200-ish. A file with a corrupt future timestamp must not
        // produce a nonsense date rather than being declined.
        assert!(civil_from_unix(7_258_118_400).is_none());
    }

    #[test]
    fn the_conversion_round_trips() {
        // Every hour across a decade that spans leap years and a century rule.
        let mut seconds = 315_532_800u64; // 1980-01-01
        while seconds < 4_200_000_000 {
            let civil = civil_from_unix(seconds).expect("in range");
            let back = unix_from_civil(civil).expect("representable");
            assert_eq!(
                back.duration_since(UNIX_EPOCH).unwrap().as_secs(),
                seconds,
                "round trip failed at {seconds} ({civil:?})"
            );
            seconds += 3_600 * 97; // a prime-ish stride, so it lands on every weekday and month
        }
    }
}
