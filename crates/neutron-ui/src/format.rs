//! Display formatting for list columns.

/// Shortcuts as the platform writes them: `Ctrl+Shift+D` is `⌘⇧D` on a Mac.
///
/// Takes any text, so a tooltip that mentions a shortcut goes through whole.
pub fn shortcut(text: &str) -> String {
    if cfg!(target_os = "macos") {
        text.replace("Ctrl+", "⌘")
            .replace("Cmd+", "⌘")
            .replace("Shift+", "⇧")
            .replace("Alt+", "⌥")
    } else {
        text.to_owned()
    }
}

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

/// Formats a Unix-millisecond timestamp the way a person says it, in local
/// time: "Today, 10:24", "Yesterday, 18:02", or "9 Oct 2026" for anything
/// older.
///
/// Sorting never reads this text — a column sorts by the number underneath —
/// so it can be written for reading rather than for lining up. It used to be
/// a fixed-width `YYYY-MM-DD HH:MM` in UTC, which was an hour or more wrong for
/// nearly everyone and asked them to do date arithmetic to see that a file was
/// changed this morning.
pub fn timestamp(millis: i64) -> String {
    // Filesystems do produce garbage timestamps; render them as blank rather
    // than as a misleading 1601 or 1970 date.
    if millis <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(millis);
    timestamp_relative(millis, now, local_offset_secs(millis / 1000))
}

/// [`timestamp`] against a given `now` and offset from UTC, for testing.
pub fn timestamp_relative(millis: i64, now_millis: i64, offset_secs: i64) -> String {
    if millis <= 0 {
        return String::new();
    }
    let local = millis / 1000 + offset_secs;
    let day = local.div_euclid(86_400);
    let today = (now_millis / 1000 + offset_secs).div_euclid(86_400);
    let tod = local.rem_euclid(86_400);
    let (hour, minute) = (tod / 3600, (tod % 3600) / 60);

    if day == today {
        return format!("Today, {hour:02}:{minute:02}");
    }
    if day == today - 1 {
        return format!("Yesterday, {hour:02}:{minute:02}");
    }
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, m, d) = civil_from_days(day);
    format!("{d} {} {y}", MONTHS[(m as usize).saturating_sub(1).min(11)])
}

/// The local offset from UTC, in seconds, at the instant `at` (seconds since
/// the epoch). Asked of the C library, which knows the zone and its daylight
/// saving rules; zero where it cannot be asked.
pub fn local_offset_secs(at: i64) -> i64 {
    #[cfg(unix)]
    {
        let time = at as libc::time_t;
        // SAFETY: `localtime_r` writes only into the `tm` it is given, and
        // both pointers are to live locals for the duration of the call.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let filled = unsafe { libc::localtime_r(&time, &mut tm) };
        if !filled.is_null() {
            return tm.tm_gmtoff as i64;
        }
    }
    let _ = at;
    0
}

/// The hour of the day where the user is, 0–23.
pub fn local_hour() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ((now + local_offset_secs(now)).rem_euclid(86_400) / 3600) as u32
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
    fn recent_timestamps_say_today_or_yesterday() {
        // 2026-10-14 12:00 UTC as "now", in UTC.
        let now = 1_791_979_200_000;
        assert_eq!(timestamp_relative(now - 3_600_000, now, 0), "Today, 11:00");
        assert_eq!(timestamp_relative(now - 86_400_000, now, 0), "Yesterday, 12:00");
        assert_eq!(timestamp_relative(1_609_459_200_000, now, 0), "1 Jan 2021");
        assert_eq!(timestamp_relative(1_709_210_040_000, now, 0), "29 Feb 2024");
    }

    #[test]
    fn the_local_offset_moves_the_day_boundary() {
        // 23:30 UTC is already tomorrow ten hours east.
        let now = 1_791_979_200_000; // 12:00 UTC
        let late = now + 11 * 3_600_000 + 1_800_000; // 23:30 UTC the same day
        assert_eq!(timestamp_relative(late, now, 0), "Today, 23:30");
        assert_eq!(timestamp_relative(late, now, 10 * 3600), "15 Oct 2026");
    }

    #[test]
    fn invalid_timestamps_render_blank() {
        assert_eq!(timestamp(0), "");
        assert_eq!(timestamp(-1), "");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn shortcuts_are_written_with_the_mac_glyphs() {
        assert_eq!(shortcut("Ctrl+Shift+D"), "⌘⇧D");
        assert_eq!(shortcut("Go back (Alt+Left)"), "Go back (⌥Left)");
        assert_eq!(shortcut("F2"), "F2");
    }
}
