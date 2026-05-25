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
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::calamine::{open_workbook, Data as CData, Reader, Xlsx};
use crate::directives::{parse_directive_cell, Direction, Directive};
use crate::styles::{self, NumFmtKind, TemplateStyles};
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
    /// Parse the `source_table` config value (xl3 evaluation.md
    /// "Source Data Model"). Returns the default — first row as
    /// header, data continues to the end of the sheet — when the
    /// value is missing or unrecognised.
    pub fn source_table(&self) -> SourceTable {
        self.get("source_table")
            .map(parse_source_table)
            .unwrap_or(SourceTable::HeaderRow(1))
    }
}

/// How the source sheet is interpreted.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceTable {
    /// `source_table: 1` (default) or `source_table: 3` — the named
    /// row (1-based) is the header. Data rows continue until the end
    /// of the sheet (modulo blank-row handling per ADR-0007).
    HeaderRow(usize),
    /// `source_table: B3:D4` (closed) or `B3:D` / `B3` (open-ended) —
    /// the first row is the header, columns are constrained, and the
    /// bottom row is either explicit (`last_row = Some(...)`) or
    /// "until the end of the used range" (`None`). Likewise
    /// `last_col` may be `None` to mean "until the rightmost used
    /// column".
    Range {
        first_row: usize,         // 1-based
        last_row: Option<usize>,  // 1-based, inclusive; None = open-ended
        first_col: usize,         // 1-based
        last_col: Option<usize>,  // 1-based, inclusive; None = open-ended
    },
}

fn parse_source_table(raw: &str) -> SourceTable {
    let s = raw.trim();
    if let Ok(n) = s.parse::<usize>() {
        return SourceTable::HeaderRow(n.max(1));
    }
    if let Some((a, b)) = s.split_once(':') {
        let lhs = parse_a1_part(a.trim());
        let rhs = parse_a1_part(b.trim());
        if let (Some((Some(r1), Some(c1))), Some((r2, c2))) = (lhs, rhs) {
            let (first_row, last_row) = match r2 {
                Some(r2) => (r1.min(r2), Some(r1.max(r2))),
                None => (r1, None),
            };
            let (first_col, last_col) = match c2 {
                Some(c2) => (c1.min(c2), Some(c1.max(c2))),
                None => (c1, None),
            };
            return SourceTable::Range {
                first_row,
                last_row,
                first_col,
                last_col,
            };
        }
    }
    SourceTable::HeaderRow(1)
}

/// Parse one half of an A1 range — accepts `B3` (cell), `B` (column
/// only) or `3` (row only). Returns `(row?, col?)` with 1-based
/// indices and `None` for whichever component wasn't present.
fn parse_a1_part(s: &str) -> Option<(Option<usize>, Option<usize>)> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut col = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        let c = bytes[i].to_ascii_uppercase();
        col = col * 26 + (c - b'A' + 1) as usize;
        i += 1;
    }
    let col_opt = if col == 0 { None } else { Some(col) };
    let row_opt = if i == bytes.len() {
        None
    } else {
        let row: usize = std::str::from_utf8(&bytes[i..]).ok()?.parse().ok()?;
        if row == 0 {
            None
        } else {
            Some(row)
        }
    };
    if col_opt.is_none() && row_opt.is_none() {
        return None;
    }
    Some((row_opt, col_opt))
}

#[derive(Debug, Clone)]
pub enum CellSource {
    Empty,
    Literal(Value),
    /// Contains at least one `{{ ... }}` expression block. `num_fmt`
    /// is the classified numFmt of the underlying *template* cell,
    /// used by ADR-0003 single-expression coercion at render time.
    /// `format_code` is the raw numFmt string from the template (e.g.
    /// `"0.00"` or `"yyyy-mm-dd"`) — preserved so the output writer
    /// can emit the same display format on the rendered cell.
    Template {
        text: String,
        num_fmt: NumFmtKind,
        format_code: Option<String>,
    },
    /// `{{ @subtotal <FN>(<ColumnRef>) }}` — emitted at the end of
    /// each group when the enclosing block has a `@group` directive.
    /// `aggregate` is normalised to uppercase; `field` is the bare
    /// column name (Phase-1 scope: no `Source[Field]` form).
    Subtotal {
        aggregate: String,
        field: String,
    },
}

