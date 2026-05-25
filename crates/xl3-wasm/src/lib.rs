//! xl3-wasm — thin wasm-bindgen wrapper around `xl3-core`.
//!
//! Phase 2 Task 2.1: exposes the three host-facing surfaces of
//! `xl3-core` to JavaScript consumers:
//!
//! - `convert(template, data, inputs)` — render output files
//! - `readTemplateInputs(template)` — describe `__inputs__`
//! - `preview(template, data)` — describe the rendered shape without
//!   producing the bytes
//!
//! Each entry point keeps the wasm-bindgen layer to argument
//! marshalling + error mapping. All semantics live in `xl3-core`
//! (PLAN.md §4.1).

use std::collections::HashMap;
use std::io::Cursor;

use anyhow::Result;
use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

use xl3_core::calamine::{Data, Reader, Xlsx};
use xl3_core::rust_xlsxwriter::Workbook;
use xl3_core::{
    is_xtl_error, preview_bytes, read_template_inputs_bytes,
    render_from_bytes_to_files_full, AlignmentSpec, ColumnWidth, FillPattern, FillSpec,
    FontSpec, HorizontalAlign, InputKind, InputSpec, OutputFile, PreviewResult, StyleManifest,
    StyleSpec, Value, VerticalAlign, XtlWarning,
};

#[cfg(feature = "debug")]
#[wasm_bindgen(start)]
pub fn _start() {
    console_error_panic_hook::set_once();
}

/// Phase 0 measurement entry point.
///
/// Reads cached cell values from `input` (an in-memory XLSX byte buffer) and
/// writes them straight back through `rust_xlsxwriter`. Style / merge /
/// formula preservation is intentionally absent — this measures the I/O
/// upper bound, mirroring the native `examples/roundtrip.rs`.
#[wasm_bindgen]
pub fn roundtrip(input: &[u8]) -> Result<Vec<u8>, JsError> {
    roundtrip_inner(input).map_err(|e| JsError::new(&e.to_string()))
}

fn roundtrip_inner(input: &[u8]) -> Result<Vec<u8>> {
    let cursor = Cursor::new(input);
    let mut wb: Xlsx<_> = Xlsx::new(cursor)?;
    let sheet_names = wb.sheet_names();

    let mut ranges = Vec::with_capacity(sheet_names.len());
    for name in &sheet_names {
        let range = wb.worksheet_range(name)?;
        ranges.push(range);
    }

    let mut out_wb = Workbook::new();
    for (idx, range) in ranges.iter().enumerate() {
        let ws = out_wb.add_worksheet();
        ws.set_name(format!("S{}", idx + 1))?;
        let (rows, cols) = range.get_size();
        for r in 0..rows {
            for c in 0..cols {
                let Some(value) = range.get((r, c)) else { continue };
                let r32 = r as u32;
                let c16 = c as u16;
                match value {
                    Data::Empty => {}
                    Data::String(s) => {
                        ws.write_string(r32, c16, s)?;
                    }
                    Data::Float(f) => {
                        ws.write_number(r32, c16, *f)?;
                    }
                    Data::Int(i) => {
                        ws.write_number(r32, c16, *i as f64)?;
                    }
                    Data::Bool(b) => {
                        ws.write_boolean(r32, c16, *b)?;
                    }
                    Data::DateTime(dt) => {
                        ws.write_number(r32, c16, dt.as_f64())?;
                    }
                    Data::DateTimeIso(s) | Data::DurationIso(s) => {
                        ws.write_string(r32, c16, s)?;
                    }
                    Data::Error(_) => {}
                }
            }
        }
    }

    let buf = out_wb.save_to_buffer()?;
    Ok(buf)
}

// ===========================================================================
// Phase 2 Task 2.1 — real render entry points.
// ===========================================================================

/// Render output files for a template + data buffer pair.
/// Returns `[{ filename, data: Uint8Array, warnings: [{ message }] }]`.
///
/// `inputs` may be `undefined`, an empty object, or `{ key: value }`
/// where each value is a string / number / boolean. Anything else is
/// stringified — the renderer accepts those forms via `Value::String`.
#[wasm_bindgen]
pub fn convert(
    template: &[u8],
    data: &[u8],
    inputs: JsValue,
    manifest: JsValue,
) -> Result<JsValue, JsError> {
    convert_inner(template, data, inputs, manifest).map_err(to_js_error)
}

