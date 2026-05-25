//! Public output types — mirror the TS / Python sibling APIs so that
//! `xl3-wasm` and any other host can return the same `OutputFile[]`
//! shape that `convert()` produces in xl3 (TS) and xl3-py.
//!
//! Multi-file split via `output_file_pattern` is a follow-up; the
//! current renderer always emits exactly one `OutputFile`.

/// A non-fatal note attached to a rendered output. Mirrors xl3 (TS)'s
/// `XtlWarning` and xl3-py's `XtlWarning`. The renderer does not yet
/// produce warnings — the field exists so callers can match against
/// the canonical surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XtlWarning {
    pub message: String,
}

/// One rendered XLSX file. The `data` buffer is the raw OOXML bytes
/// ready to write or transfer over `postMessage`. `filename` comes
/// from `__config__.output_file_pattern` (after group-key
/// substitution — currently a no-op for single-file fixtures).
#[derive(Debug, Clone)]
pub struct OutputFile {
    pub filename: String,
    pub data: Vec<u8>,
    pub warnings: Vec<XtlWarning>,
}