impl CellSource {
    pub fn is_template(&self) -> bool {
        matches!(self, CellSource::Template { .. })
    }
}

/// Try to recognise a cell whose text is a single
/// `{{ @subtotal <FN>(<ColumnRef>) }}` expression. Returns
/// `(aggregate, field)` when the shape matches.
fn parse_subtotal_cell(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?;
    let body = inner.trim().strip_prefix("@subtotal")?.trim();
    let paren_open = body.find('(')?;
    let fn_name = body[..paren_open].trim();
    let after = &body[paren_open + 1..];
    let paren_close = after.rfind(')')?;
    let arg = after[..paren_close].trim();
    // Phase-1: only the bare `[Field]` form. `Source[Field]` is a
    // future extension.
    let field = arg
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some((fn_name.to_ascii_uppercase(), field.to_string()))
}

#[derive(Debug, Clone)]
pub enum RowPlan {
    Static(Vec<CellSource>),
    ExpandDown {
        cells: Vec<CellSource>,
        directives: Vec<Directive>,
        /// Rows that follow the expansion row and contribute their
        /// subtotal cells once per group (when `@group` is active).
        /// Always empty when no `@group` directive is in scope.
        subtotal_rows: Vec<Vec<CellSource>>,
        /// Rows that follow the expansion row and contribute *side*
        /// (outside-col-range) cells per ADR-0066. Each side row maps
        /// onto the corresponding subsequent source-row position.
        /// Always empty when no side cells were absorbed.
        side_rows: Vec<Vec<CellSource>>,
        /// Inclusive (first, last) column range that the expansion
        /// templates occupy. Cells outside this range are "side"
        /// cells that follow ADR-0066 column-scoped splice semantics.
        /// `None` when there are no template cells (degenerate row).
        col_range: Option<(usize, usize)>,
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
    /// Multi-block (`@block A:B` / `@block D:E`). When non-empty, the
    /// renderer ignores `rows` and renders each `SubBlock` separately,
    /// then merges by column range. ADR-0068 / 0069.
    pub sub_blocks: Vec<SubBlock>,
    /// Total column width of the sheet (used to size the merged
    /// row-major buffer when sub_blocks are in play).
    pub n_cols: usize,
}

/// One column-bounded block in a multi-block sheet. `col_first` /
/// `col_last` are 0-based inclusive sheet columns. `rows` is the
/// block's own RowPlan sequence — cells are indexed 0..=(col_last - col_first).
#[derive(Debug, Clone)]
pub struct SubBlock {
    pub col_first: usize,
    pub col_last: usize,
    pub rows: Vec<RowPlan>,
}

#[derive(Debug, Clone)]
pub struct WorkbookPlan {
    pub config: ConfigMeta,
    pub sheets: Vec<SheetPlan>,
    /// Per-input default value from the `__inputs__` sheet, keyed by
    /// input name. Host inputs (if any) override these at render time.
    pub inputs: HashMap<String, Value>,
    /// Named value lists from the `__lists__` sheet. Each column is a
    /// list — header is the list name, cells below are the values.
    /// Used by `@filter [Field] in __lists__[Name]`.
    pub lists: HashMap<String, Vec<Value>>,
    /// Named external data sources declared on `__sources__` (xl3
    /// ADR-0012). Each entry says where the source lives in the data
    /// workbook and how to interpret its layout.
    pub named_sources: HashMap<String, SourceDecl>,
}

/// One row from the `__sources__` sheet — names a secondary source
/// reachable via `SourceName[Column]` expressions.
#[derive(Debug, Clone)]
pub struct SourceDecl {
    pub sheet: String,
    pub table: SourceTable,
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

/// True iff the template text references a *source-row* field — i.e.
/// a bare `[Column]` or `Source[Column]` reference that varies per
/// source row. Reserved-namespace refs (`__inputs__[key]`,
/// `__config__[key]`, `__lists__[key]`, `__sources__[key]`) do NOT
/// count, because they're constants for the whole render.
///
/// This is the signal the planner uses to decide whether a row is an
/// expansion row or a static-but-templated row. A row that only
/// references reserved namespaces (e.g. `Report month: {{ __inputs__[month] }}`)
/// is evaluated once, not once per source row.
/// Inclusive `(first, last)` column range of the cells flagged as
/// `Template { .. }` or `Subtotal { .. }`. The expansion engine
/// rewrites only these columns when iterating source rows; columns
/// outside the range follow the column-scoped splice rule in
/// ADR-0066. Returns `None` if no template-bearing cells were found.
fn compute_template_col_range(cells: &[CellSource]) -> Option<(usize, usize)> {
    let mut first: Option<usize> = None;
    let mut last: usize = 0;
    for (i, c) in cells.iter().enumerate() {
        let is_template = matches!(c, CellSource::Template { .. } | CellSource::Subtotal { .. });
        if is_template {
            first.get_or_insert(i);
            last = i;
        }
    }
    first.map(|f| (f, last))
}

/// True iff every non-Empty cell in `cells` sits *outside* the given
/// column range. Used to detect ADR-0066 "side rows" that the planner
/// should absorb into the preceding ExpandDown.
fn cells_only_outside_range(cells: &[CellSource], range: (usize, usize)) -> bool {
    let (lo, hi) = range;
    let mut any_outside = false;
    for (i, c) in cells.iter().enumerate() {
        let inside = i >= lo && i <= hi;
        match c {
            CellSource::Empty => continue,
            _ if inside => return false,
            _ => any_outside = true,
        }
    }
    any_outside
}

fn cells_isolate_outside(cells: &[CellSource], range: (usize, usize)) -> Vec<CellSource> {
    let (lo, hi) = range;
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i >= lo && i <= hi {
                CellSource::Empty
            } else {
                c.clone()
            }
        })
        .collect()
}

