use std::time::{SystemTime, UNIX_EPOCH};

/// New ULID as text — the primary-key format everywhere (Spec 01 §2).
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

/// Current UTC timestamp, ISO-8601 with Z (Spec 01 §2).
/// Std-only: civil-date conversion (Howard Hinnant's algorithm), no chrono dependency.
pub fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// (year, month, day) → days since 1970-01-01 (Howard Hinnant's algorithm),
/// the inverse of `civil_from_days`. `pub` since `reports.rs` (day bucketing)
/// and the shell's backup scheduler (elapsed-time checks) both need it —
/// one proleptic-calendar implementation, not two copies drifting apart.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe.div_euclid(4) - yoe.div_euclid(100) + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `now_iso` — total seconds since the epoch for one of this
/// crate's own `"YYYY-MM-DDTHH:MM:SSZ"` timestamps. Used for elapsed-time
/// checks (e.g. the backup scheduler's "> 4h since last success"), not
/// financial dates — those stay plain `YYYY-MM-DD` string comparison
/// throughout the crate, which sorts and compares correctly without this.
pub fn parse_iso_to_epoch_secs(ts: &str) -> i64 {
    let y: i64 = ts[0..4].parse().unwrap_or(1970);
    let mo: u32 = ts[5..7].parse().unwrap_or(1);
    let d: u32 = ts[8..10].parse().unwrap_or(1);
    let h: i64 = ts.get(11..13).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mi: i64 = ts.get(14..16).and_then(|s| s.parse().ok()).unwrap_or(0);
    let s: i64 = ts.get(17..19).and_then(|s| s.parse().ok()).unwrap_or(0);
    days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s
}

/// days since 1970-01-01 → (year, month, day) in the proleptic Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // year of era
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month index, March-based [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if mo <= 2 { y + 1 } else { y }, mo, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // leap year start
        assert_eq!(civil_from_days(19_782), (2024, 2, 29)); // leap day
        assert_eq!(civil_from_days(20_637), (2026, 7, 3));
    }

    #[test]
    fn now_iso_shape() {
        let ts = now_iso();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
