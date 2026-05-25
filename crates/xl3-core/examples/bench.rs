//! xl3-rs cross-impl bench — same three scenarios as xl3 (TS)'s
//! `scripts/bench.mjs` (wide-flat / multi-sheet / multi-source-join).
//!
//! Run: `cargo run --release -p xl3-core --example bench`
//!
//! Each scenario builds its template + data workbooks procedurally,
//! writes them to a temp directory, then times `render_from_paths`
//! across three runs and reports the median in milliseconds — same
//! shape as the TS bench so the two can be compared directly. The
//! numbers are a regression signal, not a conformance contract.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use rust_xlsxwriter::Workbook;
use xl3_core::render::render_from_paths;

fn main() -> Result<()> {
    let tmp = std::env::temp_dir().join("xl3rs-bench");
    std::fs::create_dir_all(&tmp).context("create temp bench dir")?;

    println!("xl3-rs bench — median of 3 runs (xl3 TS baseline in parens)");
    println!("{}", "-".repeat(72));

    let scenarios: Vec<(&str, &str, fn(&Path) -> Result<(PathBuf, PathBuf)>)> = vec![
        (
            "wide-flat (10k rows × 4 cols)",
            "~220 ms",
            build_wide_flat,
        ),
        (
            "multi-sheet (5k rows × 5 sheets)",
            "~70 ms",
            build_multi_sheet,
        ),
        (
            "multi-source-join (5k × 1k)",
            "~70 ms",
            build_multi_source_join,
        ),
    ];

    for (label, baseline, build) in scenarios {
        let (tpl, data) = build(&tmp)?;
        let median_ms = time_run(|| {
            let files = render_from_paths(&tpl, &data)?;
            if files.is_empty() {
                anyhow::bail!("no output");
            }
            Ok(())
        })?;
        println!(
            "  {:<40} {:>6.0} ms  (TS {})",
            label, median_ms, baseline
        );
    }
    Ok(())
}

