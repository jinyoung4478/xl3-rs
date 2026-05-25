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
const ROWNUM_KEY: &str = "__rownum__";

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
    In,
    NotIn,
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
                } else if starts_with_word(bytes, i + 1, b"in") {
                    // `!in` (xl3 set-membership negation). Treated as a
                    // single binary operator regardless of whitespace
                    // between `!` and `in`.
                    out.push(Tok::Op(Op::NotIn));
                    i += 1 + 2;
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
            b if b.is_ascii_alphabetic() || b == b'_' || b >= 0x80 => {
                // Allow ASCII identifiers AND UTF-8 multi-byte sequences
                // (Korean / CJK / Cyrillic column names). Multi-byte
                // bytes are always >= 0x80 so a simple byte-level check
                // is sufficient.
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'_'
                        || bytes[i] >= 0x80)
                {
                    i += 1;
                }
                let s = std::str::from_utf8(&bytes[start..i])
                    .map_err(|e| anyhow!("ident not utf-8: {e}"))?;
                let tok = match s {
                    "TRUE" | "true" | "True" => Tok::Bool(true),
                    "FALSE" | "false" | "False" => Tok::Bool(false),
                    // `in` is a binary operator (xl3 @filter set
                    // membership). The lexer surfaces it so the parser
                    // can place it at comparison precedence.
                    "in" => Tok::Op(Op::In),
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

/// Returns true if the byte slice from `start` matches `word` exactly
/// and the next byte (if any) is *not* an identifier character. Used
/// by the lexer to recognise `!in` only when "in" stands alone.
fn starts_with_word(bytes: &[u8], start: usize, word: &[u8]) -> bool {
    // skip optional whitespace between `!` and `in`
    let mut i = start;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i + word.len() > bytes.len() {
        return false;
    }
    if &bytes[i..i + word.len()] != word {
        return false;
    }
    let after = bytes.get(i + word.len());
    match after {
        Some(c) if c.is_ascii_alphanumeric() || *c == b'_' => false,
        _ => true,
    }
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
    /// `<namespace>[key]` — e.g. `__inputs__[month]`, `__lists__[Allowed]`,
    /// `__config__[name]`. Namespace is the ident immediately before the
    /// `[`; key is the trimmed text inside.
    ReservedRef(String, String),
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
            bail!(
                "unexpected trailing tokens starting at {:?}",
                self.toks.get(self.pos)
            );
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
                            other => bail!("expected ',' or ')' in argument list, got {:?}", other),
                        }
                    }
                    Ok(Ast::Call(name, args))
                } else if let Some(Tok::LBracket) = self.peek() {
                    // `<ident>[key]` reserved-ref form.
                    self.bump();
                    let key = self.read_field_name_until_rbracket()?;
                    Ok(Ast::ReservedRef(name, key))
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
        Op::Lt | Op::Gt | Op::Le | Op::Ge | Op::In | Op::NotIn => 4,
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
        Ast::ReservedRef(ns, key) => {
            // Look up the namespace in ctx — `__inputs__`, `__lists__`,
            // `__config__`. Each is a Value::Map injected by the
            // renderer. Inner values may themselves be `Value::List`
            // (for `__lists__[Name]` which resolves to the named list).
            // Missing namespace => Empty, mirroring xl3's permissive
            // read of an unset key.
            match ctx.get(ns) {
                Some(Value::Map(m)) => Ok(m.get(key).cloned().unwrap_or(Value::Empty)),
                _ => Ok(Value::Empty),
            }
        }
        Ast::UnaryNeg(inner) => {
            let v = eval_ast(inner, ctx)?;
            Ok(Value::Number(-coerce_number(&v)?))
        }
        Ast::UnaryNot(inner) => {
            let v = eval_ast(inner, ctx)?;
            Ok(Value::Bool(!is_truthy(&v)))
        }
        Ast::Call(name, args) => {
            let upper = name.to_ascii_uppercase();
            // ROW() — 1-based source row index inside the active
            // expansion block. Resolved from ctx so the render layer
            // owns the actual numbering.
            if upper == "ROW" && args.is_empty() {
                return Ok(ctx.get(ROWNUM_KEY).cloned().unwrap_or(Value::Empty));
            }
            // XLOOKUP needs the AST of its source-bracketed args, so it
            // routes through its own dispatch ahead of scalar builtins.
            // xl3 ADR-0013.
            if upper == "XLOOKUP" {
                return try_xlookup(args, ctx);
            }
            // Row-aggregate dispatch (xl3 ADR-0027 / 0044): SUM/AVG/MIN/MAX
            // applied to a column ref means "aggregate over the active
            // block's source rows", and COUNT() with no arg means row
            // count. Anything else falls through to the scalar builtins.
            if let Some(result) = try_row_aggregate(&upper, args, ctx)? {
                return Ok(result);
            }
            let mut values = Vec::with_capacity(args.len());
            for a in args {
                values.push(eval_ast(a, ctx)?);
            }
            crate::functions::call_scalar(name, &values)
        }
        Ast::BinOp(op, l, r) => {
            let lv = eval_ast(l, ctx)?;
            let rv = eval_ast(r, ctx)?;
            eval_binop(*op, &lv, &rv)
        }
    }
}

