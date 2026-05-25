//! xl3-wasm — thin wasm-bindgen wrapper around `xl3-core`.
//!
//! Phase 0 status: this crate currently exposes only one entry point —
//! `roundtrip(input_bytes) -> output_bytes` — used to measure the cost of
//! crossing the JS ↔ WASM boundary against the native baseline.
//! See `PLAN.md` §5 Phase 0 Task 0.3.
//!
//! The real API surface (decode TemplatePlan/Manifest JSON, call into
//! `xl3_core::render`) lands in Phase 2.

use std::io::Cursor;

use anyhow::Result;
use wasm_bindgen::prelude::*;

use xl3_core::calamine::{Data, Reader, Xlsx};
use xl3_core::rust_xlsxwriter::Workbook;

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
