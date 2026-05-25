//! Top-level renderer. Glues `plan` + `source` + `eval` + `output`.
//!
//! Phase 1 P1-A scope: single source per workbook, expand-down rows,
//! no manifest preservation. Returns the rendered workbook as a byte
//! buffer.

use std::path::Path;

use anyhow::{Context, Result};

use std::collections::HashMap;
use std::sync::Arc;

use crate::directives::Directive;
use crate::eval::{
    compare, eval_cell, eval_expression_str, inject_rownum, inject_rows, is_truthy, EvalContext,
};
use crate::output::{write_workbook, RenderedSheet};
use crate::plan::{
    inputs_to_value, parse_template, CellSource, RowPlan, SheetPlan, WorkbookPlan,
};
use crate::source::{CalamineSourceReader, SourceData, SourceReader};
use crate::value::Value;

/// Convenience for the conformance runner: parse the template, load the
/// source workbook, render, return bytes.
pub fn render_from_paths(template: &Path, data: &Path) -> Result<Vec<u8>> {
    let plan = parse_template(template).context("parse template")?;
    let mut source_reader = CalamineSourceReader::open(data).context("open source workbook")?;
    let source_sheet = plan
        .config
        .source_sheet()
        .map(str::to_string)
        .or_else(|| source_reader.first_sheet())
        .ok_or_else(|| anyhow::anyhow!("no source_sheet in __config__ and source workbook is empty"))?;
    let source = source_reader.read(&source_sheet)?;
    render(&plan, &source)
}

pub fn render(plan: &WorkbookPlan, source: &SourceData) -> Result<Vec<u8>> {
    // Pre-bundle the reserved-namespace values that don't change
    // between expansion rows. The renderer slots them into every ctx
    // it builds.
    let inputs_value = inputs_to_value(&plan.inputs);
    let mut out_sheets = Vec::with_capacity(plan.sheets.len());
    for sheet in &plan.sheets {
        out_sheets.push(render_sheet(sheet, source, &inputs_value)?);
    }
    write_workbook(&out_sheets)
}

fn render_sheet(
    plan: &SheetPlan,
    source: &SourceData,
    inputs_value: &Value,
) -> Result<RenderedSheet> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for row in &plan.rows {
        match row {
            RowPlan::Static(cells) => {
                rows.push(render_static_row(cells, inputs_value)?);
            }
            RowPlan::ExpandDown { cells, directives } => {
                let effective = apply_directives(&source.rows, directives)?;
                let rows_handle: Arc<Vec<HashMap<String, Value>>> = Arc::new(effective.clone());
                for (idx, source_row) in effective.iter().enumerate() {
                    let mut ctx: EvalContext = source_row.clone();
                    inject_rows(&mut ctx, Arc::clone(&rows_handle));
                    inject_rownum(&mut ctx, idx + 1);
                    ctx.insert("__inputs__".to_string(), inputs_value.clone());
                    rows.push(render_template_row(cells, &ctx)?);
                }
            }
            RowPlan::ExpandRight { cells, directives } => {
                let effective = apply_directives(&source.rows, directives)?;
                rows.push(render_expand_right_row(cells, &effective, inputs_value)?);
            }
        }
    }
    Ok(RenderedSheet {
        name: plan.name.clone(),
        rows,
    })
}

fn apply_directives(
    rows: &[HashMap<String, Value>],
    directives: &[Directive],
) -> Result<Vec<HashMap<String, Value>>> {
    let mut current: Vec<HashMap<String, Value>> = rows.to_vec();
    for d in directives {
        match d {
            Directive::Filter(expr) => {
                let mut kept = Vec::with_capacity(current.len());
                for row in current.drain(..) {
                    let v = eval_expression_str(expr, &row)?;
                    if is_truthy(&v) {
                        kept.push(row);
                    }
                }
                current = kept;
            }
            Directive::Sort { field, ascending } => {
                let asc = *ascending;
                current.sort_by(|a, b| {
                    let av = a.get(field).cloned().unwrap_or(Value::Empty);
                    let bv = b.get(field).cloned().unwrap_or(Value::Empty);
                    let ord = compare(&av, &bv).unwrap_or(0);
                    let ordering = ord.cmp(&0);
                    if asc {
                        ordering
                    } else {
                        ordering.reverse()
                    }
                });
            }
            Directive::Top(n) => {
                current.truncate(*n);
            }
            Directive::Repeat(_) | Directive::Unhandled(_) => {
                // direction is already absorbed by the planner;
                // Unhandled is intentionally inert at this milestone.
            }
        }
    }
    Ok(current)
}

fn render_expand_right_row(
    cells: &[CellSource],
    rows: &[HashMap<String, Value>],
    inputs_value: &Value,
) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(cells.len() + rows.len());
    let mut emitted_expansion = false;
    let rows_handle: Arc<Vec<HashMap<String, Value>>> = Arc::new(rows.to_vec());
    for cell in cells {
        match cell {
            CellSource::Empty => out.push(Value::Empty),
            CellSource::Literal(v) => out.push(v.clone()),
            CellSource::Template(t) => {
                if emitted_expansion {
                    anyhow::bail!(
                        "multi-column @repeat right (two template cells in one expansion row) not yet supported"
                    );
                }
                emitted_expansion = true;
                for (idx, source_row) in rows.iter().enumerate() {
                    let mut ctx: EvalContext = source_row.clone();
                    inject_rows(&mut ctx, Arc::clone(&rows_handle));
                    inject_rownum(&mut ctx, idx + 1);
                    ctx.insert("__inputs__".to_string(), inputs_value.clone());
                    out.push(eval_cell(t, &ctx)?);
                }
            }
        }
    }
    Ok(out)
}

fn render_static_row(cells: &[CellSource], inputs_value: &Value) -> Result<Vec<Value>> {
    // "Static" rows can still contain `{{ ... }}` blocks that refer to
    // reserved namespaces (e.g. `Report month: {{ __inputs__[month] }}`).
    // xl3 evaluates these with an empty-row context plus the reserved
    // namespaces — no source row, no aggregate handle.
    let mut ctx: EvalContext = HashMap::new();
    ctx.insert("__inputs__".to_string(), inputs_value.clone());
    let mut out = Vec::with_capacity(cells.len());
    for c in cells {
        let value = match c {
            CellSource::Empty => Value::Empty,
            CellSource::Literal(v) => v.clone(),
            CellSource::Template(t) => eval_cell(t, &ctx)?,
        };
        out.push(value);
    }
    Ok(out)
}

fn render_template_row(cells: &[CellSource], ctx: &EvalContext) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(cells.len());
    for cell in cells {
        match cell {
            CellSource::Empty => out.push(Value::Empty),
            CellSource::Literal(v) => out.push(v.clone()),
            CellSource::Template(t) => out.push(eval_cell(t, ctx)?),
        }
    }
    Ok(out)
}
