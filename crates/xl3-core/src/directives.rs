//! Directive parsing.
//!
//! XTL directives live inside a `{{ @<name> ... }}` cell. A row whose
//! template cells are *all* directive-only is a **directive row** — it
//! doesn't appear in the output, and its directives bind to the next
//! data row (the expansion row).
//!
//! Phase 1 P1-C scope: this minimum recognises `@repeat down|right`
//! and stores other directives opaquely (parser admits them but the
//! renderer ignores them so we don't crash on richer fixtures). Real
//! `@filter` / `@sort` / `@top` semantics land in subsequent passes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Down,
    Right,
}

#[derive(Debug, Clone)]
pub enum Directive {
    Repeat(Direction),
    /// `@filter <expr>` — expression evaluated per source row; truthy
    /// rows are kept.
    Filter(String),
    /// `@sort [Field] [asc|desc]` — sort key + direction.
    Sort { field: String, ascending: bool },
    /// `@top N` — keep first N rows after sort/filter.
    Top(usize),
    /// `@source <Name>` — the following expansion block iterates over
    /// the named external source (declared on `__sources__`) instead
    /// of the default source.
    Source(String),
    /// `@join <Name> on <Name>[<field>] = <other>[<field>]` — inner
    /// join the current block's rows against `<Name>`. Phase-1 scope:
    /// equality on a single column pair. `match_field` is the column
    /// in the joined (named) source; `primary_field` is the column in
    /// the current block's row that pairs with it.
    Join {
        source: String,
        match_field: String,
        primary_field: String,
    },
    /// `@group [Field]` or `@group [F1], [F2], …` — partition the
    /// (already-sorted) rows by the listed fields, nested left-to-right.
    /// One subtotal row is emitted at each level on group boundary,
    /// from innermost to outermost (ADR-0038).
    Group(Vec<String>),
    /// Captured but not yet acted on. Lets the planner classify rows as
    /// "directive only" without exploding when richer fixtures hit.
    Unhandled(String),
}

