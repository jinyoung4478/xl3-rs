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

use crate::manifest::{
    AlignmentSpec, FillSpec, FontSpec, HorizontalAlign, StyleManifest, StyleSpec, VerticalAlign,
};
use crate::rust_xlsxwriter::{Color, Format, FormatAlign, Workbook};
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
    /// Per-cell index into the host-supplied
    /// `StyleManifest::styles` table (Phase 2 Task 2.2). Parallel
    /// shape to `rows`. Empty when the host didn't pass a manifest;
    /// `None` entries mean the template cell wasn't styled.
    pub style_indices: Vec<Vec<Option<usize>>>,
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
        // Cache one `Format` per unique (style_idx | format_code)
        // combination — duplicate cellXf entries balloon the output
        // and `wasm-opt` can't strip them. Keys are stringified so
        // we can share one map across both axes.
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
                let style_idx: Option<usize> = sheet
                    .style_indices
                    .get(r)
                    .and_then(|fr| fr.get(c))
                    .and_then(|f| *f);
                let style_spec: Option<&StyleSpec> = style_idx
                    .and_then(|idx| manifest.and_then(|m| m.styles.get(idx)));
                // Cache key blends both axes; manifest style wins
                // for the numFmt if both are present.
                let cache_key: Option<String> = match (style_idx, fmt_code) {
                    (Some(idx), _) => Some(format!("s:{idx}")),
                    (None, Some(code)) => Some(format!("n:{code}")),
                    (None, None) => None,
                };
                if let Some(ref key) = cache_key {
                    if !formats.contains_key(key) {
                        let mut f = Format::new();
                        if let Some(spec) = style_spec {
                            f = apply_style_spec(f, spec);
                        }
                        // Fall back to the styles.xml numFmt when
                        // the manifest didn't override it.
                        let manifest_num = style_spec.and_then(|s| s.num_fmt.as_deref());
                        if manifest_num.is_none() {
                            if let Some(code) = fmt_code {
                                f = f.set_num_format(code);
                            }
                        }
                        formats.insert(key.clone(), f);
                    }
                }
                let fmt: Option<&Format> = cache_key.as_deref().and_then(|k| formats.get(k));
                match (value, fmt) {
                    // An empty cell with a style still needs an xf
                    // attached so the recipient sees the format
                    // (header bands, banded merge cells). Cells with
                    // no style stay truly absent.
                    (Value::Empty, Some(f)) => {
                        ws.write_blank(r32, c16, f)?;
                    }
                    (Value::Empty, None) => {}
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

/// Translate a manifest `StyleSpec` into a rust_xlsxwriter Format.
/// Quietly skips fields we don't yet support (borders, indent,
/// underline kind variants) — they'll join when the schema does.
fn apply_style_spec(mut f: Format, spec: &StyleSpec) -> Format {
    if let Some(font) = &spec.font {
        f = apply_font(f, font);
    }
    if let Some(num) = spec.num_fmt.as_deref() {
        f = f.set_num_format(num);
    }
    if let Some(align) = spec.alignment.as_ref() {
        f = apply_alignment(f, align);
    }
    if let Some(fill) = &spec.fill {
        f = apply_fill(f, fill);
    }
    f
}

fn apply_font(mut f: Format, font: &FontSpec) -> Format {
    if let Some(name) = font.name.as_deref() {
        f = f.set_font_name(name);
    }
    if let Some(size) = font.size {
        // Excel surface uses points; exceljs already normalised here.
        f = f.set_font_size(size);
    }
    if font.bold {
        f = f.set_bold();
    }
    if font.italic {
        f = f.set_italic();
    }
    if font.underline {
        f = f.set_underline(rust_xlsxwriter::FormatUnderline::Single);
    }
    if let Some(argb) = font.color.as_deref() {
        if let Some(color) = parse_argb_to_color(argb) {
            f = f.set_font_color(color);
        }
    }
    f
}

fn apply_alignment(mut f: Format, a: &AlignmentSpec) -> Format {
    match a.horizontal {
        Some(HorizontalAlign::Left) => f = f.set_align(FormatAlign::Left),
        Some(HorizontalAlign::Center) => f = f.set_align(FormatAlign::Center),
        Some(HorizontalAlign::Right) => f = f.set_align(FormatAlign::Right),
        Some(HorizontalAlign::Justify) => f = f.set_align(FormatAlign::Justify),
        None => {}
    }
    match a.vertical {
        Some(VerticalAlign::Top) => f = f.set_align(FormatAlign::Top),
        Some(VerticalAlign::Middle) => f = f.set_align(FormatAlign::VerticalCenter),
        Some(VerticalAlign::Bottom) => f = f.set_align(FormatAlign::Bottom),
        None => {}
    }
    if a.wrap_text {
        f = f.set_text_wrap();
    }
    if a.indent > 0 {
        f = f.set_indent(a.indent);
    }
    f
}

fn apply_fill(mut f: Format, fill: &FillSpec) -> Format {
    // Phase 2 supports the solid pattern only. The match keeps the
    // door open for stripes/grids later without changing the call
    // site.
    match fill.pattern {
        crate::manifest::FillPattern::Solid => {
            if let Some(color) = parse_argb_to_color(&fill.color) {
                f = f
                    .set_background_color(color)
                    .set_pattern(rust_xlsxwriter::FormatPattern::Solid);
            }
        }
    }
    f
}

/// Convert an `"AARRGGBB"` (or `"RRGGBB"`) hex string into the
/// rust_xlsxwriter `Color` value. Alpha is dropped — OOXML's RGB
/// channel doesn't carry transparency in normal cell styling.
fn parse_argb_to_color(s: &str) -> Option<Color> {
    let hex = if s.len() == 8 { &s[2..] } else if s.len() == 6 { s } else { return None };
    let rgb = u32::from_str_radix(hex, 16).ok()?;
    Some(Color::RGB(rgb))
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
