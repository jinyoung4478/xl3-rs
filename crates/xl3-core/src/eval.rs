//! Expression parser + evaluator for XTL templates.
//!
//! Scope at this milestone:
//! - `[Column]` field references
//! - String / number / boolean literals
//! - Comparison operators (`> < >= <= = == !=`)
//! - Arithmetic operators (`+ - * /`) and string concat (`&`)
//! - Function calls — `IF`, `ROUND`, plus a small growing set
//! - Mixed-text cells with one or more `{{ expr }}` substitutions
//!
//! Not yet:
//! - `Source[Column]` cross-source references (single-source only for now)
//! - directives (`@filter`, `@sort`, `@repeat`, `@source`, ...)
//! - aggregate functions over row sets (`sumRows`, `xlookupRows`, ...)
//! - `__config__[key]` / `__inputs__[key]` lookups inside expressions
//!
//! Those follow the conformance corpus order — they grow as more
//! fixtures are wired in.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use crate::value::{RowsHandle, Value};

const ROWS_KEY: &str = "__rows__";

pub type EvalContext = HashMap<String, Value>;

// ---------------------------------------------------------------------------
//                                Public API
// ---------------------------------------------------------------------------

pub fn eval_cell(template: &str, ctx: &EvalContext) -> Result<Value> {
    let trimmed = template.trim();

    if let Some(expr) = single_expression(trimmed) {
        return eval_expression_str(expr, ctx);
    }

    if !template.contains("{{") {
        return Ok(Value::String(template.to_string()));
    }

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
                let expr = after_open[..close].trim();
                let value = eval_expression_str(expr, ctx)?;
                out.push_str(&value.canonical());
                rest = &after_open[close + 2..];
            }
        }
    }
    Ok(Value::String(out))
}

pub fn eval_expression_str(expr: &str, ctx: &EvalContext) -> Result<Value> {
    let tokens = tokenize(expr)?;
    let mut parser = Parser::new(&tokens);
    let ast = parser.parse_expression(0)?;
    parser.expect_eof()?;
    eval_ast(&ast, ctx)
}

fn single_expression(s: &str) -> Option<&str> {
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    Some(inner.trim())
}

// ---------------------------------------------------------------------------
//                                  Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Number(f64),
    Str(String),
    Ident(String),
    Bool(bool),
    LBracket,
    RBracket,
    LParen,
    RParen,
    Comma,
    Op(Op),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Neq,
    Add,
    Sub,
    Mul,
    Div,
    Concat,
    And,
    Or,
    Not,
}