/// Returns `Some(directives)` when the cell text is one or more
/// `{{ @<name> ... }}` blocks and nothing else. Whitespace between
/// blocks is OK. Returns `None` for anything that mixes directives with
/// literal text or with a data-bearing `{{ }}` block.
pub fn parse_directive_cell(text: &str) -> Option<Vec<Directive>> {
    let trimmed = text.trim();
    if !trimmed.contains("{{") {
        return None;
    }
    let mut rest = trimmed;
    let mut out = Vec::new();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let after_open = rest.strip_prefix("{{")?;
        let close = after_open.find("}}")?;
        let inner = after_open[..close].trim();
        let d = parse_one(inner)?;
        out.push(d);
        rest = &after_open[close + 2..];
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_one(inner: &str) -> Option<Directive> {
    let s = inner.trim();
    let body = s.strip_prefix('@')?;
    let (name, rest) = match body.split_once(char::is_whitespace) {
        Some((n, r)) => (n, r.trim()),
        None => (body, ""),
    };
    let name = name.to_ascii_lowercase();
    Some(match name.as_str() {
        "repeat" => match rest {
            "" | "down" => Directive::Repeat(Direction::Down),
            "right" => Directive::Repeat(Direction::Right),
            _ => Directive::Unhandled(format!("@repeat {rest}")),
        },
        "filter" => {
            if rest.is_empty() {
                Directive::Unhandled("@filter (empty)".into())
            } else {
                Directive::Filter(rest.to_string())
            }
        }
        "sort" => parse_sort(rest),
        "top" => match rest.parse::<usize>() {
            Ok(n) => Directive::Top(n),
            Err(_) => Directive::Unhandled(format!("@top {rest}")),
        },
        "source" => {
            let name = rest.trim();
            if name.is_empty() {
                Directive::Unhandled("@source (empty)".into())
            } else {
                Directive::Source(name.to_string())
            }
        }
        "join" => parse_join(rest),
        "group" => {
            // `@group [F1], [F2], …` — comma-separated, each bracketed.
            // Bare-ident form `@group F1` also accepted for symmetry.
            let body = rest.trim();
            let fields: Vec<String> = body
                .split(',')
                .map(|p| {
                    let p = p.trim();
                    p.strip_prefix('[')
                        .and_then(|s| s.strip_suffix(']'))
                        .map(str::trim)
                        .unwrap_or(p)
                        .to_string()
                })
                .filter(|s| !s.is_empty())
                .collect();
            if fields.is_empty() {
                Directive::Unhandled("@group (empty)".into())
            } else {
                Directive::Group(fields)
            }
        }
        _ => Directive::Unhandled(format!("@{name} {rest}").trim().to_string()),
    })
}

fn parse_join(rest: &str) -> Directive {
    // Grammar (Phase 1 scope):
    //   <Name> on <Name>[<field>] = <Other>[<field>]
    // Either side of the `=` may be the join target — we identify it by
    // matching the `<Name>` that opens the directive.
    let parts: Vec<&str> = rest.splitn(2, " on ").map(str::trim).collect();
    if parts.len() != 2 {
        return Directive::Unhandled(format!("@join {rest}"));
    }
    let name = parts[0].to_string();
    let cond = parts[1];
    let Some((lhs, rhs)) = cond.split_once('=') else {
        return Directive::Unhandled(format!("@join {rest}"));
    };
    let (lhs_src, lhs_field) = match parse_source_bracket_text(lhs.trim()) {
        Some(p) => p,
        None => return Directive::Unhandled(format!("@join {rest}")),
    };
    let (rhs_src, rhs_field) = match parse_source_bracket_text(rhs.trim()) {
        Some(p) => p,
        None => return Directive::Unhandled(format!("@join {rest}")),
    };
    let (match_field, primary_field) = if lhs_src == name {
        (lhs_field, rhs_field)
    } else if rhs_src == name {
        (rhs_field, lhs_field)
    } else {
        return Directive::Unhandled(format!("@join {rest}"));
    };
    Directive::Join {
        source: name,
        match_field,
        primary_field,
    }
}

fn parse_source_bracket_text(s: &str) -> Option<(String, String)> {
    let (name, rest) = s.split_once('[')?;
    let field = rest.strip_suffix(']')?;
    let n = name.trim();
    let f = field.trim();
    if n.is_empty() || f.is_empty() {
        return None;
    }
    Some((n.to_string(), f.to_string()))
}

fn parse_sort(rest: &str) -> Directive {
    // `[Field]` then optional `asc` / `desc`. xl3 default = ascending.
    let rest = rest.trim();
    let (field_part, dir_part) = if let Some(close) = rest.find(']') {
        let after = rest[close + 1..].trim();
        (&rest[..=close], after)
    } else {
        // Bare identifier form: `@sort Field [asc|desc]`.
        match rest.split_once(char::is_whitespace) {
            Some((f, r)) => (f, r.trim()),
            None => (rest, ""),
        }
    };
    let field = field_part
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_string();
    if field.is_empty() {
        return Directive::Unhandled(format!("@sort {rest}"));
    }
    let ascending = match dir_part.to_ascii_lowercase().as_str() {
        "" | "asc" | "ascending" => true,
        "desc" | "descending" => false,
        _ => return Directive::Unhandled(format!("@sort {rest}")),
    };
    Directive::Sort { field, ascending }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_right() {
        let d = parse_directive_cell("{{ @repeat right }}").unwrap();
        assert!(matches!(d.as_slice(), [Directive::Repeat(Direction::Right)]));
    }

    #[test]
    fn repeat_default_down() {
        let d = parse_directive_cell("{{ @repeat }}").unwrap();
        assert!(matches!(d.as_slice(), [Directive::Repeat(Direction::Down)]));
    }

    #[test]
    fn mixed_text_is_not_directive() {
        assert!(parse_directive_cell("prefix {{ @repeat right }}").is_none());
    }

    #[test]
    fn data_block_is_not_directive() {
        assert!(parse_directive_cell("{{ [Customer] }}").is_none());
    }
}