fn convert_inner(
    template: &[u8],
    data: &[u8],
    inputs: JsValue,
    manifest: JsValue,
) -> Result<JsValue> {
    let host_inputs = parse_inputs(&inputs)?;
    let style_manifest = parse_manifest(&manifest)?;
    let files = render_from_bytes_to_files_full(
        template,
        data.to_vec(),
        &host_inputs,
        style_manifest,
    )?;
    let arr = Array::new();
    for f in files {
        arr.push(&output_file_to_js(&f)?);
    }
    Ok(arr.into())
}

/// JS surface: `readTemplateInputs(template) -> InputSpec[]`.
#[wasm_bindgen(js_name = readTemplateInputs)]
pub fn read_template_inputs(template: &[u8]) -> Result<JsValue, JsError> {
    read_template_inputs_inner(template).map_err(to_js_error)
}

fn read_template_inputs_inner(template: &[u8]) -> Result<JsValue> {
    let specs = read_template_inputs_bytes(template)?;
    let arr = Array::new();
    for spec in &specs {
        arr.push(&input_spec_to_js(spec)?);
    }
    Ok(arr.into())
}

/// JS surface: `preview(template, data) -> PreviewResult`.
#[wasm_bindgen]
pub fn preview(template: &[u8], data: &[u8]) -> Result<JsValue, JsError> {
    preview_inner(template, data).map_err(to_js_error)
}

fn preview_inner(template: &[u8], data: &[u8]) -> Result<JsValue> {
    let p = preview_bytes(template, data.to_vec())?;
    preview_to_js(&p)
}

// ---------------------------------------------------------------------------
// JS ↔ Rust marshalling helpers. Kept here (rather than in xl3-core) so
// the core crate stays free of `wasm_bindgen` / `js_sys` types — the
// architectural boundary called out in CLAUDE.md.
// ---------------------------------------------------------------------------

fn parse_inputs(inputs: &JsValue) -> Result<HashMap<String, Value>> {
    let mut out = HashMap::new();
    if inputs.is_undefined() || inputs.is_null() {
        return Ok(out);
    }
    let obj = inputs
        .dyn_ref::<Object>()
        .ok_or_else(|| anyhow::anyhow!("inputs argument must be an object"))?;
    let keys = Object::keys(obj);
    for i in 0..keys.length() {
        let key_val = keys.get(i);
        let key = key_val
            .as_string()
            .ok_or_else(|| anyhow::anyhow!("input key {i} is not a string"))?;
        let val = Reflect::get(obj, &key_val)
            .map_err(|_| anyhow::anyhow!("failed to read input {key:?}"))?;
        let v = if let Some(s) = val.as_string() {
            Value::String(s)
        } else if let Some(n) = val.as_f64() {
            Value::Number(n)
        } else if let Some(b) = val.as_bool() {
            Value::Bool(b)
        } else if val.is_null() || val.is_undefined() {
            Value::Empty
        } else {
            // Fall back to the JS toString; mirrors how a host would
            // splat a complex value into a template cell anyway.
            Value::String(format!("{val:?}"))
        };
        out.insert(key, v);
    }
    Ok(out)
}

fn output_file_to_js(f: &OutputFile) -> Result<JsValue> {
    let obj = Object::new();
    set_str(&obj, "filename", &f.filename)?;
    let data = Uint8Array::new_with_length(f.data.len() as u32);
    data.copy_from(&f.data);
    Reflect::set(&obj, &JsValue::from_str("data"), &data)
        .map_err(|_| anyhow::anyhow!("set data"))?;
    let warnings = Array::new();
    for w in &f.warnings {
        warnings.push(&warning_to_js(w)?);
    }
    Reflect::set(&obj, &JsValue::from_str("warnings"), &warnings)
        .map_err(|_| anyhow::anyhow!("set warnings"))?;
    Ok(obj.into())
}

fn warning_to_js(w: &XtlWarning) -> Result<JsValue> {
    let obj = Object::new();
    set_str(&obj, "message", &w.message)?;
    Ok(obj.into())
}

