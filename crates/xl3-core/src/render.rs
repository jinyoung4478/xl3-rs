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
    inputs_to_value, lists_to_value, parse_template, CellSource, RowPlan, SheetPlan, WorkbookPlan,
};
use crate::source::{CalamineSourceReader, SourceData, SourceReader};
use crate::value::Value;

/// Convenience for the conformance runner: parse the template, load the
/// source workbook, render, return bytes.
pub fn render_from_paths(template: &Path, data: &Path) -> Result<Vec<u8>> {
    render_from_paths_with_inputs(template, data, &HashMap::new())
}

/// Variant that lets the host supply `__inputs__` overrides — used by
/// the conformance runner when a fixture's `meta.yaml` declares
/// runtime inputs (ADR-0010).
pub fn render_from_paths_with_inputs(
    template: &Path,
    data: &Path,
    host_inputs: &HashMap<String, Value>,
) -> Result<Vec<u8>> {
    let mut plan = parse_template(template).context("parse template")?;
    for (key, value) in host_inputs {
        plan.inputs.insert(key.clone(), value.clone());
    }
    let mut source_reader = CalamineSourceReader::open(data).context("open source workbook")?;
    let source_sheet = match plan.config.source_sheet() {
        Some(pattern) => source_reader.resolve_sheet_name(pattern).ok_or_else(|| {
            anyhow::anyhow!(
                "source_sheet pattern {pattern:?} does not match any sheet in the data workbook"
            )
        })?,
        None => source_reader
            .first_sheet()
            .ok_or_else(|| anyhow::anyhow!("source workbook is empty"))?,
    };
    let source_table = plan.config.source_table();
    let source = source_reader.read(&source_sheet, &source_table)?;
    // Load every additional named source declared on `__sources__`.
    let mut named_sources: HashMap<String, SourceData> = HashMap::new();
    for (name, decl) in &plan.named_sources {
        let data = source_reader.read(&decl.sheet, &decl.table)?;
        named_sources.insert(name.clone(), data);
    }
    render_with_sources(&plan, &source, &named_sources)
}


pub fn render(plan: &WorkbookPlan, source: &SourceData) -> Result<Vec<u8>> {
    render_with_sources(plan, source, &HashMap::new())
}

pub fn render_with_sources(
    plan: &WorkbookPlan,
    source: &SourceData,
    named_sources: &HashMap<String, SourceData>,
) -> Result<Vec<u8>> {
    // Pre-bundle the reserved-namespace values that don't change
    // between expansion rows. The renderer slots them into every ctx
    // it builds.
    let inputs_value = inputs_to_value(&plan.inputs);
    let lists_value = lists_to_value(&plan.lists);
    // Each named source becomes a `Value::Rows` handle that the
    // evaluator can aggregate over via `Source[Field]` references.
    let named_source_handles: HashMap<String, Value> = named_sources
        .iter()
        .map(|(name, data)| {
            let handle: Arc<Vec<HashMap<String, Value>>> = Arc::new(data.rows.clone());
            (name.clone(), Value::Rows(handle))
        })
        .collect();
    let mut out_sheets = Vec::with_capacity(plan.sheets.len());
    for sheet in &plan.sheets {
        if sheet.name.contains("{{") {
            // Sheet-name is a template — ADR-0016: split the source by
            // the evaluated key in first-seen order, emit one rendered
            // sheet per group.
            let groups = split_source_by_sheet_name(
                &sheet.name,
                source,
                &inputs_value,
                &lists_value,
                &named_source_handles,
            )?;
            for (group_name, group_source) in groups {
                let mut rs = render_sheet(
                    sheet,
                    &group_source,
                    &inputs_value,
                    &lists_value,
                    &named_source_handles,
                )?;
                rs.name = sanitize_sheet_name(&group_name);
                out_sheets.push(rs);
            }
        } else {
            out_sheets.push(render_sheet(
                sheet,
                source,
                &inputs_value,
                &lists_value,
                &named_source_handles,
            )?);
        }
    }
    write_workbook(&out_sheets)
}

/// xlsx limits sheet names to 31 characters and disallows `:\/?*[]`.
/// Replace illegal chars with `_` and truncate so write_workbook does
/// not error on a group key that happens to contain whitespace, dates,
/// etc.
fn sanitize_sheet_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            ':' | '\\' | '/' | '?' | '*' | '[' | ']' => '_',
            _ => c,
        })
        .collect();
    if cleaned.chars().count() <= 31 {
        cleaned
    } else {
        cleaned.chars().take(31).collect()
    }
}