fn tokenize(input: &str) -> Result<Vec<Tok>> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            b']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'"' => {
                // Read until next ". xl3 does not currently support
                // escaped quotes inside string literals (the corpus uses
                // straight double quotes); revisit when a fixture hits.
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += 1;
                }
                if j >= bytes.len() {
                    bail!("unterminated string literal in {input:?}");
                }
                let s = std::str::from_utf8(&bytes[i + 1..j])
                    .map_err(|e| anyhow!("string literal not valid utf-8: {e}"))?;
                out.push(Tok::Str(s.to_string()));
                i = j + 1;
            }
            b'+' => {
                out.push(Tok::Op(Op::Add));
                i += 1;
            }
            b'-' => {
                out.push(Tok::Op(Op::Sub));
                i += 1;
            }
            b'*' => {
                out.push(Tok::Op(Op::Mul));
                i += 1;
            }
            b'/' => {
                out.push(Tok::Op(Op::Div));
                i += 1;
            }
            b'&' => {
                if peek_eq(bytes, i + 1, b'&') {
                    out.push(Tok::Op(Op::And));
                    i += 2;
                } else {
                    out.push(Tok::Op(Op::Concat));
                    i += 1;
                }
            }
            b'|' => {
                if peek_eq(bytes, i + 1, b'|') {
                    out.push(Tok::Op(Op::Or));
                    i += 2;
                } else {
                    bail!("unexpected '|' (single pipe not supported); use '||' for OR");
                }
            }
            b'<' => {
                if peek_eq(bytes, i + 1, b'=') {
                    out.push(Tok::Op(Op::Le));
                    i += 2;
                } else if peek_eq(bytes, i + 1, b'>') {
                    out.push(Tok::Op(Op::Neq));
                    i += 2;
                } else {
                    out.push(Tok::Op(Op::Lt));
                    i += 1;
                }
            }
            b'>' => {
                if peek_eq(bytes, i + 1, b'=') {
                    out.push(Tok::Op(Op::Ge));
                    i += 2;
                } else {
                    out.push(Tok::Op(Op::Gt));
                    i += 1;
                }
            }
            b'=' => {
                if peek_eq(bytes, i + 1, b'=') {
                    out.push(Tok::Op(Op::Eq));
                    i += 2;
                } else {
                    out.push(Tok::Op(Op::Eq));
                    i += 1;
                }
            }
            b'!' => {
                if peek_eq(bytes, i + 1, b'=') {
                    out.push(Tok::Op(Op::Neq));
                    i += 2;
                } else {
                    out.push(Tok::Op(Op::Not));
                    i += 1;
                }
            }
            b if b.is_ascii_digit() || (b == b'.' && peek_is_digit(bytes, i + 1)) => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                    i += 1;
                    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                        i += 1;
                    }
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let s = std::str::from_utf8(&bytes[start..i])
                    .map_err(|e| anyhow!("number literal not utf-8: {e}"))?;
                let n: f64 = s
                    .parse()
                    .map_err(|e| anyhow!("invalid number literal {s:?}: {e}"))?;
                out.push(Tok::Number(n));
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                {
                    i += 1;
                }
                let s = std::str::from_utf8(&bytes[start..i])
                    .map_err(|e| anyhow!("ident not utf-8: {e}"))?;
                let tok = match s {
                    "TRUE" | "true" | "True" => Tok::Bool(true),
                    "FALSE" | "false" | "False" => Tok::Bool(false),
                    _ => Tok::Ident(s.to_string()),
                };
                out.push(tok);
            }
            _ => bail!("unexpected character {:?} in {input:?}", b as char),
        }
    }
    Ok(out)
}

fn peek_eq(bytes: &[u8], idx: usize, target: u8) -> bool {
    bytes.get(idx).copied() == Some(target)
}