fn input_spec_to_js(spec: &InputSpec) -> Result<JsValue> {
    let obj = Object::new();
    set_str(&obj, "name", &spec.name)?;
    set_str(&obj, "kind", input_kind_str(spec.kind))?;
    Reflect::set(
        &obj,
        &JsValue::from_str("required"),
        &JsValue::from_bool(spec.required),
    )
    .map_err(|_| anyhow::anyhow!("set required"))?;
    set_opt_str(&obj, "default", spec.default.as_deref())?;
    set_opt_str(&obj, "label", spec.label.as_deref())?;
    set_opt_str(&obj, "description", spec.description.as_deref())?;
    let options = Array::new();
    for opt in &spec.options {
        options.push(&JsValue::from_str(opt));
    }
    Reflect::set(&obj, &JsValue::from_str("options"), &options)
        .map_err(|_| anyhow::anyhow!("set options"))?;
    Ok(obj.into())
}

fn input_kind_str(k: InputKind) -> &'static str {
    match k {
        InputKind::Text => "text",
        InputKind::Number => "number",
        InputKind::Date => "date",
        InputKind::Select => "select",
        InputKind::Other => "other",
    }
}

fn preview_to_js(p: &PreviewResult) -> Result<JsValue> {
    let obj = Object::new();
    let files = Array::new();
    for f in &p.files {
        let fo = Object::new();
        set_str(&fo, "filename", &f.filename)?;
        let sheets = Array::new();
        for s in &f.sheets {
            let so = Object::new();
            set_str(&so, "name", &s.name)?;
            sheets.push(&so);
        }
        Reflect::set(&fo, &JsValue::from_str("sheets"), &sheets)
            .map_err(|_| anyhow::anyhow!("set sheets"))?;
        files.push(&fo);
    }
    Reflect::set(&obj, &JsValue::from_str("files"), &files)
        .map_err(|_| anyhow::anyhow!("set files"))?;
    let sources = Array::new();
    for s in &p.sources {
        let so = Object::new();
        set_str(&so, "name", &s.name)?;
        let headers = Array::new();
        for h in &s.headers {
            headers.push(&JsValue::from_str(h));
        }
        Reflect::set(&so, &JsValue::from_str("headers"), &headers)
            .map_err(|_| anyhow::anyhow!("set headers"))?;
        Reflect::set(
            &so,
            &JsValue::from_str("rowCount"),
            &JsValue::from_f64(s.row_count as f64),
        )
        .map_err(|_| anyhow::anyhow!("set rowCount"))?;
        sources.push(&so);
    }
    Reflect::set(&obj, &JsValue::from_str("sources"), &sources)
        .map_err(|_| anyhow::anyhow!("set sources"))?;
    Ok(obj.into())
}

fn set_str(obj: &Object, key: &str, value: &str) -> Result<()> {
    Reflect::set(obj, &JsValue::from_str(key), &JsValue::from_str(value))
        .map_err(|_| anyhow::anyhow!("set {key}"))?;
    Ok(())
}

