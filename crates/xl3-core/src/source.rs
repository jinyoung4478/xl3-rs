//! Source data reader. Reads the data workbook (`data.xlsx`) into row
//! records keyed by the header row.
//!
//! Phase 1 P1-A scope: single source per workbook (the one named by
//! `__config__.source_sheet`). Multi-source declaration via `__sources__`
//! lands later.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::calamine::{open_workbook, Data as CData, Reader, Xlsx};
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct SourceData {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<HashMap<String, Value>>,
}

pub trait SourceReader {
    fn read(&mut self, sheet: &str) -> Result<SourceData>;
}

/// Default `SourceReader` backed by `calamine`. Assumes the source is
/// laid out as a single header row at row 1, followed by data rows.
/// (xl3 0.x's `source_table = 1` convention.)
pub struct CalamineSourceReader {
    workbook: Xlsx<std::io::BufReader<std::fs::File>>,
}

impl CalamineSourceReader {
    pub fn open(path: &Path) -> Result<Self> {
        let workbook: Xlsx<_> = open_workbook(path)
            .with_context(|| format!("open data workbook at {}", path.display()))?;
        Ok(CalamineSourceReader { workbook })
    }

    /// Convenience: when only one source is named in `__config__`, load
    /// the first non-empty sheet.
    pub fn first_sheet(&self) -> Option<String> {
        self.workbook.sheet_names().into_iter().next()
    }
}

/// True if a value should be treated as blank by the source reader.
/// Matches xl3's ADR-0007: explicit `Empty` plus strings that contain
/// only whitespace. Other types — numbers (including 0), booleans
/// (including `false`) — are NOT blank.
fn is_blank_value(v: &Value) -> bool {
    match v {
        Value::Empty => true,
        Value::String(s) => s.chars().all(|c| c.is_whitespace()),
        _ => false,
    }
}

impl SourceReader for CalamineSourceReader {
    fn read(&mut self, sheet: &str) -> Result<SourceData> {
        let range = self
            .workbook
            .worksheet_range(sheet)
            .with_context(|| format!("read source sheet {sheet:?}"))?;

        let (rows, cols) = range.get_size();
        if rows == 0 {
            return Ok(SourceData {
                name: sheet.to_string(),
                headers: vec![],
                rows: vec![],
            });
        }

        // Header row: first row, left-to-right, strings only. Blanks end
        // the header span (xl3 convention — header is a contiguous run).
        let mut headers: Vec<String> = Vec::new();
        for c in 0..cols {
            match range.get((0, c)) {
                Some(CData::String(s)) if !s.is_empty() => headers.push(s.clone()),
                Some(CData::Empty) | None => break,
                Some(other) => {
                    bail!(
                        "source header at column {c} is not a string: {other:?} (xl3 expects text headers)"
                    );
                }
            }
        }

        if headers.is_empty() {
            return Err(anyhow!("source sheet {sheet:?} has no header row"));
        }

        let mut data_rows = Vec::with_capacity(rows.saturating_sub(1));
        for r in 1..rows {
            let mut record = HashMap::with_capacity(headers.len());
            let mut row_blank = true;
            for (c, header) in headers.iter().enumerate() {
                let v = range
                    .get((r, c))
                    .map(Value::from_calamine)
                    .unwrap_or(Value::Empty);
                if !is_blank_value(&v) {
                    row_blank = false;
                }
                record.insert(header.clone(), v);
            }
            // ADR-0007: a row whose every cell is empty *or* whitespace-only
            // is skipped — even if it sits between two non-blank rows. xl3
            // does not terminate the source on a blank line; the corpus
            // explicitly covers "blank row in the middle, keep later rows".
            if row_blank {
                continue;
            }
            data_rows.push(record);
        }

        Ok(SourceData {
            name: sheet.to_string(),
            headers,
            rows: data_rows,
        })
    }
}
