//! Expression evaluator.
//!
//! Phase 1 P1-A scope — just enough to render bracket substitution:
//! - `[Column]` → ctx lookup
//! - bare `"string literal"` → the string
//! - bare numeric literal → the number
//! - everything else → an `Err` (so we get a real diagnostic instead of
//!   silently emitting the raw expression text)
//!
//! XTL function calls, directives, `__config__[key]`, joins, aggregates,
//! etc. are deliberately not here yet — they grow with later fixtures.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use crate::value::Value;

pub type EvalContext = HashMap<String, Value>;

/// Evaluate a full cell template like `"Hello {{ [Name] }}"` or
/// `"{{ [Customer] }}"`. Returns:
/// - a single `Value` when the cell is one `{{ expr }}` and nothing else
///   (so a number column stays numeric)
/// - a `Value::String` of the joined canonical form for mixed-text cells
/// - a `Value::String` echoing the raw text when no `{{` is present
pub fn eval_cell(template: &str, ctx: &EvalContext) -> Result<Value> {
    let trimmed = template.trim();

    // Sole-expression form: the whole cell is `{{ expr }}` and nothing
    // else. Preserve the underlying value's type.
    if let Some(expr) = single_expression(trimmed) {
        return eval_expression(expr, ctx);
    }

    // No expression blocks at all — return the raw text. The planner
    // hands literal cells through `CellSource::Literal` instead, so we
    // shouldn't normally land here, but it keeps the function total.
    if !template.contains("{{") {
        return Ok(Value::String(template.to_string()));
    }

    // Mixed text + one or more `{{ expr }}` substitutions. Match TS:
    // each substitution is rendered via canonical string form and
    // concatenated.
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    loop {
        match rest.find("{{") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(open) => {
                out.push_str(&rest[..open]);
                let after_open = &rest[open + 2..];
                let close = after_open
                    .find("}}")
                    .ok_or_else(|| anyhow!("unterminated {{{{ in template {template:?}"))?;
                let expr = &after_open[..close];
                let value = eval_expression(expr.trim(), ctx)?;
                out.push_str(&value.canonical());
                rest = &after_open[close + 2..];
            }
        }
    }
    Ok(Value::String(out))
}

fn single_expression(s: &str) -> Option<&str> {
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    Some(inner.trim())
}

pub fn eval_expression(expr: &str, ctx: &EvalContext) -> Result<Value> {
    let trimmed = expr.trim();

    // [Column] → ctx lookup.
    if let Some(inside) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let key = inside.trim();
        return Ok(ctx.get(key).cloned().unwrap_or(Value::Empty));
    }

    // "string literal"
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return Ok(Value::String(trimmed[1..trimmed.len() - 1].to_string()));
    }

    // Bare number literal.
    if let Ok(n) = trimmed.parse::<f64>() {
        return Ok(Value::Number(n));
    }

    // TRUE / FALSE / true / false / True / False (per xl3 TS).
    match trimmed {
        "TRUE" | "true" | "True" => return Ok(Value::Bool(true)),
        "FALSE" | "false" | "False" => return Ok(Value::Bool(false)),
        _ => {}
    }

    bail!(
        "unsupported expression {expr:?} (Phase 1 P1-A only handles [Column], \"literal\", and numeric/boolean literals)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_of(pairs: &[(&str, Value)]) -> EvalContext {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn bracket_substitution() {
        let ctx = ctx_of(&[("Customer", Value::String("Acme".into()))]);
        let out = eval_cell("{{ [Customer] }}", &ctx).unwrap();
        assert_eq!(out, Value::String("Acme".into()));
    }

    #[test]
    fn mixed_text() {
        let ctx = ctx_of(&[("Name", Value::String("Acme".into()))]);
        let out = eval_cell("Hello {{ [Name] }}!", &ctx).unwrap();
        assert_eq!(out, Value::String("Hello Acme!".into()));
    }

    #[test]
    fn number_passthrough() {
        let ctx = ctx_of(&[("Qty", Value::Number(42.0))]);
        let out = eval_cell("{{ [Qty] }}", &ctx).unwrap();
        assert_eq!(out, Value::Number(42.0));
    }

    #[test]
    fn literal_only() {
        let ctx = ctx_of(&[]);
        let out = eval_cell("Customer", &ctx).unwrap();
        assert_eq!(out, Value::String("Customer".into()));
    }
}