fn try_row_aggregate(name: &str, args: &[Ast], ctx: &EvalContext) -> Result<Option<Value>> {
    if !is_row_aggregate_name(name) {
        return Ok(None);
    }
    let rows = ctx_rows(ctx);
    // COUNT() with no args returns the row count, if a block context exists.
    if name == "COUNT" && args.is_empty() {
        return Ok(rows.map(|r| Value::Number(r.len() as f64)));
    }
    if args.len() == 1 {
        match &args[0] {
            // `SUM([Field])` etc — aggregate over the active block.
            Ast::Bracket(field) => {
                if let Some(rows) = rows {
                    return Ok(Some(aggregate_over_field(name, rows, field)?));
                }
            }
            // `SUM(Source[Field])` etc — aggregate over the named
            // source declared on `__sources__`. xl3 ADR-0012: this
            // works regardless of whether we're inside an expansion
            // block.
            Ast::ReservedRef(source, field) => {
                if !is_reserved_namespace(source) {
                    if let Some(Value::Rows(handle)) = ctx.get(source) {
                        return Ok(Some(aggregate_over_field(name, handle, field)?));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

fn is_reserved_namespace(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__")
}

/// `XLOOKUP(needle, Source[lookupCol], Source[returnCol], [fallback])`.
/// Walks the named source's rows and returns the first matching row's
/// `returnCol`. Falls back to the 4th arg's value when nothing matches.
fn try_xlookup(args: &[Ast], ctx: &EvalContext) -> Result<Value> {
    if !(3..=4).contains(&args.len()) {
        bail!(
            "XLOOKUP expects 3 or 4 arguments, got {} (signature: XLOOKUP(value, Source[lookupCol], Source[returnCol], [fallback]))",
            args.len()
        );
    }
    let needle = eval_ast(&args[0], ctx)?;
    let (lookup_src, lookup_field) = expect_source_bracket(&args[1], "XLOOKUP arg 2")?;
    let (return_src, return_field) = expect_source_bracket(&args[2], "XLOOKUP arg 3")?;
    if lookup_src != return_src {
        bail!(
            "XLOOKUP arg 2 source {lookup_src:?} and arg 3 source {return_src:?} must match"
        );
    }
    let rows = match ctx.get(lookup_src) {
        Some(Value::Rows(h)) => h,
        _ => bail!(
            "XLOOKUP source {lookup_src:?} is not declared in __sources__"
        ),
    };
    for row in rows.iter() {
        let cell = row.get(lookup_field).cloned().unwrap_or(Value::Empty);
        if values_equal(&cell, &needle) {
            return Ok(row.get(return_field).cloned().unwrap_or(Value::Empty));
        }
    }
    if args.len() == 4 {
        eval_ast(&args[3], ctx)
    } else {
        Ok(Value::Empty)
    }
}

fn expect_source_bracket<'a>(ast: &'a Ast, role: &str) -> Result<(&'a String, &'a String)> {
    match ast {
        Ast::ReservedRef(src, field) if !is_reserved_namespace(src) => Ok((src, field)),
        _ => bail!(
            "{role} must be a source-prefixed bracket reference like Source[Column]"
        ),
    }
}

fn is_row_aggregate_name(name: &str) -> bool {
    matches!(name, "SUM" | "AVERAGE" | "AVG" | "MIN" | "MAX" | "COUNT")
}

fn ctx_rows<'a>(ctx: &'a EvalContext) -> Option<&'a RowsHandle> {
    match ctx.get(ROWS_KEY) {
        Some(Value::Rows(h)) => Some(h),
        _ => None,
    }
}

fn aggregate_over_field(name: &str, rows: &RowsHandle, field: &str) -> Result<Value> {
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
            Ok(if seen {
                Value::Number(best)
            } else {
                Value::Empty
            })
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
            Ok(if seen {
                Value::Number(best)
            } else {
                Value::Empty
            })
        }
        "COUNT" => {
            let mut n = 0usize;
            for r in rows.iter() {
                if let Some(v) = r.get(field) {
                    if !crate::source::is_blank_value(v) {
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

/// Inject the 1-based row index for the current expansion iteration.
pub fn inject_rownum(ctx: &mut EvalContext, one_based: usize) {
    ctx.insert(ROWNUM_KEY.to_string(), Value::Number(one_based as f64));
}

fn eval_binop(op: Op, l: &Value, r: &Value) -> Result<Value> {
    Ok(match op {
        Op::Add => Value::Number(coerce_number(l)? + coerce_number(r)?),
        Op::Sub => Value::Number(coerce_number(l)? - coerce_number(r)?),
        Op::Mul => Value::Number(coerce_number(l)? * coerce_number(r)?),
        Op::Div => {
            let rn = coerce_number(r)?;
            if rn == 0.0 {
                // xl3 ADR-0025: division by zero emits a `#DIV/0!` error
                // cell in the output. Stage-1 conformance compares cell
                // values via calamine, which surfaces error cells as
                // Data::Error → Value::Empty in our compare path, so we
                // emit Empty here. A first-class Value::Error variant
                // (for stage-2 byte comparison) lands with manifest
                // preservation later.
                return Ok(Value::Empty);
            }
            Value::Number(coerce_number(l)? / rn)
        }
        Op::Concat => {
            // xl3 ADR-0009: blank operands (Empty, whitespace-only
            // strings) contribute the empty string in `&` concat, not
            // their literal whitespace. This keeps `"[" & [Memo] & "]"`
            // emitting `[]` for a missing memo even when the source
            // cell stores `"   "`.
            let coerce = |v: &Value| -> String {
                if crate::source::is_blank_value(v) {
                    String::new()
                } else {
                    v.canonical()
                }
            };
            Value::String(format!("{}{}", coerce(l), coerce(r)))
        }
        Op::Lt => Value::Bool(compare(l, r)? < 0),
        Op::Gt => Value::Bool(compare(l, r)? > 0),
        Op::Le => Value::Bool(compare(l, r)? <= 0),
        Op::Ge => Value::Bool(compare(l, r)? >= 0),
        Op::Eq => Value::Bool(compare(l, r)? == 0),
        Op::Neq => Value::Bool(compare(l, r)? != 0),
        Op::And => Value::Bool(is_truthy(l) && is_truthy(r)),
        Op::Or => Value::Bool(is_truthy(l) || is_truthy(r)),
        Op::In => Value::Bool(member_of(l, r)),
        Op::NotIn => Value::Bool(!member_of(l, r)),
        Op::Not => unreachable!("unary not handled in parse_prefix"),
    })
}

/// Set-membership test used by `in` / `!in`. RHS is expected to be a
/// `Value::List` (typically `__lists__[Name]`). If RHS is some other
/// shape, fall back to equality so the operator still behaves sanely
/// for a single-value RHS.
///
/// xl3 fixture 054: blank entries inside the list (Empty or
/// whitespace-only string) never match anything — a list with blanks
/// behaves the same as the same list with the blanks removed.
fn member_of(needle: &Value, haystack: &Value) -> bool {
    match haystack {
        Value::List(list) => list.iter().any(|item| {
            !crate::source::is_blank_value(item) && values_equal(item, needle)
        }),
        _ => values_equal(haystack, needle),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    compare(a, b).map(|c| c == 0).unwrap_or(false)
}

pub(crate) fn coerce_number(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) | Value::DateNumber(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Empty => Ok(0.0),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(0.0);
            }
            // Date strings (ADR-0017 canonical form) come in via source
            // numFmt-driven coercion; recover the serial so date math /
            // YEAR / MONTH / DAY etc. keep working unchanged.
            if let Some(serial) = crate::functions::iso_string_to_serial(trimmed) {
                return Ok(serial);
            }
            // xl3 ADR-0009 allows numeric strings with thousands
            // separators, e.g. "1,234.5" → 1234.5.
            let stripped: String = trimmed.chars().filter(|c| *c != ',').collect();
            stripped
                .parse::<f64>()
                .map_err(|_| anyhow!("cannot coerce string {s:?} to number"))
        }
        Value::Rows(_) | Value::Map(_) | Value::List(_) => {
            bail!("cannot coerce a composite Value to a number")
        }
    }
}

pub fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Empty => false,
        Value::Number(n) | Value::DateNumber(n) => *n != 0.0,
        // xl3 ADR-0007/0008: whitespace-only strings are blank → falsy.
        // Same rule as `is_blank_value` (source-row skip / COUNT /
        // ISBLANK).
        Value::String(s) => !s.chars().all(char::is_whitespace),
        Value::Rows(h) => !h.is_empty(),
        Value::Map(m) => !m.is_empty(),
        Value::List(l) => !l.is_empty(),
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
    // ADR-0017: an Excel-serial number compared against a YYYY-MM-DD
    // string is compared as date strings (the number's canonical form).
    // Without numFmt metadata we infer "looks like a date" from the
    // string's shape — narrow enough that non-date string compares are
    // unaffected.
    if let (Value::Number(n), Value::String(s)) = (l, r) {
        if looks_like_iso_date(s) {
            if let Some(ds) = crate::functions::serial_to_iso_date(*n) {
                return Ok(ds.as_str().cmp(s.as_str()) as i32);
            }
        }
    }
    if let (Value::String(s), Value::Number(n)) = (l, r) {
        if looks_like_iso_date(s) {
            if let Some(ds) = crate::functions::serial_to_iso_date(*n) {
                return Ok(s.as_str().cmp(ds.as_str()) as i32);
            }
        }
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

fn looks_like_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 10 {
        return false;
    }
    b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
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
        let out = eval_cell("{{ IF([Amount] > 50, \"big\", \"small\") }}", &ctx).unwrap();
        assert_eq!(out, Value::String("big".into()));
    }

    #[test]
    fn arithmetic_precedence() {
        let ctx = ctx_of(&[]);
        let out = eval_cell("{{ 1 + 2 * 3 }}", &ctx).unwrap();
        assert_eq!(out, Value::Number(7.0));
    }
}