fn peek_is_digit(bytes: &[u8], idx: usize) -> bool {
    bytes.get(idx).map(|b| b.is_ascii_digit()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
//                                  Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Ast {
    Number(f64),
    Str(String),
    Bool(bool),
    Bracket(String),
    Call(String, Vec<Ast>),
    BinOp(Op, Box<Ast>, Box<Ast>),
    UnaryNot(Box<Ast>),
    UnaryNeg(Box<Ast>),
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(toks: &'a [Tok]) -> Self {
        Parser { toks, pos: 0 }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Tok> {
        let t = self.toks.get(self.pos);
        self.pos += 1;
        t
    }

    fn expect_eof(&self) -> Result<()> {
        if self.pos != self.toks.len() {
            bail!("unexpected trailing tokens starting at {:?}", self.toks.get(self.pos));
        }
        Ok(())
    }

    /// Pratt-style precedence climbing. `min_prec` is the minimum operator
    /// precedence the caller is willing to accept on its right side.
    fn parse_expression(&mut self, min_prec: u8) -> Result<Ast> {
        let mut left = self.parse_prefix()?;
        while let Some(tok) = self.peek().cloned() {
            let op = match tok {
                Tok::Op(o) => o,
                _ => break,
            };
            let prec = match op_precedence(op) {
                Some(p) if p >= min_prec => p,
                _ => break,
            };
            self.bump();
            let right = self.parse_expression(prec + 1)?;
            left = Ast::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_prefix(&mut self) -> Result<Ast> {
        let tok = self
            .bump()
            .cloned()
            .ok_or_else(|| anyhow!("expression ended unexpectedly"))?;
        match tok {
            Tok::Number(n) => Ok(Ast::Number(n)),
            Tok::Str(s) => Ok(Ast::Str(s)),
            Tok::Bool(b) => Ok(Ast::Bool(b)),
            Tok::LBracket => {
                let name = self.read_field_name_until_rbracket()?;
                Ok(Ast::Bracket(name))
            }
            Tok::LParen => {
                let e = self.parse_expression(0)?;
                let close = self
                    .bump()
                    .cloned()
                    .ok_or_else(|| anyhow!("expected ')' after parenthesized expression"))?;
                if close != Tok::RParen {
                    bail!("expected ')', got {close:?}");
                }
                Ok(e)
            }
            Tok::Op(Op::Sub) => {
                let rhs = self.parse_expression(7)?;
                Ok(Ast::UnaryNeg(Box::new(rhs)))
            }
            Tok::Op(Op::Not) => {
                let rhs = self.parse_expression(7)?;
                Ok(Ast::UnaryNot(Box::new(rhs)))
            }
            Tok::Ident(name) => {
                if let Some(Tok::LParen) = self.peek() {
                    self.bump();
                    let mut args = Vec::new();
                    if let Some(Tok::RParen) = self.peek() {
                        self.bump();
                        return Ok(Ast::Call(name, args));
                    }
                    loop {
                        args.push(self.parse_expression(0)?);
                        match self.bump().cloned() {
                            Some(Tok::Comma) => continue,
                            Some(Tok::RParen) => break,
                            other => bail!(
                                "expected ',' or ')' in argument list, got {:?}",
                                other
                            ),
                        }
                    }
                    Ok(Ast::Call(name, args))
                } else {
                    // Bare identifier — treat as the field name lookup
                    // when present, otherwise reject. xl3 also rejects
                    // unknown bare identifiers (ADR-0054).
                    Ok(Ast::Bracket(name))
                }
            }
            other => bail!("unexpected token at start of expression: {other:?}"),
        }
    }

    fn read_field_name_until_rbracket(&mut self) -> Result<String> {
        // A `[...]` field name is a single identifier (or arbitrary
        // text up to `]`). Allow ident + whitespace tokens by reading
        // the underlying source — but since the lexer already split
        // it, support the common case: a single ident, optionally with
        // a Source[Column] form's column name being plain text.
        let mut buf = String::new();
        loop {
            let tok = self
                .bump()
                .cloned()
                .ok_or_else(|| anyhow!("unterminated [ in expression"))?;
            match tok {
                Tok::RBracket => return Ok(buf.trim().to_string()),
                Tok::Ident(s) => {
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    buf.push_str(&s);
                }
                Tok::Number(n) => {
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    buf.push_str(&n.to_string());
                }
                other => bail!("unexpected {:?} inside [...]", other),
            }
        }
    }
}

fn op_precedence(op: Op) -> Option<u8> {
    Some(match op {
        Op::Or => 1,
        Op::And => 2,
        Op::Eq | Op::Neq => 3,
        Op::Lt | Op::Gt | Op::Le | Op::Ge => 4,
        Op::Concat => 5,
        Op::Add | Op::Sub => 6,
        Op::Mul | Op::Div => 7,
        Op::Not => return None, // unary only
    })
}

// ---------------------------------------------------------------------------
//                                Evaluator
// ---------------------------------------------------------------------------

fn eval_ast(ast: &Ast, ctx: &EvalContext) -> Result<Value> {
    match ast {
        Ast::Number(n) => Ok(Value::Number(*n)),
        Ast::Str(s) => Ok(Value::String(s.clone())),
        Ast::Bool(b) => Ok(Value::Bool(*b)),
        Ast::Bracket(name) => Ok(ctx.get(name).cloned().unwrap_or(Value::Empty)),
        Ast::UnaryNeg(inner) => {
            let v = eval_ast(inner, ctx)?;
            Ok(Value::Number(-coerce_number(&v)?))
        }
        Ast::UnaryNot(inner) => {
            let v = eval_ast(inner, ctx)?;
            Ok(Value::Bool(!is_truthy(&v)))
        }
        Ast::Call(name, args) => {
            // Row-aggregate dispatch (xl3 ADR-0027 / 0044): SUM/AVG/MIN/MAX
            // applied to a column ref means "aggregate over the active
            // block's source rows", and COUNT() with no arg means row
            // count. Anything else falls through to the scalar builtins.
            let upper = name.to_ascii_uppercase();
            if let Some(result) = try_row_aggregate(&upper, args, ctx)? {
                return Ok(result);
            }
            let mut values = Vec::with_capacity(args.len());
            for a in args {
                values.push(eval_ast(a, ctx)?);
            }
            call_builtin(name, &values)
        }
        Ast::BinOp(op, l, r) => {
            let lv = eval_ast(l, ctx)?;
            let rv = eval_ast(r, ctx)?;
            eval_binop(*op, &lv, &rv)
        }
    }
}

fn try_row_aggregate(name: &str, args: &[Ast], ctx: &EvalContext) -> Result<Option<Value>> {
    let rows = ctx_rows(ctx);
    // COUNT() with no args returns the row count, if a block context exists.
    if name == "COUNT" && args.is_empty() {
        return Ok(rows.map(|r| Value::Number(r.len() as f64)));
    }
    // SUM/AVERAGE/AVG/MIN/MAX with a single bracket arg → row aggregate.
    if args.len() == 1 {
        if let Ast::Bracket(field) = &args[0] {
            if let Some(rows) = rows {
                return Ok(Some(aggregate_over_field(name, rows, field)?));
            }
        }
    }
    Ok(None)
}

fn ctx_rows<'a>(ctx: &'a EvalContext) -> Option<&'a RowsHandle> {
    match ctx.get(ROWS_KEY) {
        Some(Value::Rows(h)) => Some(h),
        _ => None,
    }
}

fn aggregate_over_field(
    name: &str,
    rows: &RowsHandle,
    field: &str,
) -> Result<Value> {
    match name {
        "SUM" => {
            let mut acc = 0f64;
            for r in rows.iter() {
                if let Some(v) = r.get(field) {
                    if let Ok(n) = coerce_number(v) {
                        acc += n;
                    }
                }
            }
            Ok(Value::Number(acc))
        }
        "AVERAGE" | "AVG" => {
            let mut acc = 0f64;
            let mut n = 0usize;
            for r in rows.iter() {
                if let Some(v) = r.get(field) {
                    if !matches!(v, Value::Empty) {
                        if let Ok(num) = coerce_number(v) {
                            acc += num;
                            n += 1;
                        }
                    }
                }
            }
            Ok(if n == 0 {
                Value::Empty
            } else {
                Value::Number(acc / n as f64)
            })
        }
        "MIN" => {
            let mut best = f64::INFINITY;
            let mut seen = false;
            for r in rows.iter() {
                if let Some(v) = r.get(field) {
                    if let Ok(n) = coerce_number(v) {
                        if n < best {
                            best = n;
                        }
                        seen = true;
                    }
                }
            }
            Ok(if seen { Value::Number(best) } else { Value::Empty })
        }
        "MAX" => {
            let mut best = f64::NEG_INFINITY;
            let mut seen = false;
            for r in rows.iter() {
                if let Some(v) = r.get(field) {
                    if let Ok(n) = coerce_number(v) {
                        if n > best {
                            best = n;
                        }
                        seen = true;
                    }
                }
            }
            Ok(if seen { Value::Number(best) } else { Value::Empty })
        }
        "COUNT" => {
            let mut n = 0usize;
            for r in rows.iter() {
                if let Some(v) = r.get(field) {
                    if !matches!(v, Value::Empty) {
                        n += 1;
                    }
                }
            }
            Ok(Value::Number(n as f64))
        }
        _ => bail!("not a row aggregate: {name}"),
    }
}

/// Public helper used by `render` to inject the active block's rows
/// into an evaluation context.
pub fn inject_rows(ctx: &mut EvalContext, rows: RowsHandle) {
    ctx.insert(ROWS_KEY.to_string(), Value::Rows(rows));
}

fn eval_binop(op: Op, l: &Value, r: &Value) -> Result<Value> {
    Ok(match op {
        Op::Add => Value::Number(coerce_number(l)? + coerce_number(r)?),
        Op::Sub => Value::Number(coerce_number(l)? - coerce_number(r)?),
        Op::Mul => Value::Number(coerce_number(l)? * coerce_number(r)?),
        Op::Div => {
            let rn = coerce_number(r)?;
            if rn == 0.0 {
                bail!("xl3/eval/div-by-zero: division by zero");
            }
            Value::Number(coerce_number(l)? / rn)
        }
        Op::Concat => Value::String(format!("{}{}", l.canonical(), r.canonical())),
        Op::Lt => Value::Bool(compare(l, r)? < 0),
        Op::Gt => Value::Bool(compare(l, r)? > 0),
        Op::Le => Value::Bool(compare(l, r)? <= 0),
        Op::Ge => Value::Bool(compare(l, r)? >= 0),
        Op::Eq => Value::Bool(compare(l, r)? == 0),
        Op::Neq => Value::Bool(compare(l, r)? != 0),
        Op::And => Value::Bool(is_truthy(l) && is_truthy(r)),
        Op::Or => Value::Bool(is_truthy(l) || is_truthy(r)),
        Op::Not => unreachable!("unary not handled in parse_prefix"),
    })
}

fn coerce_number(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Empty => Ok(0.0),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| anyhow!("cannot coerce string {s:?} to number")),
        Value::Rows(_) => bail!("cannot coerce a Rows handle to a number"),
    }
}