/// Partition the source rows by the evaluated sheet-name template
/// (xl3 ADR-0016 first-seen order). Returns `(group_key, group_source)`
/// pairs preserving order of first appearance.
fn split_source_by_sheet_name(
    template: &str,
    source: &SourceData,
    inputs_value: &Value,
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
) -> Result<Vec<(String, SourceData)>> {
    let mut groups: Vec<(String, Vec<HashMap<String, Value>>)> = Vec::new();
    for row in &source.rows {
        let mut ctx: EvalContext = row.clone();
        ctx.insert("__inputs__".to_string(), inputs_value.clone());
        ctx.insert("__lists__".to_string(), lists_value.clone());
        inject_named_sources(&mut ctx, named_sources);
        let key_value = eval_cell(template, &ctx)?;
        let raw_key = key_value.canonical();
        // ADR-0026: an empty / whitespace-only group key is substituted
        // with the literal `(blank)` placeholder before sheet-name
        // interpolation. Otherwise we'd hit rust_xlsxwriter's "sheet
        // name cannot be blank" error.
        let key = if raw_key.chars().all(char::is_whitespace) {
            "(blank)".to_string()
        } else {
            raw_key
        };
        if let Some(g) = groups.iter_mut().find(|g| g.0 == key) {
            g.1.push(row.clone());
        } else {
            groups.push((key, vec![row.clone()]));
        }
    }
    Ok(groups
        .into_iter()
        .map(|(key, rows)| {
            (
                key,
                SourceData {
                    name: source.name.clone(),
                    headers: source.headers.clone(),
                    rows,
                },
            )
        })
        .collect())
}

