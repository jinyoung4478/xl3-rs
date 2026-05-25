//! Cell value representation used by the planner, source reader, and
//! evaluator. Deliberately simple — strings, numbers, booleans, plus an
//! explicit Empty so we can distinguish "blank cell" from "empty string".
//!
//! Errors propagate up the eval stack as `Err`, not as a `Value::Error`
//! variant — that matches what the XTL spec calls "expression error" and
//! avoids the JS-side `__xl3_error__` marker object until later milestones.

use std::collections::HashMap;
use std::sync::Arc;

use crate::calamine::Data as CalamineData;

/// A bag of source rows reachable from the evaluator. Lives in
/// `EvalContext` under the reserved `__rows__` key so row-aggregate
/// builtins (`SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT`) can walk every
/// row in the active block without each row needing to know about it.
pub type RowsHandle = Arc<Vec<HashMap<String, Value>>>;
pub type MapHandle = Arc<HashMap<String, Value>>;
pub type ListHandle = Arc<Vec<Value>>;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Empty,
    String(String),
    Number(f64),
    Bool(bool),
    /// An Excel serial number whose source cell carried a date /
    /// datetime numFmt. Behaves like `Number` for arithmetic and
    /// numeric comparison; its canonical string form is the ISO
    /// `YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS` representation per
    /// ADR-0017. Output cells write the raw serial — the format is
    /// reapplied by the consuming spreadsheet.
    DateNumber(f64),
    /// All source rows visible to the active expansion block. Used for
    /// row aggregates. Not emitted as a cell value.
    Rows(RowsHandle),
    /// Reserved-sheet dictionary value (`__config__`, `__inputs__`,
    /// `__lists__`). Looked up via `<ns>[key]` in expressions.
    Map(MapHandle),
    /// A list — typically a column of `__lists__`. Used by the `in`
    /// and `!in` operators inside `@filter`.
    List(ListHandle),
}

/// xl3 ADR-0009 / ECMA-262 §6.1.6.1.13: a number's canonical string
/// form is decimal when its magnitude is in [1e-6, 1e21), and
/// exponential ("xeY") below 1e-6. Integers within ±1e16 round-trip
/// without a decimal point.
pub fn canonical_number(n: f64) -> String {
    if !n.is_finite() {
        return format!("{n}");
    }
    if n.fract() == 0.0 && n.abs() < 1e16 {
        return format!("{}", n as i64);
    }
    let abs = n.abs();
    if abs > 0.0 && abs < 1e-6 {
        format!("{n:e}")
    } else {
        format!("{n}")
    }
}

impl Value {
    /// Canonical string form per ADR-0009 (xl3 TS `canonicalString` mirror).
    /// Used when a value is substituted into a mixed text cell — e.g.
    /// `"Hello {{ [Name] }}"` — so cross-impl rendering of booleans /
    /// numbers / empty values is stable.
    pub fn canonical(&self) -> String {
        match self {
            Value::Empty => String::new(),
            Value::String(s) => s.clone(),
            Value::Number(n) => canonical_number(*n),
            Value::DateNumber(n) => crate::functions::serial_to_iso_canonical(*n)
                .unwrap_or_else(|| canonical_number(*n)),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            // Internal scaffolding values — defensive empty render.
            Value::Rows(_) | Value::Map(_) | Value::List(_) => String::new(),
        }
    }

    pub fn from_calamine(d: &CalamineData) -> Value {
        match d {
            CalamineData::Empty => Value::Empty,
            CalamineData::String(s) => Value::String(s.clone()),
            CalamineData::Float(f) => Value::Number(*f),
            CalamineData::Int(i) => Value::Number(*i as f64),
            CalamineData::Bool(b) => Value::Bool(*b),
            CalamineData::DateTime(dt) => Value::Number(dt.as_f64()),
            CalamineData::DateTimeIso(s) | CalamineData::DurationIso(s) => {
                Value::String(s.clone())
            }
            CalamineData::Error(_) => Value::Empty,
        }
    }
}
