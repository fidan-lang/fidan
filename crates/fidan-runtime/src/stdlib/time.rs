use std::time::{SystemTime, UNIX_EPOCH};

use crate::FidanValue;

use super::common::string_value;

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

fn ms_to_civil(ms: i64) -> (i32, u32, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86400) as i32;
    let day_secs = secs.rem_euclid(86400) as u32;

    let hour = day_secs / 3600;
    let min = (day_secs % 3600) / 60;
    let sec = day_secs % 60;

    let z = days + 719468;
    let era: i32 = if z >= 0 {
        z / 146097
    } else {
        (z - 146096) / 146097
    };
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i32) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, hour, min, sec)
}

fn format_default(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = ms_to_civil(ms);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

fn format_with(ms: i64, fmt: &str) -> String {
    let (y, mo, d, h, mi, s) = ms_to_civil(ms);
    fmt.replace("%Y", &format!("{y:04}"))
        .replace("%m", &format!("{mo:02}"))
        .replace("%d", &format!("{d:02}"))
        .replace("%H", &format!("{h:02}"))
        .replace("%M", &format!("{mi:02}"))
        .replace("%S", &format!("{s:02}"))
        .replace("%L", &format!("{:03}", (ms.abs() % 1000) as u32))
        .replace("YYYY", &format!("{y:04}"))
        .replace("MM", &format!("{mo:02}"))
        .replace("DD", &format!("{d:02}"))
        .replace("HH", &format!("{h:02}"))
        .replace("mm", &format!("{mi:02}"))
        .replace("ss", &format!("{s:02}"))
}

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "now" => Some(FidanValue::Integer(now_ms())),
        "timestamp" => Some(FidanValue::Integer(now_ms() / 1000)),
        "sleep" | "wait" => {
            std::thread::sleep(std::time::Duration::from_millis(to_ms(
                args.first().unwrap_or(&FidanValue::Nothing),
            )));
            Some(FidanValue::Nothing)
        }
        "elapsed" => {
            let start = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                Some(FidanValue::Float(f)) => *f as i64,
                _ => 0,
            };
            Some(FidanValue::Integer((now_ms() - start).max(0)))
        }
        "date" | "today" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            let (y, mo, d, ..) = ms_to_civil(ms);
            Some(string_value(&format!("{y:04}-{mo:02}-{d:02}")))
        }
        "time" | "timeStr" | "time_str" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            let (_, _, _, h, mi, s) = ms_to_civil(ms);
            Some(string_value(&format!("{h:02}:{mi:02}:{s:02}")))
        }
        "datetime" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(string_value(&format_default(ms)))
        }
        "format" | "formatDate" | "format_date" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            let fmt = match args.get(1) {
                Some(FidanValue::String(s)) => s.as_str().to_string(),
                _ => "YYYY-MM-DD HH:mm:ss".to_string(),
            };
            Some(string_value(&format_with(ms, &fmt)))
        }
        "year" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(FidanValue::Integer(ms_to_civil(ms).0 as i64))
        }
        "month" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(FidanValue::Integer(ms_to_civil(ms).1 as i64))
        }
        "day" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(FidanValue::Integer(ms_to_civil(ms).2 as i64))
        }
        "hour" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(FidanValue::Integer(ms_to_civil(ms).3 as i64))
        }
        "minute" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(FidanValue::Integer(ms_to_civil(ms).4 as i64))
        }
        "second" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            Some(FidanValue::Integer(ms_to_civil(ms).5 as i64))
        }
        "weekday" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => now_ms(),
            };
            let days = ms.div_euclid(86_400_000);
            Some(FidanValue::Integer(
                days.rem_euclid(7).wrapping_add(3).rem_euclid(7),
            ))
        }
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "now",
        "timestamp",
        "sleep",
        "wait",
        "elapsed",
        "date",
        "today",
        "time",
        "timeStr",
        "time_str",
        "datetime",
        "format",
        "formatDate",
        "format_date",
        "year",
        "month",
        "day",
        "hour",
        "minute",
        "second",
        "weekday",
    ]
}
