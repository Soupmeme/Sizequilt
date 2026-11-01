//! How numbers read on screen.

const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];

/// Binary sizes, as Explorer counts them on Windows, under the decimal-looking
/// names it uses: 1 GB here is 1,073,741,824 bytes.
pub fn bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    format!("{value:.decimals$} {}", UNITS[unit])
}

/// `1234567` as `1,234,567`.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A share of a whole as a percentage, one decimal under 10%.
pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "-".to_owned();
    }
    let p = part as f64 * 100.0 / whole as f64;
    if p >= 10.0 {
        format!("{p:.0}%")
    } else {
        format!("{p:.1}%")
    }
}

/// A duration in seconds, as a person would say it.
pub fn seconds(s: f64) -> String {
    if s < 10.0 {
        format!("{s:.2} s")
    } else if s < 60.0 {
        format!("{s:.1} s")
    } else {
        format!("{}m {:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    }
}

/// Milliseconds since the Unix epoch as a UTC date and time, to the second.
pub fn timestamp(ms: f64) -> String {
    let day_seconds = (ms / 1000.0).rem_euclid(86_400.0) as u64;
    let date = date(ms).unwrap_or_else(|| "unknown date".to_owned());
    let (h, m, s) = (day_seconds / 3600, day_seconds / 60 % 60, day_seconds % 60);
    format!("{date}T{h:02}:{m:02}:{s:02}Z")
}

/// Milliseconds since the Unix epoch as a UTC calendar date, `2026-11-01`.
pub fn date(ms: f64) -> Option<String> {
    if !ms.is_finite() || ms <= 0.0 {
        return None;
    }
    // Howard Hinnant's civil-from-days.
    let z = (ms / 86_400_000.0).floor() as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_explorer_counts_them() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.00 KB");
        assert_eq!(bytes(1536), "1.50 KB");
        assert_eq!(bytes(1024 * 1024 * 150), "150 MB");
        assert_eq!(bytes(1024_u64.pow(3) * 12), "12.0 GB");
    }

    #[test]
    fn counts_take_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn percent_of_nothing_is_a_dash() {
        assert_eq!(percent(1, 0), "-");
        assert_eq!(percent(1, 4), "25%");
        assert_eq!(percent(1, 200), "0.5%");
    }

    #[test]
    fn timestamps_keep_the_time_of_day() {
        assert_eq!(timestamp(90_061_000.0), "1970-01-02T01:01:01Z");
    }

    #[test]
    fn dates_are_calendar_dates() {
        assert_eq!(date(0.0), None);
        assert_eq!(date(f64::NAN), None);
        assert_eq!(date(86_400_000.0).as_deref(), Some("1970-01-02"));
        assert_eq!(date(1_793_491_200_000.0).as_deref(), Some("2026-11-01"));
    }
}
