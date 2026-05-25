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
    let mut wb = Workbook::new();
    for sheet in sheets {
        let ws = wb.add_worksheet();
        ws.set_name(&sheet.name)?;
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
    }
    Ok(wb.save_to_buffer()?)
}
