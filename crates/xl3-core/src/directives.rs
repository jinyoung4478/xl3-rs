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
        _ => Directive::Unhandled(format!("@{name} {rest}").trim().to_string()),
    })
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