fn time_run(mut f: impl FnMut() -> Result<()>) -> Result<f64> {
    let mut samples = Vec::with_capacity(3);
    for _ in 0..3 {
        let t0 = Instant::now();
        f()?;
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok(samples[1])
}

fn build_wide_flat(tmp: &Path) -> Result<(PathBuf, PathBuf)> {
    let tpl_path = tmp.join("wide-flat.template.xlsx");
    let data_path = tmp.join("wide-flat.data.xlsx");

    {
        let mut wb = Workbook::new();
        write_config(
            &mut wb,
            &[
                ("name", "wide-flat"),
                ("source_sheet", "Data"),
                ("source_table", "1"),
                ("output_file_pattern", "wide.xlsx"),
            ],
        )?;
        let ws = wb.add_worksheet();
        ws.set_name("Out")?;
        ws.write_string(0, 0, "Account")?;
        ws.write_string(0, 1, "Region")?;
        ws.write_string(0, 2, "Amount")?;
        ws.write_string(0, 3, "Tier")?;
        ws.write_string(1, 0, "{{ [Account] }}")?;
        ws.write_string(1, 1, "{{ [Region] }}")?;
        ws.write_string(1, 2, "{{ ROUND([Amount], 2) }}")?;
        ws.write_string(
            1,
            3,
            "{{ IF([Amount] > 10000, \"Priority\", \"Standard\") }}",
        )?;
        wb.save(&tpl_path)?;
    }

    {
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name("Data")?;
        ws.write_string(0, 0, "Account")?;
        ws.write_string(0, 1, "Region")?;
        ws.write_string(0, 2, "Amount")?;
        for i in 0..10_000u32 {
            ws.write_string(i + 1, 0, format!("Acct-{i}"))?;
            ws.write_string(i + 1, 1, if i % 5 == 0 { "Seoul" } else { "Busan" })?;
            ws.write_number(i + 1, 2, ((i as f64) * 7.0) % 30_000.0)?;
        }
        wb.save(&data_path)?;
    }

    Ok((tpl_path, data_path))
}

fn build_multi_sheet(tmp: &Path) -> Result<(PathBuf, PathBuf)> {
    let tpl_path = tmp.join("multi-sheet.template.xlsx");
    let data_path = tmp.join("multi-sheet.data.xlsx");

    {
        let mut wb = Workbook::new();
        write_config(
            &mut wb,
            &[
                ("name", "multi-sheet"),
                ("source_sheet", "Data"),
                ("source_table", "1"),
                ("output_file_pattern", "multi.xlsx"),
            ],
        )?;
        let ws = wb.add_worksheet();
        ws.set_name("{{ Region }}")?;
        ws.write_string(0, 0, "Account")?;
        ws.write_string(0, 1, "Amount")?;
        ws.write_string(1, 0, "{{ [Account] }}")?;
        ws.write_string(1, 1, "{{ [Amount] }}")?;
        wb.save(&tpl_path)?;
    }

    {
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name("Data")?;
        ws.write_string(0, 0, "Account")?;
        ws.write_string(0, 1, "Region")?;
        ws.write_string(0, 2, "Amount")?;
        let regions = ["Seoul", "Busan", "Daegu", "Incheon", "Jeju"];
        for i in 0..5_000u32 {
            ws.write_string(i + 1, 0, format!("A{i}"))?;
            ws.write_string(i + 1, 1, regions[(i as usize) % regions.len()])?;
            ws.write_number(i + 1, 2, i as f64)?;
        }
        wb.save(&data_path)?;
    }

    Ok((tpl_path, data_path))
}

fn build_multi_source_join(tmp: &Path) -> Result<(PathBuf, PathBuf)> {
    let tpl_path = tmp.join("multi-source-join.template.xlsx");
    let data_path = tmp.join("multi-source-join.data.xlsx");

    {
        let mut wb = Workbook::new();
        write_config(
            &mut wb,
            &[
                ("name", "multi-source-join"),
                ("source_sheet", "Renewals"),
                ("source_table", "1"),
                ("output_file_pattern", "join.xlsx"),
            ],
        )?;
        write_sources(
            &mut wb,
            &[("Renewals", "Renewals", "1"), ("Customers", "Customers", "1")],
        )?;
        let ws = wb.add_worksheet();
        ws.set_name("Out")?;
        ws.write_string(0, 0, "Account")?;
        ws.write_string(0, 1, "Region")?;
        ws.write_string(0, 2, "Amount")?;
        ws.write_string(1, 0, "{{ @source Renewals }}")?;
        ws.write_string(
            2,
            0,
            "{{ @join Customers on Customers[Account] = Renewals[Account] }}",
        )?;
        ws.write_string(3, 0, "{{ Renewals[Account] }}")?;
        ws.write_string(3, 1, "{{ Customers[Region] }}")?;
        ws.write_string(3, 2, "{{ Renewals[Amount] }}")?;
        wb.save(&tpl_path)?;
    }

    {
        let mut wb = Workbook::new();
        let cust = wb.add_worksheet();
        cust.set_name("Customers")?;
        cust.write_string(0, 0, "Account")?;
        cust.write_string(0, 1, "Region")?;
        for i in 0..1_000u32 {
            cust.write_string(i + 1, 0, format!("A{i}"))?;
            cust.write_string(i + 1, 1, if i % 2 == 0 { "Seoul" } else { "Busan" })?;
        }
        let ren = wb.add_worksheet();
        ren.set_name("Renewals")?;
        ren.write_string(0, 0, "Account")?;
        ren.write_string(0, 1, "Amount")?;
        for i in 0..5_000u32 {
            ren.write_string(i + 1, 0, format!("A{}", i % 1250))?;
            ren.write_number(i + 1, 1, i as f64)?;
        }
        wb.save(&data_path)?;
    }

    Ok((tpl_path, data_path))
}

fn write_config(wb: &mut Workbook, rows: &[(&str, &str)]) -> Result<()> {
    let ws = wb.add_worksheet();
    ws.set_name("__config__")?;
    ws.write_string(0, 0, "key")?;
    ws.write_string(0, 1, "value")?;
    for (i, (k, v)) in rows.iter().enumerate() {
        let r = (i + 1) as u32;
        ws.write_string(r, 0, *k)?;
        ws.write_string(r, 1, *v)?;
    }
    Ok(())
}

fn write_sources(wb: &mut Workbook, rows: &[(&str, &str, &str)]) -> Result<()> {
    let ws = wb.add_worksheet();
    ws.set_name("__sources__")?;
    ws.write_string(0, 0, "name")?;
    ws.write_string(0, 1, "sheet")?;
    ws.write_string(0, 2, "table")?;
    for (i, (n, s, t)) in rows.iter().enumerate() {
        let r = (i + 1) as u32;
        ws.write_string(r, 0, *n)?;
        ws.write_string(r, 1, *s)?;
        ws.write_string(r, 2, *t)?;
    }
    Ok(())
}
