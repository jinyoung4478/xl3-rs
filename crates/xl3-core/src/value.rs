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

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Empty,
    String(String),
    Number(f64),
    Bool(bool),
    /// All source rows visible to the active expansion block. Used for
    /// row aggregates. Not emitted as a cell value.
    Rows(RowsHandle),
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
            Value::Number(n) => {
                if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e16 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            // `Rows` is internal scaffolding — it should not normally
            // surface to a cell. If it does, render as empty rather
            // than panicking; the caller will see the empty result and
            // can correct the template.
            Value::Rows(_) => String::new(),
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
