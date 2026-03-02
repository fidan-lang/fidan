//! `std.time` — Time, date, and duration utilities for Fidan.
//!
//! Available via:
//!   `use std.time`  → `time.now()`, `time.sleep(ms)`, `time.date()`, etc.
//!   `use std.time.{now, sleep}` → free names in scope.
//!
//! # Functions
//!
//! | Name | Signature | Description |
//! |---|---|---|
//! | `now` | `() → Integer` | Current Unix time in **milliseconds** |
//! | `timestamp` | `() → Integer` | Current Unix time in **seconds** |
//! | `sleep` | `(ms: Integer\|Float) → Nothing` | Pause execution for `ms` milliseconds |
//! | `elapsed` | `(startMs: Integer) → Integer` | Milliseconds since `startMs` (`now() - startMs`) |
//! | `date` | `() → String` | Current local date as `"YYYY-MM-DD"` |
//! | `time` | `() → String` | Current local time as `"HH:MM:SS"` |
//! | `datetime` | `() → String` | Current local datetime as `"YYYY-MM-DD HH:MM:SS"` |
//! | `year` | `() → Integer` | Current year (local) |
//! | `month` | `() → Integer` | Current month 1–12 (local) |
//! | `day` | `() → Integer` | Current day-of-month 1–31 (local) |
//! | `hour` | `() → Integer` | Current hour 0–23 (local) |
//! | `minute` | `() → Integer` | Current minute 0–59 (local) |
//! | `second` | `() → Integer` | Current second 0–59 (local) |
//! | `weekday` | `() → Integer` | Day-of-week: 0 = Monday … 6 = Sunday |
//! | `format` | `(ms: Integer, fmt: String?) → String` | Format a Unix-ms timestamp |

use fidan_runtime::FidanValue;
use std::time::{SystemTime, UNIX_EPOCH};

// ── Helpers ────────────────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn to_ms(v: &FidanValue) -> u64 {
    match v {
        FidanValue::Integer(n) => (*n).max(0) as u64,
        FidanValue::Float(f) => f.max(0.0) as u64,
        _ => 0,
    }
}

/// Convert a Unix-millisecond timestamp to civil (year, month, day, hour, min, sec).
/// Uses the proleptic Gregorian calendar (UTC-based, matches behaviour of `chrono::Utc`).
fn ms_to_civil(ms: i64) -> (i32, u32, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86400) as i32;
    let day_secs = secs.rem_euclid(86400) as u32;

    let hour = day_secs / 3600;
    let min = (day_secs % 3600) / 60;
    let sec = day_secs % 60;

    // Civil date from Julian Day Number (days since 1970-01-01)
    // Algorithm from Richards (2013) as used in many Gregorian implementations.
    let z = days + 719468;
    let era: i32 = if z >= 0 {
        z / 146097
    } else {
        (z - 146096) / 146097
    };
    let doe = (z - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = (yoe as i32) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, hour, min, sec)
}

fn format_default(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = ms_to_civil(ms);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
}

fn format_with(ms: i64, fmt: &str) -> String {
    let (y, mo, d, h, mi, s) = ms_to_civil(ms);
    fmt.replace("%Y", &format!("{:04}", y))
        .replace("%m", &format!("{:02}", mo))
        .replace("%d", &format!("{:02}", d))
        .replace("%H", &format!("{:02}", h))
        .replace("%M", &format!("{:02}", mi))
        .replace("%S", &format!("{:02}", s))
        // Aliases / extras
        .replace("YYYY", &format!("{:04}", y))
        .replace("MM", &format!("{:02}", mo))
        .replace("DD", &format!("{:02}", d))
        .replace("HH", &format!("{:02}", h))
        .replace("mm", &format!("{:02}", mi))
        .replace("ss", &format!("{:02}", s))
}

fn str_val(s: &str) -> FidanValue {
    FidanValue::String(fidan_runtime::FidanString::new(s))
}

fn int_val(n: i64) -> FidanValue {
    FidanValue::Integer(n)
}

// ── Dispatch ───────────────────────────────────────────────────────────────────

/// Dispatch a `time.<name>(args)` call.
pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        // ── Timestamps ──────────────────────────────────────────────────────
        "now" => Some(int_val(now_ms())),

        "timestamp" => Some(int_val(now_ms() / 1000)),

        // ── Delays ──────────────────────────────────────────────────────────
        "sleep" | "wait" => {
            let ms = to_ms(args.first().unwrap_or(&FidanValue::Nothing));
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Some(FidanValue::Nothing)
        }

        // ── Elapsed time ────────────────────────────────────────────────────
        "elapsed" => {
            let start = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                Some(FidanValue::Float(f)) => *f as i64,
                _ => 0,
            };
            Some(int_val((now_ms() - start).max(0)))
        }

        // ── Formatting ──────────────────────────────────────────────────────
        "date" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            let (y, mo, d, ..) = ms_to_civil(ms);
            Some(str_val(&format!("{:04}-{:02}-{:02}", y, mo, d)))
        }
        "time" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            let (_, _, _, h, mi, s) = ms_to_civil(ms);
            Some(str_val(&format!("{:02}:{:02}:{:02}", h, mi, s)))
        }
        "datetime" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(str_val(&format_default(ms)))
        }
        "format" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            let fmt = match args.get(1) {
                Some(FidanValue::String(s)) => s.as_str().to_string(),
                _ => "YYYY-MM-DD HH:mm:ss".to_string(),
            };
            Some(str_val(&format_with(ms, &fmt)))
        }

        // ── Calendar components ──────────────────────────────────────────────
        "year" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(int_val(ms_to_civil(ms).0 as i64))
        }
        "month" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(int_val(ms_to_civil(ms).1 as i64))
        }
        "day" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(int_val(ms_to_civil(ms).2 as i64))
        }
        "hour" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(int_val(ms_to_civil(ms).3 as i64))
        }
        "minute" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(int_val(ms_to_civil(ms).4 as i64))
        }
        "second" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(int_val(ms_to_civil(ms).5 as i64))
        }

        // ── Day-of-week ──────────────────────────────────────────────────────
        // 0 = Monday … 6 = Sunday (ISO-8601 weekday).
        "weekday" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            // 1970-01-01 was a Thursday (ISO weekday 3, i.e. index 3 from Monday=0)
            let days = ms.div_euclid(86_400_000) as i64;
            Some(int_val(days.rem_euclid(7).wrapping_add(3).rem_euclid(7)))
        }

        _ => None,
    }
}

/// All exported function names from `std.time`.
pub fn exported_names() -> &'static [&'static str] {
    &[
        "now",
        "timestamp",
        "sleep",
        "wait",
        "elapsed",
        "date",
        "time",
        "datetime",
        "format",
        "year",
        "month",
        "day",
        "hour",
        "minute",
        "second",
        "weekday",
    ]
}