fn render_sheet(
    plan: &SheetPlan,
    source: &SourceData,
    inputs_value: &Value,
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
) -> Result<RenderedSheet> {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for row in &plan.rows {
        match row {
            RowPlan::Static(cells) => {
                rows.push(render_static_row(
                    cells,
                    inputs_value,
                    lists_value,
                    named_sources,
                )?);
            }
            RowPlan::ExpandDown {
                cells,
                directives,
                subtotal_rows,
            } => {
                let block_rows = resolve_block_rows(directives, source, named_sources);
                let effective =
                    apply_directives(&block_rows, directives, lists_value, named_sources)?;
                let group_fields: Vec<String> = directives
                    .iter()
                    .find_map(|d| match d {
                        Directive::Group(fs) => Some(fs.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let rows_handle: Arc<Vec<HashMap<String, Value>>> = Arc::new(effective.clone());

                let mut global_idx = 0usize;
                let mut emit_expansion =
                    |group_rows: &Vec<HashMap<String, Value>>,
                     rows: &mut Vec<Vec<Value>>,
                     global_idx: &mut usize|
                     -> Result<()> {
                        for source_row in group_rows {
                            *global_idx += 1;
                            let mut ctx: EvalContext = source_row.clone();
                            inject_rows(&mut ctx, Arc::clone(&rows_handle));
                            inject_rownum(&mut ctx, *global_idx);
                            ctx.insert("__inputs__".to_string(), inputs_value.clone());
                            ctx.insert("__lists__".to_string(), lists_value.clone());
                            inject_named_sources(&mut ctx, named_sources);
                            rows.push(render_template_row(cells, &ctx)?);
                        }
                        Ok(())
                    };

                if group_fields.is_empty() {
                    // No grouping: emit every row, then any trailing
                    // subtotal rows once over the whole block.
                    emit_expansion(&effective, &mut rows, &mut global_idx)?;
                    for subtotal_cells in subtotal_rows {
                        rows.push(render_subtotal_row(
                            subtotal_cells,
                            &rows_handle,
                            inputs_value,
                            lists_value,
                            named_sources,
                        )?);
                    }
                } else {
                    render_grouped(
                        &effective,
                        &group_fields,
                        0,
                        cells,
                        subtotal_rows,
                        &mut rows,
                        &mut global_idx,
                        &rows_handle,
                        inputs_value,
                        lists_value,
                        named_sources,
                    )?;
                }
            }
            RowPlan::ExpandRight { cells, directives } => {
                let block_rows = resolve_block_rows(directives, source, named_sources);
                let effective = apply_directives(&block_rows, directives, lists_value, named_sources)?;
                rows.push(render_expand_right_row(
                    cells,
                    &effective,
                    inputs_value,
                    lists_value,
                    named_sources,
                )?);
            }
        }
    }
    Ok(RenderedSheet {
        name: plan.name.clone(),
        rows,
    })
}

/// Resolve which row set the expansion block iterates over. With no
/// `@source` directive (the common case) it's the default source's
/// rows. With `@source Name`, look up the named source — error
/// permissively to an empty row set if the name isn't declared, so
/// later directives still apply consistently.
fn resolve_block_rows(
    directives: &[Directive],
    default_source: &SourceData,
    named_sources: &HashMap<String, Value>,
) -> Vec<HashMap<String, Value>> {
    if let Some(name) = directives.iter().find_map(|d| match d {
        Directive::Source(n) => Some(n.as_str()),
        _ => None,
    }) {
        match named_sources.get(name) {
            Some(Value::Rows(handle)) => handle.as_ref().clone(),
            _ => Vec::new(),
        }
    } else {
        default_source.rows.clone()
    }
}

/// Split `rows` into consecutive groups of equal-valued `field`. With
/// `field = None` the whole row set is one group. Equality uses the
/// expression-language `compare()` so numeric/string differences match
/// xl3's evaluator. Assumes the caller already applied any `@sort`
/// directive — xl3 groups *consecutive* rows, not all rows with the
/// same key.
fn partition_into_groups(
    rows: &[HashMap<String, Value>],
    field: Option<&str>,
) -> Vec<Vec<HashMap<String, Value>>> {
    let Some(field) = field else {
        return vec![rows.to_vec()];
    };
    let mut out: Vec<Vec<HashMap<String, Value>>> = Vec::new();
    let mut current_key: Option<Value> = None;
    for row in rows {
        let key = row.get(field).cloned().unwrap_or(Value::Empty);
        let same = current_key
            .as_ref()
            .map(|prev| crate::eval::compare(prev, &key).map(|c| c == 0).unwrap_or(false))
            .unwrap_or(false);
        if same {
            out.last_mut().unwrap().push(row.clone());
        } else {
            out.push(vec![row.clone()]);
            current_key = Some(key);
        }
    }
    out
}

/// Recursive nested-group emission. ADR-0038:
/// - groups are nested left-to-right (`@group [Outer], [Inner]`)
/// - leaf groups (deepest level) emit their data rows
/// - on the way back up, each completed group emits one subtotal row
///   per attached `subtotal_rows` slot, where slot index 0 is the
///   innermost level's row, slot 1 is the next outer, etc.
fn render_grouped(
    rows: &[HashMap<String, Value>],
    group_fields: &[String],
    depth: usize,
    cells: &[CellSource],
    subtotal_rows: &[Vec<CellSource>],
    out_rows: &mut Vec<Vec<Value>>,
    global_idx: &mut usize,
    rows_handle: &Arc<Vec<HashMap<String, Value>>>,
    inputs_value: &Value,
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
) -> Result<()> {
    if depth == group_fields.len() {
        // Leaf — emit every row through the expansion template.
        for source_row in rows {
            *global_idx += 1;
            let mut ctx: EvalContext = source_row.clone();
            inject_rows(&mut ctx, Arc::clone(rows_handle));
            inject_rownum(&mut ctx, *global_idx);
            ctx.insert("__inputs__".to_string(), inputs_value.clone());
            ctx.insert("__lists__".to_string(), lists_value.clone());
            inject_named_sources(&mut ctx, named_sources);
            out_rows.push(render_template_row(cells, &ctx)?);
        }
        return Ok(());
    }
    let groups = partition_into_groups(rows, Some(&group_fields[depth]));
    for group in &groups {
        render_grouped(
            group,
            group_fields,
            depth + 1,
            cells,
            subtotal_rows,
            out_rows,
            global_idx,
            rows_handle,
            inputs_value,
            lists_value,
            named_sources,
        )?;
        // Subtotal slot index for *this* level (the level we're closing).
        // Innermost = group_fields.len() - 1 → slot 0.
        // Outermost = depth 0 → slot group_fields.len() - 1.
        let slot = group_fields.len() - 1 - depth;
        if slot < subtotal_rows.len() {
            let group_handle: Arc<Vec<HashMap<String, Value>>> = Arc::new(group.clone());
            out_rows.push(render_subtotal_row(
                &subtotal_rows[slot],
                &group_handle,
                inputs_value,
                lists_value,
                named_sources,
            )?);
        }
    }
    Ok(())
}

fn render_subtotal_row(
    cells: &[CellSource],
    group_handle: &Arc<Vec<HashMap<String, Value>>>,
    inputs_value: &Value,
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
) -> Result<Vec<Value>> {
    // Subtotal cells aggregate over the group's rows, with no current
    // row in scope. Inputs / lists / named sources stay reachable so a
    // mixed-content subtotal row (literal label + aggregate value) can
    // reference them if needed.
    let mut ctx: EvalContext = HashMap::new();
    inject_rows(&mut ctx, Arc::clone(group_handle));
    ctx.insert("__inputs__".to_string(), inputs_value.clone());
    ctx.insert("__lists__".to_string(), lists_value.clone());
    inject_named_sources(&mut ctx, named_sources);
    let mut out = Vec::with_capacity(cells.len());
    for cell in cells {
        let value = match cell {
            CellSource::Empty => Value::Empty,
            CellSource::Literal(v) => v.clone(),
            CellSource::Template(t) => eval_cell(t, &ctx)?,
            CellSource::Subtotal { aggregate, field } => {
                // Build an `<FN>([<field>])` expression and run it
                // through the evaluator — that gives us a single,
                // well-tested aggregate path instead of a parallel
                // implementation here.
                let synthetic = format!("{aggregate}([{field}])");
                eval_expression_str(&synthetic, &ctx)?
            }
        };
        out.push(value);
    }
    Ok(out)
}

fn inject_named_sources(ctx: &mut EvalContext, named_sources: &HashMap<String, Value>) {
    for (name, handle) in named_sources {
        // Avoid clobbering a same-named source-row column. The current
        // row's field takes precedence (xl3 doesn't allow conflicting
        // names, but we lean permissive here rather than erroring).
        if !ctx.contains_key(name) {
            ctx.insert(name.clone(), handle.clone());
        }
    }
}

fn apply_directives(
    rows: &[HashMap<String, Value>],
    directives: &[Directive],
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
) -> Result<Vec<HashMap<String, Value>>> {
    let mut current: Vec<HashMap<String, Value>> = rows.to_vec();
    for d in directives {
        match d {
            Directive::Filter(expr) => {
                let mut kept = Vec::with_capacity(current.len());
                for row in current.drain(..) {
                    // Filter expressions may reference `__lists__[Name]`
                    // for set-membership tests. Slot the lists value
                    // into the per-row ctx before evaluating.
                    let mut ctx = row.clone();
                    ctx.insert("__lists__".to_string(), lists_value.clone());
                    let v = eval_expression_str(expr, &ctx)?;
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
            Directive::Join {
                source,
                match_field,
                primary_field,
            } => {
                let target_rows = match named_sources.get(source) {
                    Some(Value::Rows(handle)) => Arc::clone(handle),
                    _ => {
                        anyhow::bail!(
                            "@join source {source:?} is not declared in __sources__"
                        );
                    }
                };
                let mut joined = Vec::with_capacity(current.len());
                for mut row in current.drain(..) {
                    let primary_val = row
                        .get(primary_field)
                        .cloned()
                        .unwrap_or(Value::Empty);
                    let matched = target_rows.iter().find(|t| {
                        t.get(match_field)
                            .map(|v| {
                                // Equality semantics mirror eval::compare
                                // (numeric path first, then canonical).
                                crate::eval::compare(v, &primary_val)
                                    .map(|c| c == 0)
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false)
                    });
                    if let Some(m) = matched {
                        // Promote the joined row into the per-row ctx via
                        // the same key the named source occupies. The
                        // ReservedRef path then resolves `Source[Field]`
                        // against this Map instead of the full Rows.
                        row.insert(
                            source.clone(),
                            Value::Map(Arc::new(m.clone())),
                        );
                        joined.push(row);
                    }
                }
                current = joined;
            }
            Directive::Repeat(_)
            | Directive::Source(_)
            | Directive::Group(_)
            | Directive::Unhandled(_) => {
                // Repeat: direction is absorbed by the planner.
                // Source: applied earlier by `resolve_block_rows`.
                // Group: applied at expansion time by the renderer.
                // Unhandled: inert at this milestone.
            }
        }
    }
    Ok(current)
}

fn render_expand_right_row(
    cells: &[CellSource],
    rows: &[HashMap<String, Value>],
    inputs_value: &Value,
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
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
                    ctx.insert("__lists__".to_string(), lists_value.clone());
                    inject_named_sources(&mut ctx, named_sources);
                    out.push(eval_cell(t, &ctx)?);
                }
            }
            CellSource::Subtotal { .. } => {
                // @subtotal cells inside an ExpandRight block aren't a
                // pattern xl3 emits; if one shows up we keep going so
                // the rest of the row renders.
                out.push(Value::Empty);
            }
        }
    }
    Ok(out)
}

fn render_static_row(
    cells: &[CellSource],
    inputs_value: &Value,
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
) -> Result<Vec<Value>> {
    // "Static" rows can still contain `{{ ... }}` blocks that refer to
    // reserved namespaces (e.g. `Report month: {{ __inputs__[month] }}`)
    // or aggregates over named sources. xl3 evaluates these with an
    // empty-row context plus the reserved namespaces — no source row,
    // no current-block aggregate handle.
    let mut ctx: EvalContext = HashMap::new();
    ctx.insert("__inputs__".to_string(), inputs_value.clone());
    ctx.insert("__lists__".to_string(), lists_value.clone());
    inject_named_sources(&mut ctx, named_sources);
    let mut out = Vec::with_capacity(cells.len());
    for c in cells {
        let value = match c {
            CellSource::Empty => Value::Empty,
            CellSource::Literal(v) => v.clone(),
            CellSource::Template(t) => eval_cell(t, &ctx)?,
            CellSource::Subtotal { .. } => Value::Empty,
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
            CellSource::Subtotal { .. } => out.push(Value::Empty),
        }
    }
    Ok(out)
}
