//! Context-free scalar function implementations.

use anyhow::{bail, Result};

use crate::errors::{code, XtlError};
use crate::eval::{coerce_number, is_truthy};
use crate::value::Value;

/// Raise an `XtlError` with the canonical `xl3/eval/arity-mismatch`
/// code carrying the same message format the spec-conformance fixtures
/// match on.
fn arity_err(message: impl Into<String>) -> anyhow::Error {
    XtlError::new(code::EVAL_ARITY_MISMATCH, message).into()
}

const SECONDS_PER_DAY: f64 = 86_400.0;
const EXCEL_EPOCH_DAYS: i64 = -25_569;
const EXCEL_FAKE_LEAP_DAY_SERIAL: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExcelDateTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    fake_leap_day: bool,
}

pub fn call_scalar(name: &str, args: &[Value]) -> Result<Value> {
    // xl3 fixture 116: function names are case-insensitive (IF == if).
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "IF" => {
            if args.len() != 3 {
                return Err(arity_err(format!("IF: expected 3 arguments, got {}", args.len())));
            }
            Ok(if is_truthy(&args[0]) {
                args[1].clone()
            } else {
                args[2].clone()
            })
        }
        "ROUND" => {
            if args.len() != 2 {
                return Err(arity_err(format!("ROUND: expected 2 arguments, got {}", args.len())));
            }
            let n = coerce_number(&args[0])?;
            let d = coerce_number(&args[1])? as i32;
            Ok(Value::Number(round_half_away_from_zero(n, d)))
        }
        "ABS" => {
            if args.len() != 1 {
                return Err(arity_err(format!("ABS: expected 1 argument, got {}", args.len())));
            }
            Ok(Value::Number(coerce_number(&args[0])?.abs()))
        }
        "TEXT" => text(args),
        "YEAR" => year(args),
        "MONTH" => month(args),
        "DAY" => day(args),
        "EOMONTH" => eomonth(args),
        "EDATE" => edate(args),
        "DATE" => {
            if args.len() != 3 {
                return Err(arity_err(format!("DATE: expected 3 arguments, got {}", args.len())));
            }
            let y = integer_arg("DATE year", &args[0])?;
            let m = integer_arg("DATE month", &args[1])?;
            let d = integer_arg("DATE day", &args[2])?;
            // Excel DATE is lenient with out-of-range month/day, but the
            // corpus only exercises in-range values; reject obvious bad
            // input rather than silently coercing.
            if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
                bail!("DATE({y},{m},{d}) — month/day out of range");
            }
            Ok(Value::Number(date_to_excel_serial(y, m as u32, d as u32)?))
        }
        "IFERROR" => {
            if args.len() != 2 {
                return Err(arity_err(format!("IFERROR: expected 2 arguments, got {}", args.len())));
            }
            // Our error model surfaces division-by-zero / other failures
            // as Value::Empty (ADR-0025 stage-1). IFERROR replaces Empty
            // results with the fallback. A first-class Value::Error
            // variant would let us distinguish "blank" from "errored"
            // — future work.
            Ok(if matches!(&args[0], Value::Empty) {
                args[1].clone()
            } else {
                args[0].clone()
            })
        }
        "UPPER" => {
            if args.len() != 1 {
                return Err(arity_err(format!("UPPER: expected 1 argument, got {}", args.len())));
            }
            Ok(Value::String(args[0].canonical().to_uppercase()))
        }
        "LOWER" => {
            if args.len() != 1 {
                return Err(arity_err(format!("LOWER: expected 1 argument, got {}", args.len())));
            }
            Ok(Value::String(args[0].canonical().to_lowercase()))
        }
        "TRIM" => {
            if args.len() != 1 {
                return Err(arity_err(format!("TRIM: expected 1 argument, got {}", args.len())));
            }
            Ok(Value::String(args[0].canonical().trim().to_string()))
        }
        "LEN" => {
            if args.len() != 1 {
                return Err(arity_err(format!("LEN: expected 1 argument, got {}", args.len())));
            }
            Ok(Value::Number(args[0].canonical().chars().count() as f64))
        }
        "CONCAT" => {
            let mut s = String::new();
            for a in args {
                s.push_str(&a.canonical());
            }
            Ok(Value::String(s))
        }
        "HYPERLINK" => {
            if args.is_empty() || args.len() > 2 {
                return Err(arity_err("HYPERLINK: expected 1 or 2 arguments"));
            }
            // Stage-1 conformance (cell-value comparison) only needs
            // the display label (arg 2, falling back to the URL when
            // unset). xl3's `XtlHyperlinkCell` marker — which adds the
            // actual link record to the output cell — is a Stage-2
            // concern; revisit when manifest preservation lands.
            Ok(args.last().cloned().unwrap_or(Value::Empty))
        }
        "ISBLANK" => {
            if args.len() != 1 {
                return Err(arity_err(format!("ISBLANK: expected 1 argument, got {}", args.len())));
            }
            // xl3 fixture 130: ISBLANK is true for Empty and for
            // whitespace-only strings. Same definition as the source
            // reader's row-skip logic (ADR-0007).
            Ok(Value::Bool(crate::source::is_blank_value(&args[0])))
        }
        "IFEMPTY" => {
            if args.len() != 2 {
                return Err(arity_err(format!("IFEMPTY: expected 2 arguments, got {}", args.len())));
            }
            Ok(if is_empty_for_ifempty(&args[0]) {
                args[1].clone()
            } else {
                args[0].clone()
            })
        }
        "IFS" => {
            // IFS(cond1, val1, cond2, val2, ..., [default]) — first
            // truthy cond's val wins. xl3 allows the trailing arg to be
            // a bare default; we follow that.
            if args.is_empty() {
                bail!("IFS expects at least one (cond, value) pair");
            }
            let mut i = 0;
            while i + 1 < args.len() {
                if is_truthy(&args[i]) {
                    return Ok(args[i + 1].clone());
                }
                i += 2;
            }
            // Odd-length: last arg is the default.
            if args.len() % 2 == 1 {
                return Ok(args[args.len() - 1].clone());
            }
            // No condition matched and no default — xl3 emits an error
            // cell, but for Phase 1 we mirror Excel and return empty.
            Ok(Value::Empty)
        }
        "MAX" => {
            if args.is_empty() {
                return Err(arity_err("MAX expects at least one argument"));
            }
            let mut best = f64::NEG_INFINITY;
            for a in args {
                best = best.max(coerce_number(a)?);
            }
            Ok(Value::Number(best))
        }
        "MIN" => {
            if args.is_empty() {
                return Err(arity_err("MIN expects at least one argument"));
            }
            let mut best = f64::INFINITY;
            for a in args {
                best = best.min(coerce_number(a)?);
            }
            Ok(Value::Number(best))
        }
        "SUM" => {
            let mut acc = 0f64;
            for a in args {
                acc += coerce_number(a)?;
            }
            Ok(Value::Number(acc))
        }
        "NOT" => {
            if args.len() != 1 {
                return Err(arity_err(format!("NOT: expected 1 argument, got {}", args.len())));
            }
            Ok(Value::Bool(!is_truthy(&args[0])))
        }
        "AND" => {
            for a in args {
                if !is_truthy(a) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        "OR" => {
            for a in args {
                if is_truthy(a) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        "TODAY" => {
            if !args.is_empty() {
                return Err(arity_err(format!("TODAY: expected no arguments, got {}", args.len())));
            }
            // ADR-0005: TODAY() is the current UTC calendar date as an
            // Excel serial. We avoid pulling in `chrono` for this one
            // call — read the system clock, convert to civil date with
            // the same Howard Hinnant algorithm the corpus runner uses.
            Ok(Value::Number(today_utc_serial()))
        }
        other => bail!("unknown function {other}"),
    }
}

/// TEXT(value, format) returns a string rendered with supported date, number, or @ formats.
fn text(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(arity_err(format!("TEXT: expected 2 arguments, got {}", args.len())));
    }
    let fmt = args[1].canonical();
    if let Value::String(s) = &args[0] {
        if fmt.contains('@') {
            return Ok(Value::String(fmt.replace('@', s)));
        }
    }
    if is_text_date_format(&fmt) {
        if let Value::Number(n) = args[0] {
            let d = excel_serial_to_datetime(n)?;
            return Ok(Value::String(format_date_text(d, &fmt)));
        }
    }
    if is_numeric_format(&fmt) {
        return Ok(Value::String(format_number(&args[0], &fmt)?));
    }
    Ok(Value::String(args[0].canonical()))
}

/// YEAR(date_serial) returns the Gregorian year component of an Excel serial date.
fn year(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(arity_err(format!("YEAR: expected 1 argument, got {}", args.len())));
    }
    Ok(Value::Number(excel_date_arg("YEAR", &args[0])?.year as f64))
}

