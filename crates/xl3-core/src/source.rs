//! Source data reader. Reads the data workbook (`data.xlsx`) into row
//! records keyed by the header row.
//!
//! Phase 1 P1-A scope: single source per workbook (the one named by
//! `__config__.source_sheet`). Multi-source declaration via `__sources__`
//! lands later.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::calamine::{open_workbook, Data as CData, Reader, Xlsx};
use crate::plan::SourceTable;
use crate::styles::{self, NumFmtKind, TemplateStyles};
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct SourceData {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<HashMap<String, Value>>,
}

pub trait SourceReader {
    fn read(&mut self, sheet: &str, table: &SourceTable) -> Result<SourceData>;
}

/// Default `SourceReader` backed by `calamine`. Assumes the source is
/// laid out as a single header row at row 1, followed by data rows.
/// (xl3 0.x's `source_table = 1` convention.)
pub struct CalamineSourceReader {
    workbook: Xlsx<std::io::BufReader<std::fs::File>>,
    /// Source workbook styles — only parsed lazily on first need.
    /// Used to render numeric cells whose numFmt is a date format as
    /// canonical ISO strings (ADR-0017).
    path: PathBuf,
    styles: Option<TemplateStyles>,
}

impl CalamineSourceReader {
    pub fn open(path: &Path) -> Result<Self> {
        let workbook: Xlsx<_> = open_workbook(path)
            .with_context(|| format!("open data workbook at {}", path.display()))?;
        Ok(CalamineSourceReader {
            workbook,
            path: path.to_path_buf(),
            styles: None,
        })
    }

    fn ensure_styles(&mut self) -> &TemplateStyles {
        if self.styles.is_none() {
            self.styles = Some(styles::parse_template_styles(&self.path).unwrap_or_default());
        }
        self.styles.as_ref().unwrap()
    }

    /// Convenience: when only one source is named in `__config__`, load
    /// the first non-empty sheet.
    pub fn first_sheet(&self) -> Option<String> {
        self.workbook.sheet_names().into_iter().next()
    }

    /// Resolve a `source_sheet` configuration value to an actual sheet
    /// name in the workbook. Accepts:
    /// - exact name (`"Data"`) — returned as-is iff present
    /// - prefix glob (`"Data_*"`) — first sheet in workbook order whose
    ///   name starts with the literal prefix (xl3 evaluation.md
    ///   "Source Data Model")
    pub fn resolve_sheet_name(&self, pattern: &str) -> Option<String> {
        if let Some(prefix) = pattern.strip_suffix('*') {
            self.workbook
                .sheet_names()
                .into_iter()
                .find(|n| n.starts_with(prefix))
        } else {
            let pattern = pattern.to_string();
            self.workbook
                .sheet_names()
                .into_iter()
                .find(|n| n == &pattern)
        }
    }
}

/// True if a value should be treated as blank.
/// Matches xl3's ADR-0007: explicit `Empty` plus strings that contain
/// only whitespace. Other types — numbers (including 0), booleans
/// (including `false`) — are NOT blank.
///
/// Used by:
/// - source reader (skip blank rows)
/// - `COUNT([Field])` row aggregate (only non-blank values count)
/// - any future code that needs the same notion of "missing data"
pub fn is_blank_value(v: &Value) -> bool {
    match v {
        Value::Empty => true,
        Value::String(s) => s.chars().all(|c| c.is_whitespace()),
        _ => false,
    }
}

