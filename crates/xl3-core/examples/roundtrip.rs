//! Phase 0 measurement: native XLSX roundtrip using calamine + rust_xlsxwriter.
//!
//! Reads every sheet from the input workbook (cached cell values only — no
//! style / merge / formula preservation) and writes the same cell values out
//! through `rust_xlsxwriter`. This intentionally ignores everything xl3 cares
//! about (styles, conditional formatting, merges, etc.) — the goal is to
//! pin down the **upper bound** of how fast the underlying I/O engines can
//! move cells in and out. See `PLAN.md` §5 Phase 0 Task 0.2.
//!
//! Usage:
//!   cargo run --release -p xl3-core --example roundtrip -- <input.xlsx> [output.xlsx]

use std::env;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use calamine::{open_workbook, Data, Reader, Xlsx};
use rust_xlsxwriter::Workbook;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        bail!("usage: roundtrip <input.xlsx> [output.xlsx]");
    }
    let input = args[1].clone();
    let output = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "out/roundtrip.xlsx".to_string());

    if let Some(parent) = Path::new(&output).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }

    let input_bytes = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
    eprintln!("input : {input}  ({} MB)", input_bytes / (1024 * 1024));
    eprintln!("output: {output}");

    // ---- load phase ----
    let t_load = Instant::now();
    let mut wb: Xlsx<_> = open_workbook(&input).context("open input xlsx")?;
    let sheet_names = wb.sheet_names();
    let mut ranges = Vec::with_capacity(sheet_names.len());
    let mut total_cells = 0u64;
    for name in &sheet_names {
        let range = wb
            .worksheet_range(name)
            .with_context(|| format!("read sheet {name}"))?;
        let (rows, cols) = range.get_size();
        total_cells += (rows as u64) * (cols as u64);
        ranges.push((name.clone(), range));
    }
    let load_ms = t_load.elapsed().as_millis();
    eprintln!(
        "load  : {load_ms:>6} ms  sheets={}  cells(range area)={}",
        sheet_names.len(),
        total_cells
    );

    // ---- write phase ----
    let t_write = Instant::now();
    let mut out_wb = Workbook::new();
    let mut written = 0u64;
    for (idx, (_name, range)) in ranges.iter().enumerate() {
        let ws = out_wb.add_worksheet();
        // Output sheet names are normalized to S{idx} — we are measuring I/O,
        // not name preservation. Phase 1 will keep the originals via the
        // manifest.
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
                        written += 1;
                    }
                    Data::Float(f) => {
                        ws.write_number(r32, c16, *f)?;
                        written += 1;
                    }
                    Data::Int(i) => {
                        ws.write_number(r32, c16, *i as f64)?;
                        written += 1;
                    }
                    Data::Bool(b) => {
                        ws.write_boolean(r32, c16, *b)?;
                        written += 1;
                    }
                    Data::DateTime(dt) => {
                        ws.write_number(r32, c16, dt.as_f64())?;
                        written += 1;
                    }
                    Data::DateTimeIso(s) | Data::DurationIso(s) => {
                        ws.write_string(r32, c16, s)?;
                        written += 1;
                    }
                    Data::Error(_) => {}
                }
            }
        }
    }
    out_wb.save(&output).context("save output xlsx")?;
    let write_ms = t_write.elapsed().as_millis();
    eprintln!("write : {write_ms:>6} ms  cells_written={written}");

    let total_ms = load_ms + write_ms;
    let out_bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "total : {total_ms:>6} ms  output={} MB",
        out_bytes / (1024 * 1024)
    );

    Ok(())
}