/// MONTH(date_serial) returns the 1-based month component of an Excel serial date.
fn month(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(arity_err(format!("MONTH: expected 1 argument, got {}", args.len())));
    }
    Ok(Value::Number(
        excel_date_arg("MONTH", &args[0])?.month as f64,
    ))
}

/// DAY(date_serial) returns the day-of-month component of an Excel serial date.
fn day(args: &[Value]) -> Result<Value> {
    if args.len() != 1 {
        return Err(arity_err(format!("DAY: expected 1 argument, got {}", args.len())));
    }
    Ok(Value::Number(excel_date_arg("DAY", &args[0])?.day as f64))
}

/// EOMONTH(start_serial, months) returns the serial of the target month's last day.
fn eomonth(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(arity_err(format!("EOMONTH: expected 2 arguments, got {}", args.len())));
    }
    let d = excel_date_arg("EOMONTH", &args[0])?;
    let months = integer_arg("EOMONTH", &args[1])?;
    let (year, month) = add_months(d.year, d.month, months)?;
    Ok(Value::Number(date_to_excel_serial(
        year,
        month,
        days_in_excel_month(year, month),
    )?))
}

/// EDATE(start_serial, months) returns the same day in the target month, clamped to month end.
fn edate(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(arity_err(format!("EDATE: expected 2 arguments, got {}", args.len())));
    }
    let d = excel_date_arg("EDATE", &args[0])?;
    let months = integer_arg("EDATE", &args[1])?;
    let (year, month) = add_months(d.year, d.month, months)?;
    let day = d.day.min(days_in_excel_month(year, month));
    Ok(Value::Number(date_to_excel_serial(year, month, day)?))
}

