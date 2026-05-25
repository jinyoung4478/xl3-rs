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
use crate::styles::NumFmtKind;
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
    // ADR-0068/0069 multi-block sheet: render each sub-block as if it
    // were its own single-block sheet, then merge column-by-column.
    if !plan.sub_blocks.is_empty() {
        let mut sub_outputs: Vec<(usize, usize, Vec<Vec<Value>>)> = Vec::new();
        for sub in &plan.sub_blocks {
            let sub_plan = SheetPlan {
                name: plan.name.clone(),
                rows: sub.rows.clone(),
                sub_blocks: Vec::new(),
                n_cols: sub.col_last - sub.col_first + 1,
            };
            let sub_rendered = render_sheet(
                &sub_plan,
                source,
                inputs_value,
                lists_value,
                named_sources,
            )?;
            sub_outputs.push((sub.col_first, sub.col_last, sub_rendered.rows));
        }
        let max_rows = sub_outputs
            .iter()
            .map(|(_, _, r)| r.len())
            .max()
            .unwrap_or(0);
        let n_cols = plan.n_cols.max(1);
        let mut merged: Vec<Vec<Value>> = (0..max_rows)
            .map(|_| vec![Value::Empty; n_cols])
            .collect();
        for (col_first, _col_last, sub_rows) in sub_outputs {
            for (r_idx, sub_row) in sub_rows.iter().enumerate() {
                for (c_off, v) in sub_row.iter().enumerate() {
                    let c = col_first + c_off;
                    if c < n_cols {
                        merged[r_idx][c] = v.clone();
                    }
                }
            }
        }
        return Ok(RenderedSheet {
            name: plan.name.clone(),
            rows: merged,
        });
    }

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
                side_rows,
                col_range,
            } => {
                let _ = col_range;
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
                let active_source: Option<String> = directives.iter().find_map(|d| match d {
                    Directive::Source(n) => Some(n.clone()),
                    _ => None,
                });
                let rows_handle: Arc<Vec<HashMap<String, Value>>> = Arc::new(effective.clone());

                let mut global_idx = 0usize;
                let emit_expansion =
                    |group_rows: &Vec<HashMap<String, Value>>,
                     rows: &mut Vec<Vec<Value>>,
                     global_idx: &mut usize|
                     -> Result<()> {
                        for (iter_idx, source_row) in group_rows.iter().enumerate() {
                            *global_idx += 1;
                            let mut ctx: EvalContext = source_row.clone();
                            inject_rows(&mut ctx, Arc::clone(&rows_handle));
                            inject_rownum(&mut ctx, *global_idx);
                            ctx.insert("__inputs__".to_string(), inputs_value.clone());
                            ctx.insert("__lists__".to_string(), lists_value.clone());
                            // ADR-0012: when `@source Name` is active,
                            // `Name[Col]` should resolve to the current
                            // row's column (active source = default).
                            if let Some(name) = &active_source {
                                ctx.insert(
                                    name.clone(),
                                    Value::Map(Arc::new(source_row.clone())),
                                );
                            }
                            inject_named_sources(&mut ctx, named_sources);
                            // ADR-0066 column-scoped splice: side cells
                            // outside the expansion's col_range come
                            // from the *original* row at this iter index
                            // (iter 0 → expansion row itself, iter N+1
                            // → side_rows[N], or Empty if exhausted).
                            let effective_cells = compose_iteration_cells(
                                cells, side_rows, *col_range, iter_idx,
                            );
                            rows.push(render_template_row(&effective_cells, &ctx)?);
                        }
                        Ok(())
                    };

                if group_fields.is_empty() {
                    // No grouping: emit every row, then any trailing
                    // subtotal rows once over the whole block.
                    emit_expansion(&effective, &mut rows, &mut global_idx)?;
                    // ADR-0066: side_rows that the expansion didn't
                    // consume (one per source row beyond the first) are
                    // post-block outside-only rows. Their template-row
                    // position keeps them at their original spot, so we
                    // emit them right after the expansion before any
                    // subtotal / inside-footer rows.
                    let consumed = effective.len().saturating_sub(1);
                    if side_rows.len() > consumed {
                        for extra in &side_rows[consumed..] {
                            rows.push(render_static_row(
                                extra,
                                inputs_value,
                                lists_value,
                                named_sources,
                            )?);
                        }
                    }
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
                        side_rows,
                        *col_range,
                        &mut rows,
                        &mut global_idx,
                        &rows_handle,
                        inputs_value,
                        lists_value,
                        named_sources,
                        active_source.as_deref(),
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
    side_rows: &[Vec<CellSource>],
    col_range: Option<(usize, usize)>,
    out_rows: &mut Vec<Vec<Value>>,
    global_idx: &mut usize,
    rows_handle: &Arc<Vec<HashMap<String, Value>>>,
    inputs_value: &Value,
    lists_value: &Value,
    named_sources: &HashMap<String, Value>,
    active_source: Option<&str>,
) -> Result<()> {
    if depth == group_fields.len() {
        // Leaf — emit every row through the expansion template.
        for (iter_idx, source_row) in rows.iter().enumerate() {
            *global_idx += 1;
            let mut ctx: EvalContext = source_row.clone();
            inject_rows(&mut ctx, Arc::clone(rows_handle));
            inject_rownum(&mut ctx, *global_idx);
            ctx.insert("__inputs__".to_string(), inputs_value.clone());
            ctx.insert("__lists__".to_string(), lists_value.clone());
            if let Some(name) = active_source {
                ctx.insert(name.to_string(), Value::Map(Arc::new(source_row.clone())));
            }
            inject_named_sources(&mut ctx, named_sources);
            let effective_cells =
                compose_iteration_cells(cells, side_rows, col_range, iter_idx);
            out_rows.push(render_template_row(&effective_cells, &ctx)?);
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
            side_rows,
            col_range,
            out_rows,
            global_idx,
            rows_handle,
            inputs_value,
            lists_value,
            named_sources,
            active_source,
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
            CellSource::Template { text, num_fmt } => {
                coerce_for_num_fmt(eval_cell(text, &ctx)?, *num_fmt)
            }
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

/// Build the cell list for one expansion iteration. Inside the
/// `col_range` we use the original expansion row's cells (which the
/// evaluator will substitute against the current source row). Outside
/// the range:
/// - iteration 0 → the expansion row's own outside-range cells
///   (literals, side templates) live in this row
/// - iteration N > 0 → look up `side_rows[N-1]` for the same column;
///   absent slots emit Empty (ADR-0066 column-scoped splice).
fn compose_iteration_cells(
    cells: &[CellSource],
    side_rows: &[Vec<CellSource>],
    col_range: Option<(usize, usize)>,
    iter_idx: usize,
) -> Vec<CellSource> {
    let Some((lo, hi)) = col_range else {
        return cells.to_vec();
    };
    cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let inside = i >= lo && i <= hi;
            if inside || iter_idx == 0 {
                cell.clone()
            } else {
                side_rows
                    .get(iter_idx - 1)
                    .and_then(|r| r.get(i))
                    .cloned()
                    .unwrap_or(CellSource::Empty)
            }
        })
        .collect()
}

/// Split a row's cells into outside-only and inside-only copies for
/// ADR-0066's column-scoped splice. Empty cells stay empty in both
/// halves. Returns `(outside, inside, has_outside_content, has_inside_content)`.
fn split_inside_outside(
    cells: &[CellSource],
    range: (usize, usize),
) -> (Vec<CellSource>, Vec<CellSource>, bool, bool) {
    let (lo, hi) = range;
    let mut outside = vec![CellSource::Empty; cells.len()];
    let mut inside = vec![CellSource::Empty; cells.len()];
    let mut has_outside = false;
    let mut has_inside = false;
    for (i, c) in cells.iter().enumerate() {
        if matches!(c, CellSource::Empty) {
            continue;
        }
        if i >= lo && i <= hi {
            inside[i] = c.clone();
            has_inside = true;
        } else {
            outside[i] = c.clone();
            has_outside = true;
        }
    }
    (outside, inside, has_outside, has_inside)
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
    // xl3 ADR-0016 multi-sort priority: directives appear in priority
    // order (first = primary, last = least). We stably sort by the
    // *least* priority field first and let later (= higher priority)
    // sorts preserve the existing order for equal keys.
    let mut ordered: Vec<&Directive> = directives.iter().collect();
    {
        let sort_positions: Vec<usize> = ordered
            .iter()
            .enumerate()
            .filter(|(_, d)| matches!(d, Directive::Sort { .. }))
            .map(|(i, _)| i)
            .collect();
        if sort_positions.len() > 1 {
            // Reverse the order of Sort directives in `ordered`, keeping
            // every other directive at its original position.
            let mut reversed = sort_positions.clone();
            reversed.reverse();
            let originals: Vec<&Directive> =
                sort_positions.iter().map(|&i| ordered[i]).collect();
            for (slot, src) in reversed.iter().zip(originals.iter()) {
                ordered[*slot] = *src;
            }
        }
    }
    for d in ordered {
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
            | Directive::Block { .. }
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
            CellSource::Template { text, num_fmt } => {
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
                    out.push(coerce_for_num_fmt(eval_cell(text, &ctx)?, *num_fmt));
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
            CellSource::Template { text, num_fmt } => {
                coerce_for_num_fmt(eval_cell(text, &ctx)?, *num_fmt)
            }
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
            CellSource::Template { text, num_fmt } => {
                out.push(coerce_for_num_fmt(eval_cell(text, ctx)?, *num_fmt))
            }
            CellSource::Subtotal { .. } => out.push(Value::Empty),
        }
    }
    Ok(out)
}

/// Apply ADR-0003 single-expression cell coercion driven by the
/// template cell's numFmt classification:
/// - numeric format + string value → parse to Number (fallback: keep
///   the string)
/// - date format + ISO-style date string → Excel serial Number
/// - text format (`@`) + Number → canonical string
fn coerce_for_num_fmt(value: Value, kind: NumFmtKind) -> Value {
    match kind {
        NumFmtKind::Numeric => match value {
            Value::String(s) => {
                let trimmed = s.trim();
                let cleaned: String = trimmed.chars().filter(|c| *c != ',').collect();
                match cleaned.parse::<f64>() {
                    Ok(n) => Value::Number(n),
                    Err(_) => Value::String(s),
                }
            }
            other => other,
        },
        NumFmtKind::Date => match value {
            Value::String(ref s) => {
                if let Some(serial) = parse_iso_date_to_serial(s.trim()) {
                    Value::Number(serial)
                } else {
                    value
                }
            }
            other => other,
        },
        NumFmtKind::Text => match value {
            Value::Number(n) => Value::String(canonical_number(n)),
            other => other,
        },
        NumFmtKind::General => value,
    }
}

fn canonical_number(n: f64) -> String {
    crate::value::canonical_number(n)
}

fn parse_iso_date_to_serial(s: &str) -> Option<f64> {
    // Accept `YYYY-MM-DD` (the only date-string form xl3 emits as input).
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i32 = std::str::from_utf8(&bytes[..4]).ok()?.parse().ok()?;
    let month: u32 = std::str::from_utf8(&bytes[5..7]).ok()?.parse().ok()?;
    let day: u32 = std::str::from_utf8(&bytes[8..10]).ok()?.parse().ok()?;
    // Roundtrip through functions::serial_to_iso_date by constructing
    // the date directly. We use the DATE() builtin's serial path —
    // exposed via a small helper here to avoid a circular dep.
    excel_date_to_serial(year, month, day)
}

fn excel_date_to_serial(year: i32, month: u32, day: u32) -> Option<f64> {
    // Minimal Gregorian → Excel serial (matches functions.rs internal
    // helper, but inlined to avoid cross-module plumbing).
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month as i32, day as i32);
    // Excel epoch: 1899-12-30 = day 0. Add the 1900-02-29 leap-day
    // adjustment (`+1` for dates on or after 1900-03-01).
    let epoch = days_from_civil(1899, 12, 30);
    let mut serial = days - epoch;
    let leap_threshold = days_from_civil(1900, 3, 1);
    if days >= leap_threshold {
        // already correct
    } else if days >= days_from_civil(1900, 1, 1) {
        serial -= 1;
    }
    Some(serial as f64)
}

/// Days since the proleptic Gregorian epoch (March-1, 2000-style civil
/// algorithm — Howard Hinnant). Returns a signed count; the caller
/// offsets by the Excel epoch.
fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = ((153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146097 + doe - 719468
}