fn set_opt_str(obj: &Object, key: &str, value: Option<&str>) -> Result<()> {
    let v = match value {
        Some(s) => JsValue::from_str(s),
        None => JsValue::NULL,
    };
    Reflect::set(obj, &JsValue::from_str(key), &v).map_err(|_| anyhow::anyhow!("set {key}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// StyleManifest decoding — mirror of `xl3/src/manifest.ts` shape.
// JSON path: { styles: [...], cells: {sheet: {"row,col": idx, ...}, ...},
//              merges: {sheet: ["A1:B2", ...], ...},
//              columns: {sheet: [{col, width}, ...], ...} }
// ---------------------------------------------------------------------------

fn parse_manifest(val: &JsValue) -> Result<Option<StyleManifest>> {
    if val.is_undefined() || val.is_null() {
        return Ok(None);
    }
    let obj = val
        .dyn_ref::<Object>()
        .ok_or_else(|| anyhow::anyhow!("manifest argument must be an object"))?;

    let styles_js = Reflect::get(obj, &JsValue::from_str("styles"))
        .map_err(|_| anyhow::anyhow!("manifest.styles missing"))?;
    let styles_arr = styles_js
        .dyn_ref::<Array>()
        .ok_or_else(|| anyhow::anyhow!("manifest.styles must be an array"))?;
    let mut styles = Vec::with_capacity(styles_arr.length() as usize);
    for i in 0..styles_arr.length() {
        styles.push(parse_style_spec(&styles_arr.get(i))?);
    }

    let cells = parse_string_keyed_object(obj, "cells", |sheet_val| {
        let sheet_obj = sheet_val
            .dyn_ref::<Object>()
            .ok_or_else(|| anyhow::anyhow!("manifest.cells.<sheet> must be an object"))?;
        let keys = Object::keys(sheet_obj);
        let mut out: HashMap<(u32, u32), usize> = HashMap::with_capacity(keys.length() as usize);
        for j in 0..keys.length() {
            let key_val = keys.get(j);
            let key_str = key_val
                .as_string()
                .ok_or_else(|| anyhow::anyhow!("cell key is not a string"))?;
            let (row, col) = parse_row_col_key(&key_str)?;
            let v = Reflect::get(sheet_obj, &key_val)
                .map_err(|_| anyhow::anyhow!("read cell entry"))?;
            let idx = v
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("cell entry must be a number"))?;
            out.insert((row, col), idx as usize);
        }
        Ok(out)
    })?;

    let merges = parse_string_keyed_object(obj, "merges", |sheet_val| {
        let arr = sheet_val
            .dyn_ref::<Array>()
            .ok_or_else(|| anyhow::anyhow!("manifest.merges.<sheet> must be an array"))?;
        let mut out = Vec::with_capacity(arr.length() as usize);
        for j in 0..arr.length() {
            let s = arr
                .get(j)
                .as_string()
                .ok_or_else(|| anyhow::anyhow!("merge entry must be a string"))?;
            out.push(s);
        }
        Ok(out)
    })?;

    let columns = parse_string_keyed_object(obj, "columns", |sheet_val| {
        let arr = sheet_val
            .dyn_ref::<Array>()
            .ok_or_else(|| anyhow::anyhow!("manifest.columns.<sheet> must be an array"))?;
        let mut out = Vec::with_capacity(arr.length() as usize);
        for j in 0..arr.length() {
            let entry = arr.get(j);
            let entry_obj = entry
                .dyn_ref::<Object>()
                .ok_or_else(|| anyhow::anyhow!("column entry must be an object"))?;
            let col = Reflect::get(entry_obj, &JsValue::from_str("col"))
                .map_err(|_| anyhow::anyhow!("read column.col"))?
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("column.col must be a number"))?;
            let width = Reflect::get(entry_obj, &JsValue::from_str("width"))
                .map_err(|_| anyhow::anyhow!("read column.width"))?
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("column.width must be a number"))?;
            out.push(ColumnWidth {
                col: col as u32,
                width,
            });
        }
        Ok(out)
    })?;

    Ok(Some(StyleManifest {
        styles,
        cells,
        merges,
        columns,
    }))
}

fn parse_string_keyed_object<V>(
    obj: &Object,
    key: &str,
    mut decode: impl FnMut(&JsValue) -> Result<V>,
) -> Result<HashMap<String, V>> {
    let val = Reflect::get(obj, &JsValue::from_str(key))
        .map_err(|_| anyhow::anyhow!("read {key}"))?;
    if val.is_undefined() || val.is_null() {
        return Ok(HashMap::new());
    }
    let inner = val
        .dyn_ref::<Object>()
        .ok_or_else(|| anyhow::anyhow!("manifest.{key} must be an object"))?;
    let keys = Object::keys(inner);
    let mut out = HashMap::with_capacity(keys.length() as usize);
    for i in 0..keys.length() {
        let k_val = keys.get(i);
        let k_str = k_val
            .as_string()
            .ok_or_else(|| anyhow::anyhow!("{key} key is not a string"))?;
        let v = Reflect::get(inner, &k_val).map_err(|_| anyhow::anyhow!("read {key}.{k_str}"))?;
        out.insert(k_str, decode(&v)?);
    }
    Ok(out)
}

fn parse_row_col_key(s: &str) -> Result<(u32, u32)> {
    let (r, c) = s
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("cell key {s:?} must be 'row,col'"))?;
    let row: u32 = r
        .parse()
        .map_err(|_| anyhow::anyhow!("row in {s:?} is not a non-negative integer"))?;
    let col: u32 = c
        .parse()
        .map_err(|_| anyhow::anyhow!("col in {s:?} is not a non-negative integer"))?;
    Ok((row, col))
}