fn is_empty_for_ifempty(v: &Value) -> bool {
    match v {
        Value::Empty => true,
        // xl3 (ADR-0025/0028): treat ASCII whitespace-only strings as
        // empty for IFEMPTY. Numbers and booleans (including 0 / false)
        // are explicitly NOT empty.
        Value::String(s) => s.chars().all(|c| c.is_ascii_whitespace()),
        Value::Number(_)
        | Value::DateNumber(_)
        | Value::Bool(_)
        | Value::Rows(_)
        | Value::Map(_)
        | Value::List(_) => false,
    }
}

fn excel_date_arg(name: &str, v: &Value) -> Result<ExcelDateTime> {
    let serial = coerce_number(v)?;
    excel_serial_to_datetime(serial).map_err(|e| anyhow::anyhow!("{name} expected a date: {e}"))
}

fn integer_arg(name: &str, v: &Value) -> Result<i32> {
    let n = coerce_number(v)?;
    if !n.is_finite() || n.fract() != 0.0 || n < i32::MIN as f64 || n > i32::MAX as f64 {
        bail!("{name} expected an integer month offset");
    }
    Ok(n as i32)
}

/// Public helper used by `eval::compare` to render an Excel serial date
/// as a `YYYY-MM-DD` string for ADR-0017 (number ↔ date-string)
/// comparison.
pub fn serial_to_iso_date(serial: f64) -> Option<String> {
    if !serial.is_finite() {
        return None;
    }
    let d = excel_serial_to_datetime(serial).ok()?;
    Some(format!("{:04}-{:02}-{:02}", d.year, d.month, d.day))
}

