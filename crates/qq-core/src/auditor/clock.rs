//! Civil-date math and the report's local-time stamps.

use std::process::Command;

pub fn now_sec() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u64;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe as i64 - 719_468).max(0) as u64
}

/// `(year, month, day)` of a day count since 1970-01-01.
pub fn ymd_from_days(days: u64) -> (u64, u64, u64) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y as u64, m as u64, d as u64)
}

/// `date +<fmt>`: the filename hour is the machine's local time. Falls back to UTC.
fn date(fmt: &str, utc_fallback: impl Fn(u64) -> String) -> String {
    if let Ok(o) = Command::new("date").arg(format!("+{fmt}")).output() {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && !s.is_empty() {
            return s;
        }
    }
    utc_fallback(now_sec())
}

/// `YYYY-MM-DD-HH`, the report file stamp.
pub fn date_hour() -> String {
    date("%Y-%m-%d-%H", |s| {
        let (y, m, d) = ymd_from_days(s / 86_400);
        format!("{y:04}-{m:02}-{d:02}-{:02}", s % 86_400 / 3600)
    })
}

/// `YYYY-MM-DD HH:MM`, for the FAILED report.
pub fn date_minute() -> String {
    date("%Y-%m-%d %H:%M", |s| {
        let (y, m, d) = ymd_from_days(s / 86_400);
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}",
            s % 86_400 / 3600,
            s % 3600 / 60
        )
    })
}