pub fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Empty => false,
        Value::Number(n) => *n != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Rows(h) => !h.is_empty(),
    }
}

/// Three-way comparison: -1 / 0 / 1. Numeric on both sides when both
/// are numbers (or coerce-able). Otherwise lexicographic on canonical
/// strings. (Matches the xl3 0.x default — see ADR-0009 / functions.ts.)
pub fn compare(l: &Value, r: &Value) -> Result<i32> {
    if matches!(l, Value::Number(_) | Value::Bool(_) | Value::Empty)
        && matches!(r, Value::Number(_) | Value::Bool(_) | Value::Empty)
    {
        let ln = coerce_number(l)?;
        let rn = coerce_number(r)?;
        return Ok(if ln < rn {
            -1
        } else if ln > rn {
            1
        } else {
            0
        });
    }
    // Try numeric on strings if both parse — covers `[Amount] > 50` when
    // the source stored Amount as text. Falls back to string compare.
    if let (Ok(ln), Ok(rn)) = (coerce_number(l), coerce_number(r)) {
        return Ok(if ln < rn {
            -1
        } else if ln > rn {
            1
        } else {
            0
        });
    }
    let ls = l.canonical();
    let rs = r.canonical();
    Ok(ls.as_str().cmp(rs.as_str()) as i32)
}

