//! Surface-area parity with xl3 (TS) / xl3-py introspection APIs.
//!
//! - `read_template_inputs(template) -> Vec<InputSpec>` — what the
//!   template declares on `__inputs__` (xl3 ADR-0010).
//! - `preview(template, data) -> PreviewResult` — the *shape* of what
//!   `render` would produce: filenames + sheet names + source columns.
//!   Implemented in terms of `parse_template` + the source reader; no
//!   actual workbook bytes are produced.
//!
//! Both functions are pure introspection — they never touch
//! `rust_xlsxwriter` or run the evaluator. Aimed at the same use-case
//! as the TS/py siblings: hosts that need to render input forms or
//! describe the output before committing to a full convert call.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};

use crate::calamine::{open_workbook, Data as CData, Reader, Xlsx};
use crate::plan::{parse_template, parse_template_bytes};
use crate::source::CalamineSourceReader;

/// Mirrors xl3 (TS)'s `InputSpec` and xl3-py's `InputSpec`. The shape
/// is intentionally identical so a host can feed the result through a
/// language-agnostic UI generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSpec {
    pub name: String,
    pub kind: InputKind,
    pub required: bool,
    pub default: Option<String>,
    pub label: Option<String>,
    pub description: Option<String>,
    /// `select`-only — the pipe-separated `options` column unpacked
    /// into a list. Empty for other input kinds.
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Text,
    Number,
    Date,
    Select,
    /// Unknown / unspecified — accept any string. Lets us not reject
    /// templates that omit the `type` column or use a fixture-only
    /// extension.
    Other,
}

impl InputKind {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" => InputKind::Text,
            "number" => InputKind::Number,
            "date" => InputKind::Date,
            "select" => InputKind::Select,
            _ => InputKind::Other,
        }
    }
}

/// One file the renderer would produce. Currently always exactly one
/// (`output_file_pattern` template splitting is pending). Mirrors the
/// PreviewFile shape from the sibling implementations.
#[derive(Debug, Clone)]
pub struct PreviewFile {
    pub filename: String,
    pub sheets: Vec<PreviewSheet>,
}

#[derive(Debug, Clone)]
pub struct PreviewSheet {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct PreviewResult {
    pub files: Vec<PreviewFile>,
    pub sources: Vec<PreviewSource>,
}

#[derive(Debug, Clone)]
pub struct PreviewSource {
    pub name: String,
    pub headers: Vec<String>,
    pub row_count: usize,
}

/// Read just the `__inputs__` sheet, returning structured specs.
/// Does not render or open the data workbook. Mirrors xl3 (TS)'s
/// `readTemplateInputs` and xl3-py's `read_template_inputs`.
pub fn read_template_inputs(template: &Path) -> Result<Vec<InputSpec>> {
    let wb: Xlsx<_> = open_workbook(template)
        .with_context(|| format!("open template workbook at {}", template.display()))?;
    read_template_inputs_inner(wb)
}

/// In-memory variant for hosts that already have the template bytes
/// (e.g. WASM `readTemplateInputs(buffer)`).
pub fn read_template_inputs_bytes(template_bytes: &[u8]) -> Result<Vec<InputSpec>> {
    let cursor = Cursor::new(template_bytes.to_vec());
    let wb: Xlsx<_> = Xlsx::new(cursor).context("open template workbook from bytes")?;
    read_template_inputs_inner(wb)
}

fn read_template_inputs_inner<R: std::io::Read + std::io::Seek>(
    mut wb: Xlsx<R>,
) -> Result<Vec<InputSpec>> {
    let names = wb.sheet_names();
    if !names.iter().any(|n| n == "__inputs__") {
        return Ok(Vec::new());
    }
    let range = wb
        .worksheet_range("__inputs__")
        .context("read __inputs__ sheet")?;
    let (rows, cols) = range.get_size();
    if rows < 2 || cols < 1 {
        return Ok(Vec::new());
    }
    let mut headers: Vec<String> = Vec::with_capacity(cols);
    for c in 0..cols {
        headers.push(match range.get((0, c)) {
            Some(CData::String(s)) => s.clone(),
            _ => String::new(),
        });
    }
    let col_of = |key: &str| -> Option<usize> {
        headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(key))
    };
    let type_col = col_of("type");
    let default_col = col_of("default");
    let label_col = col_of("label");
    let description_col = col_of("description");
    let options_col = col_of("options");
    let required_col = col_of("required");

