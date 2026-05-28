//! Stable error-code surface, mirroring xl3 (TS)'s `XtlError` /
//! `xtlError` / `isXtlError` (ADR-0015) and xl3-py's `XtlError`.
//!
//! Status: this is the *type-level* parity. Internally most call
//! sites still throw `anyhow::Error` with a free-form message — they
//! migrate to `XtlError::new(code, msg)` as we touch each one. The
//! down-cast helper lets a host (xl3-wasm, conformance runner) ask
//! "is this a known XTL error?" today regardless of how many sites
//! have moved.

use std::fmt;

/// One known XTL error code, mirroring the slash-namespaced strings
/// the TS/py implementations emit (e.g. `xl3/source/sheet-missing`).
/// Stored as a free-form string so we can stay in sync with the
/// canonical catalogue without versioning a Rust enum every time a
/// new code lands upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XtlError {
    pub code: String,
    pub message: String,
}

impl XtlError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        XtlError {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for XtlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for XtlError {}

/// Downcast helper mirroring xl3 (TS) `isXtlError(e)` / xl3-py
/// `is_xtl_error(e)`. Returns the `&XtlError` view when the anyhow
/// chain originated from an XtlError, otherwise `None`.
pub fn is_xtl_error(err: &anyhow::Error) -> Option<&XtlError> {
    err.downcast_ref::<XtlError>()
}

/// A few canonical codes hosts will want to match on without hard-
/// coding magic strings. The full catalogue (36 in TS, 43 in py)
/// stays in the slash-string namespace — these are just the ones the
/// Rust core actively emits today.
pub mod code {
    pub const SOURCE_SHEET_MISSING: &str = "xl3/source/sheet-missing";
    pub const SOURCE_NO_HEADER: &str = "xl3/source/no-header";
    pub const SOURCE_DUPLICATE_COLUMN: &str = "xl3/source/duplicate-column";
    pub const SOURCE_MISSING_HEADER: &str = "xl3/source/missing-header";
    pub const SOURCE_DUPLICATE_NAME: &str = "xl3/source/duplicate-name";
    pub const SOURCE_UNKNOWN_COLUMN: &str = "xl3/source/unknown-column";
    pub const SOURCE_ROW_CROSS_BLOCK: &str = "xl3/source/row-cross-block";
    pub const SOURCE_RESERVED_COLUMN_NAME: &str = "xl3/source/reserved-column-name";
    pub const CONFIG_INVALID_SOURCE_TABLE: &str = "xl3/config/invalid-source-table";
    pub const CELL_FORMULA_NO_CACHE: &str = "xl3/cell/formula-no-cache";
    pub const CELL_NUMFMT_COERCION: &str = "xl3/cell/numfmt-coercion";
    pub const CELL_ROW_OUTSIDE_REPEAT: &str = "xl3/cell/row-outside-repeat";
    pub const EVAL_DIV_BY_ZERO: &str = "xl3/eval/div-by-zero";
    pub const EVAL_UNSUPPORTED_SYNTAX: &str = "xl3/eval/unsupported-syntax";
    pub const EVAL_UNKNOWN_NAME: &str = "xl3/expression/unknown-name";
    pub const EVAL_ARITY_MISMATCH: &str = "xl3/eval/arity-mismatch";
    pub const EVAL_OPERAND_COERCION: &str = "xl3/eval/operand-coercion";
    pub const DIRECTIVE_BAD_JOIN: &str = "xl3/directive/bad-join";
    pub const XLOOKUP_BARE_BRACKET: &str = "xl3/xlookup/bare-bracket";
    pub const XLOOKUP_SOURCE_MISMATCH: &str = "xl3/xlookup/source-mismatch";
    pub const XLOOKUP_NO_MATCH: &str = "xl3/xlookup/no-match";
    pub const PARSER_EMPTY_BLOCK: &str = "xl3/parser/empty-block";
    pub const FILENAME_EMPTY: &str = "xl3/filename/empty";
    pub const FILENAME_TOO_LONG: &str = "xl3/filename/too-long";
    pub const FILENAME_COLLISION: &str = "xl3/filename/collision";
    pub const FILENAME_SANITIZED: &str = "xl3w/filename/sanitized";
    pub const INPUTS_MISSING_REQUIRED: &str = "xl3/inputs/missing-required";
    pub const SUBTOTAL_OUTSIDE_GROUP: &str = "xl3/subtotal/outside-group";
    pub const TEMPLATE_NO_SHEETS: &str = "xl3/template/no-visible-sheets";
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    fn fails_with_code() -> Result<()> {
        Err(XtlError::new(code::EVAL_DIV_BY_ZERO, "test message").into())
    }

    #[test]
    fn downcasts_through_anyhow() {
        let err = fails_with_code().unwrap_err();
        let xtl = is_xtl_error(&err).expect("expected XtlError");
        assert_eq!(xtl.code, "xl3/eval/div-by-zero");
        assert_eq!(xtl.message, "test message");
    }

    #[test]
    fn non_xtl_error_returns_none() {
        let err: anyhow::Error = anyhow::anyhow!("plain anyhow");
        assert!(is_xtl_error(&err).is_none());
    }

    #[test]
    fn display_uses_bracket_code() {
        let e = XtlError::new(code::SOURCE_SHEET_MISSING, "foo");
        assert_eq!(format!("{e}"), "[xl3/source/sheet-missing] foo");
    }
}
