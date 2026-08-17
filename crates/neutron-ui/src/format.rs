//! Display formatting for list columns.

/// Formats a byte count the way Explorer does: binary units, but labelled KB/MB
/// rather than KiB/MiB, because that is what Windows users expect to see.
///
/// Directories pass `None` and render blank rather than `0 B`, since a folder's
/// own size is meaningless and showing zero implies emptiness.
pub fn size(bytes: Option<u64>) -> String {
    let Some(b) = bytes else {
        return String::new();
    };

    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if b < 1024 {
        return format!("{b} B");
    }

    let mut value = b as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    // One decimal below 10 keeps the column narrow while staying precise enough
    // to distinguish 1.2 GB from 1.9 GB.
    if value < 10.0 {
        format!("{:.1} {}", value, UNITS[unit])
    } else {
        format!("{:.0} {}", value, UNITS[unit])
    }
}

/// Formats a Unix-millisecond timestamp as `YYYY-MM-DD HH:MM`.
///
/// Sortable, unambiguous across locales, and fixed-width so the column never
/// jitters. Implemented directly rather than pulling in a date crate — this is
/// the only date formatting Neutron needs.
pub fn timestamp(millis: i64) -> String {
    // Filesystems do produce garbage timestamps; render them as blank rather
    // than as a misleading 1601 or 1970 date.
    if millis <= 0 {
        return String::new();
    }

    let secs = millis / 1000;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hour, minute) = (tod / 3600, (tod % 3600) / 60);
    let (y, m, d) = civil_from_days(days);

    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

/// Days since the Unix epoch to a calendar date.
///
/// Howard Hinnant's `civil_from_days`, which handles leap years and the
/// 100/400-year rules exactly. Shifting the era to start in March puts the leap
/// day at the end of the year, so no month-length special cases are needed.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_binary_units_with_windows_labels() {
        assert_eq!(size(Some(0)), "0 B");
        assert_eq!(size(Some(999)), "999 B");
        assert_eq!(size(Some(1024)), "1.0 KB");
        assert_eq!(size(Some(1536)), "1.5 KB");
        assert_eq!(size(Some(1024 * 1024)), "1.0 MB");
        assert_eq!(size(Some(15 * 1024 * 1024)), "15 MB");
        assert_eq!(size(Some(1024u64.pow(3))), "1.0 GB");
    }

    #[test]
    fn directories_render_blank_not_zero() {
        assert_eq!(size(None), "");
    }

    #[test]
    fn largest_unit_does_not_overflow() {
        // u64::MAX must land in PB rather than running off the unit table.
        let s = size(Some(u64::MAX));
        assert!(s.ends_with(" PB"), "got {s}");
    }

    #[test]
    fn timestamps_format_as_sortable_datetimes() {
        // 2021-01-01T00:00:00Z
        assert_eq!(timestamp(1_609_459_200_000), "2021-01-01 00:00");
        // 2024-02-29T12:34:00Z — leap day, the case naive implementations miss.
        assert_eq!(timestamp(1_709_210_040_000), "2024-02-29 12:34");
        // 2100-03-01 — 2100 is not a leap year despite being divisible by 4,
        // which is the rule a hand-rolled calendar most often gets wrong.
        assert_eq!(timestamp(4_107_542_400_000), "2100-03-01 00:00");
    }

    #[test]
    fn invalid_timestamps_render_blank() {
        assert_eq!(timestamp(0), "");
        assert_eq!(timestamp(-1), "");
    }

    #[test]
    fn epoch_boundary_is_correct() {
        assert_eq!(timestamp(1), "1970-01-01 00:00");
    }
}