/// Parse the ADR-0017 canonical form (`YYYY-MM-DD` or
/// `YYYY-MM-DDTHH:MM:SS`) back into an Excel serial. Returns `None`
/// for any other shape so the caller can fall back to a numeric parse.
pub fn iso_string_to_serial(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: i32 = std::str::from_utf8(&b[..4]).ok()?.parse().ok()?;
    let month: u32 = std::str::from_utf8(&b[5..7]).ok()?.parse().ok()?;
    let day: u32 = std::str::from_utf8(&b[8..10]).ok()?.parse().ok()?;
    let base = date_to_excel_serial(year, month, day).ok()?;
    if b.len() == 10 {
        return Some(base);
    }
    if b.len() >= 19 && b[10] == b'T' && b[13] == b':' && b[16] == b':' {
        let hour: u32 = std::str::from_utf8(&b[11..13]).ok()?.parse().ok()?;
        let minute: u32 = std::str::from_utf8(&b[14..16]).ok()?.parse().ok()?;
        let second: u32 = std::str::from_utf8(&b[17..19]).ok()?.parse().ok()?;
        let frac = (hour * 3600 + minute * 60 + second) as f64 / SECONDS_PER_DAY;
        return Some(base + frac);
    }
    None
}

/// Render an Excel serial as ADR-0017 canonical: `YYYY-MM-DD` when the
/// time component is exactly midnight, `YYYY-MM-DDTHH:MM:SS` otherwise.
pub fn serial_to_iso_canonical(serial: f64) -> Option<String> {
    if !serial.is_finite() {
        return None;
    }
    let d = excel_serial_to_datetime(serial).ok()?;
    let date = format!("{:04}-{:02}-{:02}", d.year, d.month, d.day);
    if d.hour == 0 && d.minute == 0 && d.second == 0 {
        Some(date)
    } else {
        Some(format!(
            "{date}T{:02}:{:02}:{:02}",
            d.hour, d.minute, d.second
        ))
    }
}

fn excel_serial_to_datetime(serial: f64) -> Result<ExcelDateTime> {
    if !serial.is_finite() {
        bail!("date serial must be finite");
    }

    let mut whole = serial.floor() as i64;
    // Round, not floor: a serial like 46150.39583333333 corresponds to
    // 09:30:00 exactly, but its fractional part times 86400 is
    // 34199.9999997… due to f64 precision. Floor + tiny epsilon would
    // bias toward truncation; `.round()` lands on 34200 (= 09:30:00).
    let mut seconds =
        ((serial - serial.floor()) * SECONDS_PER_DAY).round() as i64;
    if seconds >= SECONDS_PER_DAY as i64 {
        whole += 1;
        seconds -= SECONDS_PER_DAY as i64;
    }

    let (year, month, day, fake_leap_day) = if whole == EXCEL_FAKE_LEAP_DAY_SERIAL {
        (1900, 2, 29, true)
    } else if whole == 0 {
        (1899, 12, 30, false)
    } else if (1..EXCEL_FAKE_LEAP_DAY_SERIAL).contains(&whole) {
        let (y, m, d) = civil_from_days(EXCEL_EPOCH_DAYS + whole + 1);
        (y, m, d, false)
    } else {
        let (y, m, d) = civil_from_days(EXCEL_EPOCH_DAYS + whole);
        (y, m, d, false)
    };

    Ok(ExcelDateTime {
        year,
        month,
        day,
        hour: (seconds / 3600) as u32,
        minute: ((seconds / 60) % 60) as u32,
        second: (seconds % 60) as u32,
        fake_leap_day,
    })
}

fn today_utc_serial() -> f64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days_since_unix = secs.div_euclid(86_400);
    // Days since 1970-01-01 → civil date (Howard Hinnant).
    let z = days_since_unix + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { (y + 1) as i32 } else { y as i32 };
    date_to_excel_serial(y, m, d).unwrap_or(0.0)
}

