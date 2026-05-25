//! Template plan: the parsed, evaluation-ready representation of a
//! template workbook.
//!
//! Phase 1 P1-A scope:
//! - parse the workbook's reserved `__config__` sheet into `ConfigMeta`
//! - for every non-reserved, visible sheet, classify each row as either
//!   a `RowPlan::Static` (copy as-is) or a `RowPlan::ExpandDown`
//!   (repeat once per source row)
//!
//! Auto-detection (xl3 0.x default): a row is an expansion row iff
//! any cell in that row contains `{{ ... }}`. Explicit `#block` /
//! `@repeat` directives land in later milestones.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::calamine::{open_workbook, Data as CData, Reader, Xlsx};
use crate::directives::{parse_directive_cell, Direction, Directive};
use crate::value::Value;

#[derive(Debug, Default, Clone)]
pub struct ConfigMeta {
    pub values: HashMap<String, String>,
}

impl ConfigMeta {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
    pub fn source_sheet(&self) -> Option<&str> {
        self.get("source_sheet")
    }
    pub fn output_file_pattern(&self) -> Option<&str> {
        self.get("output_file_pattern")
    }
}

#[derive(Debug, Clone)]
pub enum CellSource {
    Empty,
    Literal(Value),
    /// Contains at least one `{{ ... }}` expression block.
    Template(String),
}

impl CellSource {
    pub fn is_template(&self) -> bool {
        matches!(self, CellSource::Template(_))
    }
}

#[derive(Debug, Clone)]
pub enum RowPlan {
    Static(Vec<CellSource>),
    ExpandDown {
        cells: Vec<CellSource>,
        directives: Vec<Directive>,
    },
    /// Same row, repeated *to the right* once per source row. The first
    /// template cell in the row is the anchor — its column is the
    /// starting column of the expanded run.
    ExpandRight {
        cells: Vec<CellSource>,
        directives: Vec<Directive>,
    },
}

#[derive(Debug, Clone)]
pub struct SheetPlan {
    pub name: String,
    pub rows: Vec<RowPlan>,
}

#[derive(Debug, Clone)]
pub struct WorkbookPlan {
    pub config: ConfigMeta,
    pub sheets: Vec<SheetPlan>,
}

const RESERVED_SHEETS: &[&str] = &["__config__", "__inputs__", "__lists__", "__sources__"];

fn is_reserved_sheet(name: &str) -> bool {
    RESERVED_SHEETS.contains(&name)
}

fn cell_is_template_text(s: &str) -> bool {
    // Same condition the TS implementation uses: a cell is a template
    // cell iff it contains `{{`. We don't try to validate balance here;
    // `eval::eval_cell` will surface malformed expressions.
    s.contains("{{")
}

pub fn parse_template(path: &Path) -> Result<WorkbookPlan> {
    let mut wb: Xlsx<_> = open_workbook(path)
        .with_context(|| format!("open template workbook at {}", path.display()))?;

    let mut config = ConfigMeta::default();
    let sheet_names = wb.sheet_names();

    // Read __config__ first (it may not exist; xl3 lets default behavior
    // kick in then).
    if sheet_names.iter().any(|n| n == "__config__") {
        let range = wb
            .worksheet_range("__config__")
            .context("read __config__ sheet")?;
        let (rows, cols) = range.get_size();
        for r in 0..rows {
            if cols < 2 {
                break;
            }
            let key = match range.get((r, 0)) {
                Some(CData::String(s)) if !s.is_empty() => s.clone(),
                _ => continue,
            };
            let value = match range.get((r, 1)) {
                Some(CData::String(s)) => s.clone(),
                Some(CData::Float(f)) => format!("{f}"),
                Some(CData::Int(i)) => format!("{i}"),
                Some(CData::Bool(b)) => b.to_string(),
                _ => String::new(),
            };
            config.values.insert(key, value);
        }
    }

    let mut sheets = Vec::with_capacity(sheet_names.len());
    for name in sheet_names {
        if is_reserved_sheet(&name) {
            continue;
        }
        let range = wb
            .worksheet_range(&name)
            .with_context(|| format!("read template sheet {name:?}"))?;
        let (rows, cols) = range.get_size();
        let mut row_plans = Vec::with_capacity(rows);
        // Pending state from previous directive rows. xl3 attaches all
        // directive rows that precede the next data row to that row,
        // in declaration order.
        let mut pending_direction = Direction::Down;
        let mut pending_directives: Vec<Directive> = Vec::new();
        for r in 0..rows {
            let mut row_cells = Vec::with_capacity(cols);
            let mut has_template = false;
            let mut directive_only = true;
            let mut any_cell = false;
            for c in 0..cols {
                let cell = match range.get((r, c)) {
                    None | Some(CData::Empty) => CellSource::Empty,
                    Some(CData::String(s)) if cell_is_template_text(s) => {
                        any_cell = true;
                        if parse_directive_cell(s).is_some() {
                            // Directive cells don't surface in output;
                            // they contribute their metadata instead.
                            CellSource::Empty
                        } else {
                            has_template = true;
                            directive_only = false;
                            CellSource::Template(s.clone())
                        }
                    }
                    Some(other) => {
                        any_cell = true;
                        directive_only = false;
                        CellSource::Literal(Value::from_calamine(other))
                    }
                };
                row_cells.push(cell);
            }

            // A row whose template cells are *all* directive-only is a
            // directive row — pull its directives into `pending_*` and
            // omit it from the plan.
            if any_cell && directive_only {
                for c in 0..cols {
                    if let Some(CData::String(s)) = range.get((r, c)) {
                        if let Some(directives) = parse_directive_cell(s) {
                            for d in directives {
                                match d {
                                    Directive::Repeat(dir) => pending_direction = dir,
                                    other => pending_directives.push(other),
                                }
                            }
                        }
                    }
                }
                continue;
            }

            let row_plan = if has_template {
                let directives = std::mem::take(&mut pending_directives);
                let plan = match pending_direction {
                    Direction::Down => RowPlan::ExpandDown {
                        cells: row_cells,
                        directives,
                    },
                    Direction::Right => RowPlan::ExpandRight {
                        cells: row_cells,
                        directives,
                    },
                };
                pending_direction = Direction::Down;
                plan
            } else {
                RowPlan::Static(row_cells)
            };
            row_plans.push(row_plan);
        }
        sheets.push(SheetPlan {
            name,
            rows: row_plans,
        });
    }

    // Sanity check that we picked up the bits we need.
    if sheets.is_empty() {
        bail!("template has no visible (non-reserved) sheets");
    }

    Ok(WorkbookPlan { config, sheets })
}
