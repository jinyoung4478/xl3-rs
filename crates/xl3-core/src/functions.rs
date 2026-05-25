//! Context-free scalar function implementations.

use anyhow::{bail, Result};

use crate::eval::{coerce_number, is_truthy};
use crate::value::Value;

pub fn call_scalar(name: &str, args: &[Value]) -> Result<Value> {
    // xl3 fixture 116: function names are case-insensitive (IF == if).
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "IF" => {
            if args.len() != 3 {
                bail!("IF expects 3 arguments, got {}", args.len());
            }
            Ok(if is_truthy(&args[0]) {
                args[1].clone()
            } else {
                args[2].clone()
            })
        }
        "ROUND" => {
            if args.len() != 2 {
                bail!("ROUND expects 2 arguments, got {}", args.len());
            }
            let n = coerce_number(&args[0])?;
            let d = coerce_number(&args[1])? as i32;
            Ok(Value::Number(round_half_away_from_zero(n, d)))
        }
        "ABS" => {
            if args.len() != 1 {
                bail!("ABS expects 1 argument, got {}", args.len());
            }
            Ok(Value::Number(coerce_number(&args[0])?.abs()))
        }
        "UPPER" => {
            if args.len() != 1 {
                bail!("UPPER expects 1 argument, got {}", args.len());
            }
            Ok(Value::String(args[0].canonical().to_uppercase()))
        }
        "LOWER" => {
            if args.len() != 1 {
                bail!("LOWER expects 1 argument, got {}", args.len());
            }
            Ok(Value::String(args[0].canonical().to_lowercase()))
        }
        "TRIM" => {
            if args.len() != 1 {
                bail!("TRIM expects 1 argument, got {}", args.len());
            }
            Ok(Value::String(args[0].canonical().trim().to_string()))
        }
        "LEN" => {
            if args.len() != 1 {
                bail!("LEN expects 1 argument, got {}", args.len());
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
        "ISBLANK" => {
            if args.len() != 1 {
                bail!("ISBLANK expects 1 argument, got {}", args.len());
            }
            Ok(Value::Bool(matches!(&args[0], Value::Empty)))
        }
        "IFEMPTY" => {
            if args.len() != 2 {
                bail!("IFEMPTY expects 2 arguments, got {}", args.len());
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
                bail!("MAX expects at least one argument");
            }
            let mut best = f64::NEG_INFINITY;
            for a in args {
                best = best.max(coerce_number(a)?);
            }
            Ok(Value::Number(best))
        }
        "MIN" => {
            if args.is_empty() {
                bail!("MIN expects at least one argument");
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
                bail!("NOT expects 1 argument, got {}", args.len());
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
        other => bail!("unknown function {other}"),
    }
}

fn is_empty_for_ifempty(v: &Value) -> bool {
    match v {
        Value::Empty => true,
        // xl3 (ADR-0025/0028): treat ASCII whitespace-only strings as
        // empty for IFEMPTY. Numbers and booleans (including 0 / false)
        // are explicitly NOT empty.
        Value::String(s) => s.chars().all(|c| c.is_ascii_whitespace()),
        Value::Number(_) | Value::Bool(_) | Value::Rows(_) | Value::Map(_) | Value::List(_) => {
            false
        }
    }
}

fn round_half_away_from_zero(n: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    if n >= 0.0 {
        (n * factor + 0.5).floor() / factor
    } else {
        -(((-n) * factor + 0.5).floor() / factor)
    }
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
}