fn date_to_excel_serial(year: i32, month: u32, day: u32) -> Result<f64> {
    if year == 1900 && month == 2 && day == 29 {
        return Ok(EXCEL_FAKE_LEAP_DAY_SERIAL as f64);
    }
    if !valid_gregorian_date(year, month, day) {
        bail!("invalid date {year:04}-{month:02}-{day:02}");
    }
    let days = days_from_civil(year, month, day);
    let serial = if days >= days_from_civil(1900, 3, 1) {
        days - EXCEL_EPOCH_DAYS
    } else if days >= days_from_civil(1900, 1, 1) {
        days - EXCEL_EPOCH_DAYS - 1
    } else {
        days - EXCEL_EPOCH_DAYS
    };
    Ok(serial as f64)
}

fn is_text_date_format(fmt: &str) -> bool {
    let lower = fmt.to_ascii_lowercase();
    ["yyyy", "yy", "mm", "dd", "hh", "ss"]
        .iter()
        .any(|token| lower.contains(token))
        || lower.contains('m')
        || lower.contains('d')
}

fn format_date_text(d: ExcelDateTime, fmt: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < fmt.len() {
        if starts_with_ci(fmt, i, "yyyy") {
            out.push_str(&format!("{:04}", d.year));
            i += 4;
        } else if starts_with_ci(fmt, i, "yy") {
            out.push_str(&format!("{:02}", d.year.rem_euclid(100)));
            i += 2;
        } else if starts_with_ci(fmt, i, "dd") {
            out.push_str(&format!("{:02}", d.day));
            i += 2;
        } else if starts_with_ci(fmt, i, "d") {
            out.push_str(&d.day.to_string());
            i += 1;
        } else if starts_with_ci(fmt, i, "hh") {
            out.push_str(&format!("{:02}", d.hour));
            i += 2;
        } else if starts_with_ci(fmt, i, "mm") {
            if is_minute_position(fmt, i) {
                out.push_str(&format!("{:02}", d.minute));
            } else {
                out.push_str(&format!("{:02}", d.month));
            }
            i += 2;
        } else if starts_with_ci(fmt, i, "m") {
            if is_minute_position(fmt, i) {
                out.push_str(&d.minute.to_string());
            } else {
                out.push_str(&d.month.to_string());
            }
            i += 1;
        } else if starts_with_ci(fmt, i, "ss") {
            out.push_str(&format!("{:02}", d.second));
            i += 2;
        } else {
            let ch = fmt[i..].chars().next().expect("valid char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn starts_with_ci(s: &str, idx: usize, token: &str) -> bool {
    s.get(idx..idx + token.len())
        .map(|part| part.eq_ignore_ascii_case(token))
        .unwrap_or(false)
}

fn is_minute_position(fmt: &str, idx: usize) -> bool {
    if idx == 0 || !fmt.as_bytes().get(idx - 1).is_some_and(|b| *b == b':') {
        return false;
    }
    fmt[..idx]
        .bytes()
        .any(|b| b == b'h' || b == b'H')
}

fn is_numeric_format(fmt: &str) -> bool {
    let mut saw_digit_token = false;
    for ch in fmt.chars() {
        match ch {
            '0' | '#' => saw_digit_token = true,
            ',' | '.' => {}
            _ => return false,
        }
    }
    saw_digit_token
}

fn format_number(v: &Value, fmt: &str) -> Result<String> {
    let n = coerce_number(v)?;
    if !n.is_finite() {
        bail!("TEXT cannot format non-finite number");
    }

    let decimals = fmt
        .rfind('.')
        .map(|dot| {
            fmt[dot + 1..]
                .chars()
                .filter(|ch| *ch == '0' || *ch == '#')
                .count()
        })
        .unwrap_or(0);
    let rounded = round_half_away_from_zero(n, decimals as i32);
    let sign = if n < 0.0 { "-" } else { "" };
    let abs = rounded.abs();
    let mut s = if decimals == 0 {
        format!("{}", abs.floor() as i64)
    } else {
        format!("{abs:.decimals$}")
    };
    if fmt.contains(',') {
        if let Some(dot) = s.find('.') {
            let grouped = add_thousands(&s[..dot]);
            s = format!("{}{}", grouped, &s[dot..]);
        } else {
            s = add_thousands(&s);
        }
    }
    Ok(format!("{sign}{s}"))
}

fn add_thousands(digits: &str) -> String {
    let mut out = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn round_half_away_from_zero(n: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    if n >= 0.0 {
        (n * factor + 0.5).floor() / factor
    } else {
        -(((-n) * factor + 0.5).floor() / factor)
    }
}

fn add_months(year: i32, month: u32, months: i32) -> Result<(i32, u32)> {
    let total = year as i64 * 12 + month as i64 - 1 + months as i64;
    let year = total.div_euclid(12);
    if year < i32::MIN as i64 || year > i32::MAX as i64 {
        bail!("month offset produced an out-of-range year");
    }
    Ok((year as i32, total.rem_euclid(12) as u32 + 1))
}

fn days_in_excel_month(year: i32, month: u32) -> u32 {
    if year == 1900 && month == 2 {
        29
    } else {
        days_in_gregorian_month(year, month)
    }
}

fn days_in_gregorian_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_gregorian_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn valid_gregorian_date(year: i32, month: u32, day: u32) -> bool {
    month != 0 && day != 0 && day <= days_in_gregorian_month(year, month)
}

fn is_gregorian_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year as i64 - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = month as i64;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (
        (year + i64::from(month <= 2)) as i32,
        month as u32,
        day as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_half_away() {
        assert_eq!(round_half_away_from_zero(2.5, 0), 3.0);
        assert_eq!(round_half_away_from_zero(-2.5, 0), -3.0);
        assert_eq!(round_half_away_from_zero(2.45, 1), 2.5);
        assert_eq!(round_half_away_from_zero(-2.45, 1), -2.5);
    }

    #[test]
    fn text_formats_dates_and_numbers() {
        assert_eq!(
            text(&[
                Value::Number(46_145.0),
                Value::String("YYYY-MM-DD".into())
            ])
            .unwrap(),
            Value::String("2026-05-03".into())
        );
        assert_eq!(
            text(&[Value::Number(1234.5), Value::String("#,##0".into())]).unwrap(),
            Value::String("1,235".into())
        );
        assert_eq!(
            text(&[Value::Number(-2.345), Value::String("0.00".into())]).unwrap(),
            Value::String("-2.35".into())
        );
    }

    #[test]
    fn date_components_use_excel_serials() {
        assert_eq!(
            year(&[Value::Number(46_053.0)]).unwrap(),
            Value::Number(2026.0)
        );
        assert_eq!(
            month(&[Value::Number(46_053.0)]).unwrap(),
            Value::Number(1.0)
        );
        assert_eq!(
            day(&[Value::Number(46_053.0)]).unwrap(),
            Value::Number(31.0)
        );
        assert_eq!(
            text(&[Value::Number(1.0), Value::String("YYYY-MM-DD".into())]).unwrap(),
            Value::String("1900-01-01".into())
        );
        assert_eq!(
            text(&[Value::Number(60.0), Value::String("YYYY-MM-DD".into())]).unwrap(),
            Value::String("1900-02-29".into())
        );
    }

    #[test]
    fn eomonth_and_edate_return_serials() {
        let eomonth_value = eomonth(&[Value::Number(46_053.0), Value::Number(0.0)]).unwrap();
        assert_eq!(
            text(&[eomonth_value, Value::String("YYYY-MM-DD".into())]).unwrap(),
            Value::String("2026-01-31".into())
        );

        let edate_value = edate(&[Value::Number(46_053.0), Value::Number(1.0)]).unwrap();
        assert_eq!(
            text(&[edate_value, Value::String("YYYY-MM-DD".into())]).unwrap(),
            Value::String("2026-02-28".into())
        );
    }
}