fn parse_style_spec(val: &JsValue) -> Result<StyleSpec> {
    let obj = val
        .dyn_ref::<Object>()
        .ok_or_else(|| anyhow::anyhow!("style entry must be an object"))?;
    let mut spec = StyleSpec::default();
    if let Some(font_val) = optional_field(obj, "font")? {
        spec.font = Some(parse_font(&font_val)?);
    }
    if let Some(num_fmt) = optional_string(obj, "numFmt")? {
        spec.num_fmt = Some(num_fmt);
    }
    if let Some(align_val) = optional_field(obj, "alignment")? {
        spec.alignment = Some(parse_alignment(&align_val)?);
    }
    if let Some(fill_val) = optional_field(obj, "fill")? {
        spec.fill = Some(parse_fill(&fill_val)?);
    }
    Ok(spec)
}

fn parse_font(val: &JsValue) -> Result<FontSpec> {
    let obj = val
        .dyn_ref::<Object>()
        .ok_or_else(|| anyhow::anyhow!("font must be an object"))?;
    Ok(FontSpec {
        name: optional_string(obj, "name")?,
        size: optional_f64(obj, "size")?,
        bold: optional_bool(obj, "bold")?.unwrap_or(false),
        italic: optional_bool(obj, "italic")?.unwrap_or(false),
        underline: optional_bool(obj, "underline")?.unwrap_or(false),
        color: optional_string(obj, "color")?,
    })
}

fn parse_alignment(val: &JsValue) -> Result<AlignmentSpec> {
    let obj = val
        .dyn_ref::<Object>()
        .ok_or_else(|| anyhow::anyhow!("alignment must be an object"))?;
    let horizontal = match optional_string(obj, "horizontal")?.as_deref() {
        Some("left") => Some(HorizontalAlign::Left),
        Some("center") => Some(HorizontalAlign::Center),
        Some("right") => Some(HorizontalAlign::Right),
        Some("justify") => Some(HorizontalAlign::Justify),
        _ => None,
    };
    let vertical = match optional_string(obj, "vertical")?.as_deref() {
        Some("top") => Some(VerticalAlign::Top),
        Some("middle") => Some(VerticalAlign::Middle),
        Some("bottom") => Some(VerticalAlign::Bottom),
        _ => None,
    };
    Ok(AlignmentSpec {
        horizontal,
        vertical,
        wrap_text: optional_bool(obj, "wrapText")?.unwrap_or(false),
        indent: optional_f64(obj, "indent")?.unwrap_or(0.0) as u8,
    })
}

fn parse_fill(val: &JsValue) -> Result<FillSpec> {
    let obj = val
        .dyn_ref::<Object>()
        .ok_or_else(|| anyhow::anyhow!("fill must be an object"))?;
    let pattern_str = optional_string(obj, "pattern")?
        .ok_or_else(|| anyhow::anyhow!("fill.pattern required"))?;
    let pattern = match pattern_str.as_str() {
        "solid" => FillPattern::Solid,
        other => anyhow::bail!("unsupported fill.pattern {other:?}"),
    };
    let color = optional_string(obj, "color")?
        .ok_or_else(|| anyhow::anyhow!("fill.color required"))?;
    Ok(FillSpec { pattern, color })
}

fn optional_field(obj: &Object, key: &str) -> Result<Option<JsValue>> {
    let v = Reflect::get(obj, &JsValue::from_str(key))
        .map_err(|_| anyhow::anyhow!("read {key}"))?;
    if v.is_undefined() || v.is_null() {
        Ok(None)
    } else {
        Ok(Some(v))
    }
}

fn optional_string(obj: &Object, key: &str) -> Result<Option<String>> {
    Ok(optional_field(obj, key)?.and_then(|v| v.as_string()))
}

fn optional_f64(obj: &Object, key: &str) -> Result<Option<f64>> {
    Ok(optional_field(obj, key)?.and_then(|v| v.as_f64()))
}

fn optional_bool(obj: &Object, key: &str) -> Result<Option<bool>> {
    Ok(optional_field(obj, key)?.and_then(|v| v.as_bool()))
}

fn to_js_error(e: anyhow::Error) -> JsError {
    // The message format matches `XtlError`'s Display impl when the
    // root cause is one — `[xl3/...] message`. JS callers parse the
    // prefix to recover the stable error code.
    if let Some(xtl) = is_xtl_error(&e) {
        JsError::new(&format!("{xtl}"))
    } else {
        JsError::new(&format!("{e:#}"))
    }
}
