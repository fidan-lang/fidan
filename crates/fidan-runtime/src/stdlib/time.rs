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
    let year = format!("{y:04}");
    let month = format!("{mo:02}");
    let day = format!("{d:02}");
    let hour = format!("{h:02}");
    let minute = format!("{mi:02}");
    let second = format!("{s:02}");
    let millis = format!("{:03}", ms.rem_euclid(1000) as u32);

    let mut out = String::with_capacity(fmt.len() + 8);
    let mut index = 0;
    while index < fmt.len() {
        let rest = &fmt[index..];
        let replacement = if rest.starts_with("%Y") {
            Some((year.as_str(), 2))
        } else if rest.starts_with("%m") {
            Some((month.as_str(), 2))
        } else if rest.starts_with("%d") {
            Some((day.as_str(), 2))
        } else if rest.starts_with("%H") {
            Some((hour.as_str(), 2))
        } else if rest.starts_with("%M") {
            Some((minute.as_str(), 2))
        } else if rest.starts_with("%S") {
            Some((second.as_str(), 2))
        } else if rest.starts_with("%L") {
            Some((millis.as_str(), 2))
        } else if rest.starts_with("YYYY") {
            Some((year.as_str(), 4))
        } else if rest.starts_with("MM") {
            Some((month.as_str(), 2))
        } else if rest.starts_with("DD") {
            Some((day.as_str(), 2))
        } else if rest.starts_with("HH") {
            Some((hour.as_str(), 2))
        } else if rest.starts_with("mm") {
            Some((minute.as_str(), 2))
        } else if rest.starts_with("ss") {
            Some((second.as_str(), 2))
        } else {
            None
        };

        if let Some((value, consumed)) = replacement {
            out.push_str(value);
            index += consumed;
        } else {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            index += ch.len_utf8();
        }
    }

    out
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

#[cfg(test)]
mod tests {
    use super::{dispatch, format_with};
    use crate::{FidanString, FidanValue};

    fn dispatch_string(name: &str, args: Vec<FidanValue>) -> String {
        match dispatch(name, args) {
            Some(FidanValue::String(value)) => value.as_str().to_string(),
            other => panic!("expected string result, got {other:?}"),
        }
    }

    #[test]
    fn format_supports_percent_and_named_tokens_in_one_pass() {
        let formatted = dispatch_string(
            "format",
            vec![
                FidanValue::Integer(1_735_689_904_321),
                FidanValue::String(FidanString::new("%Y-%m-%d HH:mm:ss.%L")),
            ],
        );
        assert_eq!(formatted, "2025-01-01 00:05:04.321");
    }

    #[test]
    fn format_uses_positive_millisecond_component_for_negative_timestamps() {
        assert_eq!(
            format_with(-1, "%Y-%m-%d %H:%M:%S.%L"),
            "1969-12-31 23:59:59.999"
        );
    }
}
