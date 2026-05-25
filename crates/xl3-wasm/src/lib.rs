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
    render_from_bytes_to_files_with_inputs, InputKind, InputSpec, OutputFile, PreviewResult,
    Value, XtlWarning,
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
) -> Result<JsValue, JsError> {
    convert_inner(template, data, inputs).map_err(to_js_error)
}

fn convert_inner(template: &[u8], data: &[u8], inputs: JsValue) -> Result<JsValue> {
    let host_inputs = parse_inputs(&inputs)?;
    let files = render_from_bytes_to_files_with_inputs(template, data.to_vec(), &host_inputs)?;
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