    let cell_to_string = |r: usize, c: Option<usize>| -> Option<String> {
        let c = c?;
        match range.get((r, c))? {
            CData::String(s) if !s.is_empty() => Some(s.clone()),
            CData::Float(f) => Some(format!("{f}")),
            CData::Int(i) => Some(format!("{i}")),
            CData::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    };

    let mut out = Vec::new();
    for r in 1..rows {
        let name = match range.get((r, 0)) {
            Some(CData::String(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };
        let kind = type_col
            .and_then(|c| cell_to_string(r, Some(c)))
            .map(|s| InputKind::parse(&s))
            .unwrap_or(InputKind::Other);
        let default = default_col.and_then(|c| cell_to_string(r, Some(c)));
        let label = label_col.and_then(|c| cell_to_string(r, Some(c)));
        let description = description_col.and_then(|c| cell_to_string(r, Some(c)));
        let options_raw = options_col.and_then(|c| cell_to_string(r, Some(c)));
        let options: Vec<String> = options_raw
            .as_deref()
            .map(|s| {
                s.split('|')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let required = required_col
            .and_then(|c| cell_to_string(r, Some(c)))
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1"))
            .unwrap_or(false);

        out.push(InputSpec {
            name,
            kind,
            required,
            default,
            label,
            description,
            options,
        });
    }
    Ok(out)
}

/// Describe the rendered output without producing it. Reports the
/// output filename(s), each sheet name, and a header / row-count
/// summary of every source the template will consult.
pub fn preview(template: &Path, data: &Path) -> Result<PreviewResult> {
    let plan = parse_template(template).context("parse template")?;
    let source_reader = CalamineSourceReader::open(data).context("open source workbook")?;
    preview_inner(plan, source_reader)
}

/// In-memory variant of [`preview`] for hosts that already have the
/// template and data bytes (e.g. the WASM wrapper).
pub fn preview_bytes(template_bytes: &[u8], data_bytes: Vec<u8>) -> Result<PreviewResult> {
    let plan = parse_template_bytes(template_bytes).context("parse template")?;
    let source_reader =
        CalamineSourceReader::open_bytes(data_bytes).context("open source workbook")?;
    preview_inner(plan, source_reader)
}

fn preview_inner(
    plan: crate::plan::WorkbookPlan,
    mut source_reader: CalamineSourceReader,
) -> Result<PreviewResult> {
    let source_sheet = match plan.config.source_sheet() {
        Some(pattern) => source_reader
            .resolve_sheet_name(pattern)
            .ok_or_else(|| {
                anyhow::Error::from(crate::errors::XtlError::new(
                    crate::errors::code::SOURCE_SHEET_MISSING,
                    format!("Source sheet \"{pattern}\" was not found"),
                ))
            })?,
        None => source_reader
            .first_sheet()
            .ok_or_else(|| {
                anyhow::Error::from(crate::errors::XtlError::new(
                    crate::errors::code::SOURCE_SHEET_MISSING,
                    "Source workbook is empty",
                ))
            })?,
    };
    let source_table = plan.config.source_table();
    let default_source = {
        use crate::source::SourceReader;
        source_reader.read(&source_sheet, &source_table)?
    };

    let mut sources = vec![PreviewSource {
        name: default_source.name.clone(),
        headers: default_source.headers.clone(),
        row_count: default_source.rows.len(),
    }];
    for (name, decl) in &plan.named_sources {
        use crate::source::SourceReader;
        if let Ok(data) = source_reader.read(&decl.sheet, &decl.table) {
            sources.push(PreviewSource {
                name: name.clone(),
                headers: data.headers,
                row_count: data.rows.len(),
            });
        }
    }

    let filename = plan
        .config
        .output_file_pattern()
        .map(str::to_string)
        .unwrap_or_else(|| "output.xlsx".to_string());
    let sheets: Vec<PreviewSheet> = plan
        .sheets
        .iter()
        .map(|s| PreviewSheet {
            name: s.name.clone(),
        })
        .collect();
    Ok(PreviewResult {
        files: vec![PreviewFile { filename, sheets }],
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(format!(
            "/Users/wefun/workspaces/playground/xl3/conformance/fixtures/{name}"
        ))
    }

    #[test]
    fn read_inputs_from_065() {
        let dir = fixture("065-input-text-default-applied");
        if !dir.exists() {
            return; // skip when sibling repo isn't checked out
        }
        let specs = read_template_inputs(&dir.join("template.xlsx")).unwrap();
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.name, "month");
        assert_eq!(s.kind, InputKind::Text);
        assert_eq!(s.default.as_deref(), Some("2026-05"));
        assert_eq!(s.label.as_deref(), Some("Report month"));
    }

    #[test]
    fn read_inputs_select_from_068() {
        let dir = fixture("068-input-select-host-supplied");
        if !dir.exists() {
            return;
        }
        let specs = read_template_inputs(&dir.join("template.xlsx")).unwrap();
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.name, "region");
        assert_eq!(s.kind, InputKind::Select);
        assert_eq!(
            s.options,
            vec!["Seoul".to_string(), "Busan".to_string(), "Daegu".to_string()]
        );
    }

    #[test]
    fn preview_001_single_file_single_sheet() {
        let dir = fixture("001-bracket-substitution");
        if !dir.exists() {
            return;
        }
        let pv = preview(&dir.join("template.xlsx"), &dir.join("data.xlsx")).unwrap();
        assert_eq!(pv.files.len(), 1);
        assert_eq!(pv.files[0].filename, "output.xlsx");
        assert_eq!(pv.files[0].sheets.len(), 1);
        assert_eq!(pv.files[0].sheets[0].name, "Report");
        assert_eq!(pv.sources.len(), 1);
        assert_eq!(pv.sources[0].headers, vec!["Customer".to_string()]);
        assert_eq!(pv.sources[0].row_count, 2);
    }
}