impl SourceReader for CalamineSourceReader {
    fn read(&mut self, sheet: &str, table: &SourceTable) -> Result<SourceData> {
        // Styles & merge info — pulled before worksheet_range so the
        // two `&mut self.workbook` borrows don't overlap.
        self.ensure_styles();
        let styles = self.styles.clone().unwrap_or_default();
        let header_merges = self
            .workbook
            .worksheet_merge_cells(sheet)
            .and_then(|r| r.ok())
            .unwrap_or_default();

        let range = self
            .workbook
            .worksheet_range(sheet)
            .with_context(|| format!("read source sheet {sheet:?}"))?;

        if range.get_size() == (0, 0) {
            return Ok(SourceData {
                name: sheet.to_string(),
                headers: vec![],
                rows: vec![],
            });
        }

        // calamine returns a `Range` whose `(0, 0)` is the first *used*
        // cell, not sheet A1. The xl3 conventions (and the SourceTable
        // values we receive) are in absolute 1-based A1 coordinates, so
        // we read via `Range::get_value(absolute)` and never use the
        // relative `get`. `end()` gives the absolute bottom-right (also
        // 0-based) so we know how far down to walk.
        let (last_row_abs, last_col_abs) = range
            .end()
            .map(|(r, c)| (r as usize, c as usize))
            .unwrap_or((0, 0));

        // Resolve the (header_row, data_row_range, col_range) tuple
        // from the SourceTable. All indices below are absolute, 0-based.
        let (header_row, data_first, data_last_excl, col_first, col_last_excl) = match table {
            SourceTable::HeaderRow(n) => {
                let header = n.saturating_sub(1);
                let row_end_excl = last_row_abs + 1;
                let col_end_excl = last_col_abs + 1;
                (header, header + 1, row_end_excl, 0usize, col_end_excl)
            }
            SourceTable::Range {
                first_row,
                last_row,
                first_col,
                last_col,
            } => {
                let header = first_row.saturating_sub(1);
                let data_first = header + 1;
                let data_last_excl = match last_row {
                    Some(lr) => (*lr).min(last_row_abs + 1),
                    None => last_row_abs + 1,
                };
                let col_first0 = first_col.saturating_sub(1);
                let col_last_excl0 = match last_col {
                    Some(lc) => (*lc).min(last_col_abs + 1),
                    None => last_col_abs + 1,
                };
                (header, data_first, data_last_excl, col_first0, col_last_excl0)
            }
        };

        if header_row > last_row_abs {
            return Err(anyhow!(
                "source sheet {sheet:?} header row {} is past the last used row {}",
                header_row + 1,
                last_row_abs + 1
            ));
        }

        // Reverse map: any cell *inside* a merge that isn't the master
        // borrows the master's value (ADR-0033 vertical slave rule).
        let slave_to_master: HashMap<(usize, usize), (u32, u32)> = header_merges
            .iter()
            .flat_map(|d| {
                let master = (d.start.0, d.start.1);
                let mut out: Vec<((usize, usize), (u32, u32))> = Vec::new();
                for r in d.start.0..=d.end.0 {
                    for c in d.start.1..=d.end.1 {
                        if (r, c) != master {
                            out.push(((r as usize, c as usize), master));
                        }
                    }
                }
                out
            })
            .collect();
        // Per-master end column lookup — used to jump past all
        // horizontal slave columns once we've emitted a master.
        let master_end_col: HashMap<(u32, u32), usize> = header_merges
            .iter()
            .map(|d| ((d.start.0, d.start.1), d.end.1 as usize))
            .collect();
        let cell_at = |r: usize, c: usize| -> Option<&CData> {
            if let Some(&(mr, mc)) = slave_to_master.get(&(r, c)) {
                range.get_value((mr, mc))
            } else {
                range.get_value((r as u32, c as u32))
            }
        };

        // Header span: cells in (header_row, col_first..col_last_excl)
        // up until the first blank. A merged-header master claims its
        // whole run as one logical column.
        let mut headers: Vec<String> = Vec::new();
        let mut header_cols: Vec<usize> = Vec::new();
        let mut c = col_first;
        let mut seen_masters: std::collections::HashSet<(u32, u32)> =
            std::collections::HashSet::new();
        while c < col_last_excl {
            // The effective master position for (header_row, c): if the
            // cell is a merge slave, follow it to the master; otherwise
            // the cell is its own master.
            let master_pos = slave_to_master
                .get(&(header_row, c))
                .copied()
                .unwrap_or((header_row as u32, c as u32));

            // ADR-0033: a master claims its whole horizontal run as one
            // logical column. Subsequent slave columns sharing the same
            // master must not contribute additional headers.
            if !seen_masters.insert(master_pos) {
                c += 1;
                continue;
            }

            let cell = range.get_value(master_pos);
            match cell {
                Some(CData::String(s)) if !s.is_empty() => {
                    headers.push(s.clone());
                    header_cols.push(c);
                    // Jump past the merge's horizontal extent so we
                    // resume scanning at the next logical column.
                    if let Some(&end_col) = master_end_col.get(&master_pos) {
                        c = end_col + 1;
                    } else {
                        c += 1;
                    }
                }
                None | Some(CData::Empty) => break,
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

        let mut data_rows = Vec::with_capacity(data_last_excl.saturating_sub(data_first));
        for r in data_first..data_last_excl {
            let mut record = HashMap::with_capacity(headers.len());
            let mut row_blank = true;
            for (i, header) in headers.iter().enumerate() {
                let c = header_cols[i];
                let raw = cell_at(r, c)
                    .map(Value::from_calamine)
                    .unwrap_or(Value::Empty);
                // ADR-0017: a numeric source cell whose style is a date
                // numFmt becomes Value::DateNumber. Arithmetic and
                // comparison treat it like Number; only `canonical()`
                // (used by string concat) renders the ISO form.
                let v = match &raw {
                    Value::Number(n) => {
                        let kind = styles
                            .format_code(sheet, r as u32, c as u32)
                            .as_deref()
                            .map(styles::classify_num_fmt)
                            .unwrap_or(NumFmtKind::General);
                        if kind == NumFmtKind::Date {
                            Value::DateNumber(*n)
                        } else {
                            raw
                        }
                    }
                    _ => raw,
                };
                if !is_blank_value(&v) {
                    row_blank = false;
                }
                record.insert(header.clone(), v);
            }
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