fn cells_isolate_inside(cells: &[CellSource], range: (usize, usize)) -> Vec<CellSource> {
    let (lo, hi) = range;
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i >= lo && i <= hi {
                c.clone()
            } else {
                CellSource::Empty
            }
        })
        .collect()
}

fn template_depends_on_source_row(s: &str, named_sources_to_exclude: &[&str]) -> bool {
    let mut cleaned = s.to_string();
    for ns in [
        "__config__[",
        "__inputs__[",
        "__lists__[",
        "__sources__[",
    ] {
        cleaned = cleaned.replace(ns, "");
    }
    // Named-source references that don't belong to the *active* source
    // are row-set refs (XLOOKUP / aggregate input), not per-row.
    // Active-source refs (`<active>[Col]`) ARE per-row when `@source`
    // is in scope — the caller passes in the non-active named-source
    // names so they get stripped here.
    for name in named_sources_to_exclude {
        let prefix = format!("{name}[");
        cleaned = cleaned.replace(&prefix, "");
    }
    cleaned.contains('[')
}

pub fn parse_template(path: &Path) -> Result<WorkbookPlan> {
    let styles = styles::parse_template_styles(path).unwrap_or_default();
    let mut wb: Xlsx<_> = open_workbook(path)
        .with_context(|| format!("open template workbook at {}", path.display()))?;

    // First pass: collect named-source names so the row classifier can
    // recognise `<Source>[Column]` as a row-set reference (not a per-
    // source-row reference).
    let named_source_names: Vec<String> = if sheet_names_set(&wb).contains("__sources__") {
        if let Ok(range) = wb.worksheet_range("__sources__") {
            let (rows, cols) = range.get_size();
            if rows >= 2 && cols >= 1 {
                (1..rows)
                    .filter_map(|r| match range.get((r, 0)) {
                        Some(CData::String(s)) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

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

        // ADR-0068/0069 multi-block detection. A `@block A:B` directive
        // anywhere on the sheet declares a column-bounded sub-block.
        // We pre-scan once so we know whether to drive the
        // single-block (current) or multi-block path for this sheet.
        let mut block_ranges: Vec<(usize, usize)> = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                if let Some(CData::String(s)) = range.get((r, c)) {
                    if let Some(dirs) = parse_directive_cell(s) {
                        for d in dirs {
                            if let Directive::Block { col_first, col_last } = d {
                                block_ranges.push((col_first, col_last));
                            }
                        }
                    }
                }
            }
        }
        block_ranges.sort();
        block_ranges.dedup();

        if !block_ranges.is_empty() {
            let mut sub_blocks_out: Vec<SubBlock> = Vec::new();
            for (col_first, col_last) in &block_ranges {
                let sub_rows = build_row_plans_for_range(
                    &range,
                    &name,
                    rows,
                    *col_first,
                    *col_last,
                    &styles,
                    &named_source_names,
                )?;
                sub_blocks_out.push(SubBlock {
                    col_first: *col_first,
                    col_last: *col_last,
                    rows: sub_rows,
                });
            }
            sheets.push(SheetPlan {
                name: name.clone(),
                rows: Vec::new(),
                sub_blocks: sub_blocks_out,
                n_cols: cols,
            });
            continue;
        }

        let row_plans = build_row_plans_for_range(
            &range,
            &name,
            rows,
            0,
            cols.saturating_sub(1),
            &styles,
            &named_source_names,
        )?;
        sheets.push(SheetPlan {
            name,
            rows: row_plans,
            sub_blocks: Vec::new(),
            n_cols: cols,
        });
    }


    // Sanity check that we picked up the bits we need.
    if sheets.is_empty() {
        bail!("template has no visible (non-reserved) sheets");
    }

    // Parse `__inputs__` defaults if present. xl3's spec gives the
    // sheet `name | type | default | label | description | options ...`
    // columns; we only need name → default for now.
    let mut inputs = HashMap::new();
    if sheet_names_set(&wb).contains("__inputs__") {
        if let Ok(range) = wb.worksheet_range("__inputs__") {
            let (rows, cols) = range.get_size();
            if rows >= 2 && cols >= 1 {
                let mut headers: Vec<String> = Vec::new();
                for c in 0..cols {
                    headers.push(match range.get((0, c)) {
                        Some(CData::String(s)) => s.clone(),
                        _ => String::new(),
                    });
                }
                let name_col = 0usize; // xl3: first column is always the input name
                let default_col = headers
                    .iter()
                    .position(|h| h.eq_ignore_ascii_case("default"));
                if let Some(default_col) = default_col {
                    // ADR-0050: __inputs__ default values may themselves
                    // be XTL templates that reference __config__ and
                    // pure scalar functions. Evaluate them with a
                    // ctx-of-config so the resulting plan holds the
                    // already-rendered defaults.
                    let mut input_ctx: HashMap<String, Value> = HashMap::new();
                    let config_map: HashMap<String, Value> = config
                        .values
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect();
                    input_ctx
                        .insert("__config__".to_string(), Value::Map(Arc::new(config_map)));
                    for r in 1..rows {
                        let name = match range.get((r, name_col)) {
                            Some(CData::String(s)) if !s.is_empty() => s.clone(),
                            _ => continue,
                        };
                        let raw = range
                            .get((r, default_col))
                            .map(Value::from_calamine)
                            .unwrap_or(Value::Empty);
                        let evaluated = if let Value::String(s) = &raw {
                            if s.contains("{{") {
                                crate::eval::eval_cell(s, &input_ctx).unwrap_or(raw.clone())
                            } else {
                                raw
                            }
                        } else {
                            raw
                        };
                        inputs.insert(name, evaluated);
                    }
                }
            }
        }
    }

    // Parse `__lists__` if present. Each column is a list (header is
    // its name, values are the cells below until the first blank).
    let mut lists: HashMap<String, Vec<Value>> = HashMap::new();
    if sheet_names_set(&wb).contains("__lists__") {
        if let Ok(range) = wb.worksheet_range("__lists__") {
            let (rows, cols) = range.get_size();
            for c in 0..cols {
                let header = match range.get((0, c)) {
                    Some(CData::String(s)) if !s.is_empty() => s.clone(),
                    _ => continue,
                };
                let mut values = Vec::new();
                for r in 1..rows {
                    match range.get((r, c)) {
                        Some(CData::Empty) | None => break,
                        Some(other) => values.push(Value::from_calamine(other)),
                    }
                }
                lists.insert(header, values);
            }
        }
    }

    // Parse `__sources__` if present. xl3's column convention is
    // `name | sheet | table | description | …`. We only need name →
    // (sheet, table). Other columns (description, etc.) are ignored
    // for now.
    let mut named_sources: HashMap<String, SourceDecl> = HashMap::new();
    if sheet_names_set(&wb).contains("__sources__") {
        if let Ok(range) = wb.worksheet_range("__sources__") {
            let (rows, cols) = range.get_size();
            if rows >= 2 && cols >= 1 {
                let mut headers: Vec<String> = Vec::with_capacity(cols);
                for c in 0..cols {
                    headers.push(match range.get((0, c)) {
                        Some(CData::String(s)) => s.clone(),
                        _ => String::new(),
                    });
                }
                let name_col = 0usize;
                let sheet_col = headers
                    .iter()
                    .position(|h| h.eq_ignore_ascii_case("sheet"));
                let table_col = headers
                    .iter()
                    .position(|h| h.eq_ignore_ascii_case("table"));
                for r in 1..rows {
                    let name = match range.get((r, name_col)) {
                        Some(CData::String(s)) if !s.is_empty() => s.clone(),
                        _ => continue,
                    };
                    let sheet = sheet_col
                        .and_then(|c| range.get((r, c)))
                        .and_then(|d| match d {
                            CData::String(s) if !s.is_empty() => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| name.clone());
                    let table_raw = table_col
                        .and_then(|c| range.get((r, c)))
                        .map(|d| match d {
                            CData::String(s) => s.clone(),
                            CData::Float(f) => format!("{f}"),
                            CData::Int(i) => format!("{i}"),
                            _ => String::new(),
                        })
                        .unwrap_or_default();
                    let table = if table_raw.is_empty() {
                        SourceTable::HeaderRow(1)
                    } else {
                        parse_source_table(&table_raw)
                    };
                    named_sources.insert(name, SourceDecl { sheet, table });
                }
            }
        }
    }

    Ok(WorkbookPlan {
        config,
        sheets,
        inputs,
        lists,
        named_sources,
    })
}

fn sheet_names_set<R: std::io::Read + std::io::Seek>(
    wb: &Xlsx<R>,
) -> std::collections::HashSet<String> {
    wb.sheet_names().into_iter().collect()
}

/// Build the RowPlan sequence for the given column range of a sheet —
/// re-used by both the single-block path and each sub-block of a
/// multi-block sheet (ADR-0068/0069). Cells *outside* the range are
/// not visited at all.
fn build_row_plans_for_range(
    range: &calamine::Range<CData>,
    sheet_name: &str,
    rows: usize,
    col_first: usize,
    col_last: usize,
    styles: &TemplateStyles,
    named_source_names: &[String],
) -> Result<Vec<RowPlan>> {
    let mut row_plans: Vec<RowPlan> = Vec::with_capacity(rows);
    let mut pending_direction = Direction::Down;
    let mut pending_directives: Vec<Directive> = Vec::new();
    let cols_in_range = col_last.saturating_sub(col_first) + 1;
    for r in 0..rows {
        let mut row_cells = Vec::with_capacity(cols_in_range);
        let mut has_source_template = false;
        let mut has_subtotal = false;
        let mut directive_only = true;
        let mut any_cell = false;
        let active_source: Option<&str> = pending_directives.iter().find_map(|d| match d {
            Directive::Source(n) => Some(n.as_str()),
            _ => None,
        });
        let exclude_named: Vec<&str> = named_source_names
            .iter()
            .filter(|n| Some(n.as_str()) != active_source)
            .map(|s| s.as_str())
            .collect();
        for c_off in 0..cols_in_range {
            let c = col_first + c_off;
            let cell = match range.get((r, c)) {
                None | Some(CData::Empty) => CellSource::Empty,
                Some(CData::String(s)) if cell_is_template_text(s) => {
                    any_cell = true;
                    if let Some((aggregate, field)) = parse_subtotal_cell(s) {
                        directive_only = false;
                        has_subtotal = true;
                        CellSource::Subtotal { aggregate, field }
                    } else if parse_directive_cell(s).is_some() {
                        CellSource::Empty
                    } else {
                        directive_only = false;
                        if template_depends_on_source_row(s, &exclude_named) {
                            has_source_template = true;
                        }
                        let format_code = styles.format_code(sheet_name, r as u32, c as u32);
                        let num_fmt = format_code
                            .as_deref()
                            .map(styles::classify_num_fmt)
                            .unwrap_or(NumFmtKind::General);
                        CellSource::Template {
                            text: s.clone(),
                            num_fmt,
                            format_code,
                        }
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

        if any_cell && directive_only {
            for c_off in 0..cols_in_range {
                let c = col_first + c_off;
                if let Some(CData::String(s)) = range.get((r, c)) {
                    if let Some(directives) = parse_directive_cell(s) {
                        for d in directives {
                            match d {
                                Directive::Repeat(dir) => pending_direction = dir,
                                Directive::Block { .. } => {} // already consumed by outer scan
                                other => pending_directives.push(other),
                            }
                        }
                    }
                }
            }
            continue;
        }

        if !has_source_template && !has_subtotal {
            if let Some(RowPlan::ExpandDown {
                col_range: Some(range_inner),
                side_rows,
                ..
            }) = row_plans.last_mut()
            {
                let range_inner = *range_inner;
                if !any_cell {
                    side_rows.push(row_cells);
                    continue;
                }
                if cells_only_outside_range(&row_cells, range_inner) {
                    side_rows.push(row_cells);
                    continue;
                }
                let outside = cells_isolate_outside(&row_cells, range_inner);
                let inside = cells_isolate_inside(&row_cells, range_inner);
                let has_inside = inside.iter().any(|c| !matches!(c, CellSource::Empty));
                let has_outside = outside.iter().any(|c| !matches!(c, CellSource::Empty));
                if has_inside && has_outside {
                    side_rows.push(outside);
                    row_plans.push(RowPlan::Static(inside));
                    continue;
                }
            }
            if !any_cell {
                continue;
            }
        }

        if has_subtotal && !has_source_template {
            if let Some(RowPlan::ExpandDown { subtotal_rows, .. }) = row_plans.last_mut() {
                subtotal_rows.push(row_cells);
                continue;
            }
        }

        let row_plan = if has_source_template {
            let directives = std::mem::take(&mut pending_directives);
            let col_range = compute_template_col_range(&row_cells);
            let plan = match pending_direction {
                Direction::Down => RowPlan::ExpandDown {
                    cells: row_cells,
                    directives,
                    subtotal_rows: Vec::new(),
                    side_rows: Vec::new(),
                    col_range,
                },
                Direction::Right => RowPlan::ExpandRight {
                    cells: row_cells,
                    directives,
                },
            };
            pending_direction = Direction::Down;
            plan
        } else {
            if let Some(RowPlan::ExpandDown {
                col_range: Some(range_inner),
                side_rows,
                ..
            }) = row_plans.last_mut()
            {
                if cells_only_outside_range(&row_cells, *range_inner) {
                    side_rows.push(row_cells);
                    continue;
                }
            }
            RowPlan::Static(row_cells)
        };
        row_plans.push(row_plan);
    }
    Ok(row_plans)
}

/// Wrap `inputs` as a single `Value::Map` so the evaluator can resolve
/// `__inputs__[key]` via the reserved-ref path without thinking about
/// where the value came from. Host overrides should be merged into the
/// `inputs` map *before* this call.
pub fn inputs_to_value(inputs: &HashMap<String, Value>) -> Value {
    Value::Map(Arc::new(inputs.clone()))
}

/// Wrap `lists` as a `Value::Map` whose values are `Value::List` —
/// matches the `__lists__[Name]` lookup shape (namespace is a map of
/// name → list, and `<ns>[key]` resolves to a list).
pub fn lists_to_value(lists: &HashMap<String, Vec<Value>>) -> Value {
    let inner: HashMap<String, Value> = lists
        .iter()
        .map(|(k, v)| (k.clone(), Value::List(Arc::new(v.clone()))))
        .collect();
    Value::Map(Arc::new(inner))
}
