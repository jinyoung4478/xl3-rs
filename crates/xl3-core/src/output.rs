//! Output buffer assembly.
//!
//! Phase 1 P1-A scope: take rendered rows of `Value`s and write them
//! into a fresh `rust_xlsxwriter::Workbook`, save to an in-memory buffer.
//! Phase 2 (P2-F): per-cell numFmt strings captured at planning time
//! propagate through `RenderedSheet::formats` and become
//! `rust_xlsxwriter::Format` instances. Font / fill / border
//! preservation still lives in the manifest pipeline for later.

use std::collections::HashMap;

use anyhow::Result;

use crate::rust_xlsxwriter::{Format, Workbook};
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct RenderedSheet {
    pub name: String,
    pub rows: Vec<Vec<Value>>,
    /// Per-cell numFmt code (e.g. `"0.00"`, `"yyyy-mm-dd"`). Parallel
    /// shape to `rows`; `None` means no numFmt overrides "General".
    /// Empty when no styles were captured (planner skipped the
    /// styles.xml lookup).
    pub formats: Vec<Vec<Option<String>>>,
}

pub fn write_workbook(sheets: &[RenderedSheet]) -> Result<Vec<u8>> {
    write_workbook_with_manifest(sheets, None)
}

/// Variant that also applies sheet-level manifest information
/// (merge ranges + column widths). Per-cell font/fill/alignment
/// from the manifest will join here once the planner threads the
/// style index through to `RenderedSheet`.
pub fn write_workbook_with_manifest(
    sheets: &[RenderedSheet],
    manifest: Option<&crate::manifest::StyleManifest>,
) -> Result<Vec<u8>> {
    let mut wb = Workbook::new();
    for sheet in sheets {
        let ws = wb.add_worksheet();
        ws.set_name(&sheet.name)?;
        // Apply column widths first so any later cell write doesn't
        // get truncated by a default-width auto-fit. cw maps 1:1 to
        // rust_xlsxwriter's f64 width units.
        if let Some(m) = manifest {
            for cw in m.sheet_columns(&sheet.name) {
                if cw.col <= u16::MAX as u32 {
                    let col = cw.col as u16;
                    ws.set_column_width(col, cw.width)?;
                }
            }
        }
        // Cache one `Format` per unique numFmt code so we don't add a
        // new cellXf per cell — duplicate xf entries balloon the output
        // and `wasm-opt` can't strip them.
        let mut formats: HashMap<String, Format> = HashMap::new();
        for (r, row) in sheet.rows.iter().enumerate() {
            for (c, value) in row.iter().enumerate() {
                let r32 = r as u32;
                let c16 = c as u16;
                let fmt_code: Option<&str> = sheet
                    .formats
                    .get(r)
                    .and_then(|fr| fr.get(c))
                    .and_then(|f| f.as_deref());
                if let Some(code) = fmt_code {
                    formats
                        .entry(code.to_string())
                        .or_insert_with(|| Format::new().set_num_format(code));
                }
                let fmt: Option<&Format> = fmt_code.and_then(|c| formats.get(c));
                match (value, fmt) {
                    (Value::Empty, _) => {}
                    (Value::String(s), Some(f)) => {
                        ws.write_string_with_format(r32, c16, s, f)?;
                    }
                    (Value::String(s), None) => {
                        ws.write_string(r32, c16, s)?;
                    }
                    (Value::Number(n) | Value::DateNumber(n), Some(f)) => {
                        ws.write_number_with_format(r32, c16, *n, f)?;
                    }
                    (Value::Number(n) | Value::DateNumber(n), None) => {
                        ws.write_number(r32, c16, *n)?;
                    }
                    (Value::Bool(b), Some(f)) => {
                        ws.write_boolean_with_format(r32, c16, *b, f)?;
                    }
                    (Value::Bool(b), None) => {
                        ws.write_boolean(r32, c16, *b)?;
                    }
                    (Value::Rows(_) | Value::Map(_) | Value::List(_), _) => {
                        // Internal context values — never emitted to a
                        // cell. Defensive no-op.
                    }
                }
            }
        }
        // Merge ranges land after cell writes — rust_xlsxwriter
        // requires the cell at (first_row, first_col) to exist
        // before `merge_range` is called.
        if let Some(m) = manifest {
            for range in m.sheet_merges(&sheet.name) {
                let Some((fr, fc, lr, lc)) = parse_a1_range(range) else {
                    // Skip silently — a malformed range from the
                    // manifest shouldn't blow up the whole render.
                    continue;
                };
                // The merged cells need to exist; rust_xlsxwriter
                // writes a blank merged cell with optional format
                // when we pass an empty placeholder.
                let blank = Format::new();
                ws.merge_range(fr, fc, lr, lc, "", &blank).ok();
            }
        }
    }
    Ok(wb.save_to_buffer()?)
}

/// Parse `"A1:B2"` → `(first_row, first_col, last_row, last_col)`
/// in zero-based form. Returns `None` for any shape that isn't a
/// rectangular A1 range.
fn parse_a1_range(s: &str) -> Option<(u32, u16, u32, u16)> {
    let (a, b) = s.split_once(':')?;
    let (r1, c1) = parse_a1_cell(a)?;
    let (r2, c2) = parse_a1_cell(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

fn parse_a1_cell(s: &str) -> Option<(u32, u16)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut col: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        col = col * 26 + (bytes[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
    }
    if col == 0 || i == bytes.len() {
        return None;
    }
    let row: u32 = std::str::from_utf8(&bytes[i..]).ok()?.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, (col - 1) as u16))
}