// ---------------------------------------------------------------------------
//                              Built-in functions
// ---------------------------------------------------------------------------

fn is_empty_for_ifempty(v: &Value) -> bool {
    match v {
        Value::Empty => true,
        // xl3 (ADR-0025/0028): treat ASCII whitespace-only strings as
        // empty for IFEMPTY. Numbers and booleans (including 0 / false)
        // are explicitly NOT empty.
        Value::String(s) => s.chars().all(|c| c.is_ascii_whitespace()),
        Value::Number(_) | Value::Bool(_) | Value::Rows(_) => false,
    }
}

fn call_builtin(name: &str, args: &[Value]) -> Result<Value> {
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

fn round_half_away_from_zero(n: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    if n >= 0.0 {
        (n * factor + 0.5).floor() / factor
    } else {
        -(((-n) * factor + 0.5).floor() / factor)
    }
}

// ---------------------------------------------------------------------------
//                                  Tests
// ---------------------------------------------------------------------------

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

    #[test]
    fn if_with_comparison() {
        let ctx = ctx_of(&[("Amount", Value::Number(75.0))]);
        let out =
            eval_cell("{{ IF([Amount] > 50, \"big\", \"small\") }}", &ctx).unwrap();
        assert_eq!(out, Value::String("big".into()));
    }

    #[test]
    fn round_half_away() {
        assert_eq!(round_half_away_from_zero(2.5, 0), 3.0);
        assert_eq!(round_half_away_from_zero(-2.5, 0), -3.0);
        assert_eq!(round_half_away_from_zero(2.45, 1), 2.5);
        assert_eq!(round_half_away_from_zero(-2.45, 1), -2.5);
    }

    #[test]
    fn arithmetic_precedence() {
        let ctx = ctx_of(&[]);
        let out = eval_cell("{{ 1 + 2 * 3 }}", &ctx).unwrap();
        assert_eq!(out, Value::Number(7.0));
    }
}
