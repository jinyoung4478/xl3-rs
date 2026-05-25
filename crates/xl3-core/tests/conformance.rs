//! Conformance harness — runs xl3 (TS reference) `conformance/fixtures/`
//! against the Rust implementation. Stage 1 (cell-value comparison) only.
//!
//! The TS spec corpus lives in the sibling repository
//! `/Users/wefun/workspaces/playground/xl3/conformance/fixtures/`. Tests
//! are skipped (with a note) if that directory isn't present, so this
//! crate stays buildable in isolation.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use xl3_core::calamine::{open_workbook, Data, Reader, Xlsx};
use xl3_core::{render::render_from_paths, value::Value};

fn conformance_root() -> Option<PathBuf> {
    // Resolves to the sibling xl3 repo. Falls back to None when running
    // somewhere that doesn't have it (e.g. CI without the TS repo).
    let candidates = [
        PathBuf::from("/Users/wefun/workspaces/playground/xl3/conformance/fixtures"),
        PathBuf::from("../../../xl3/conformance/fixtures"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn fixture_dir(name: &str) -> Option<PathBuf> {
    let root = conformance_root()?;
    let dir = root.join(name);
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

fn run_fixture(fixture_name: &str) -> Result<()> {
    let Some(dir) = fixture_dir(fixture_name) else {
        eprintln!(
            "[skip] {fixture_name}: xl3 conformance corpus not found (looked for the sibling xl3 repo)"
        );
        return Ok(());
    };

    let template = dir.join("template.xlsx");
    let data = dir.join("data.xlsx");
    let expected = dir.join("expected.xlsx");

    if !expected.exists() {
        return Err(anyhow!(
            "fixture {fixture_name} has no expected.xlsx — error/dynamic fixtures not yet supported by the Rust runner"
        ));
    }

    let actual_bytes = render_from_paths(&template, &data)
        .with_context(|| format!("render fixture {fixture_name}"))?;

    compare_workbooks_stage1(&actual_bytes, &expected)
        .with_context(|| format!("compare fixture {fixture_name}"))
}

fn compare_workbooks_stage1(actual_bytes: &[u8], expected_path: &Path) -> Result<()> {
    let actual_cells = read_all_cells_from_bytes(actual_bytes)?;
    let expected_cells = read_all_cells_from_path(expected_path)?;

    if actual_cells.len() != expected_cells.len() {
        return Err(anyhow!(
            "sheet count mismatch — expected {}, got {} (sheets={:?} vs {:?})",
            expected_cells.len(),
            actual_cells.len(),
            expected_cells.keys().collect::<Vec<_>>(),
            actual_cells.keys().collect::<Vec<_>>(),
        ));
    }

    for (sheet, expected_rows) in &expected_cells {
        let actual_rows = actual_cells.get(sheet).ok_or_else(|| {
            anyhow!("expected sheet {sheet:?} missing from actual output")
        })?;
        if expected_rows.len() != actual_rows.len() {
            return Err(anyhow!(
                "sheet {sheet:?} row count mismatch — expected {}, got {}",
                expected_rows.len(),
                actual_rows.len(),
            ));
        }
        for (r, (exp, act)) in expected_rows.iter().zip(actual_rows.iter()).enumerate() {
            if exp.len() != act.len() {
                return Err(anyhow!(
                    "sheet {sheet:?} row {r} col count mismatch — expected {}, got {}",
                    exp.len(),
                    act.len(),
                ));
            }
            for (c, (e, a)) in exp.iter().zip(act.iter()).enumerate() {
                if !value_eq(e, a) {
                    return Err(anyhow!(
                        "sheet {sheet:?} cell ({r},{c}) mismatch — expected {e:?}, got {a:?}",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn read_all_cells_from_path(path: &Path) -> Result<HashMap<String, Vec<Vec<Value>>>> {
    let mut wb: Xlsx<_> =
        open_workbook(path).with_context(|| format!("open {}", path.display()))?;
    read_all_cells(&mut wb)
}

fn read_all_cells_from_bytes(bytes: &[u8]) -> Result<HashMap<String, Vec<Vec<Value>>>> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut wb: Xlsx<_> = Xlsx::new(cursor).context("open actual workbook from bytes")?;
    read_all_cells(&mut wb)
}

fn read_all_cells<R: std::io::Read + std::io::Seek>(
    wb: &mut Xlsx<R>,
) -> Result<HashMap<String, Vec<Vec<Value>>>> {
    let mut out = HashMap::new();
    let names: Vec<String> = wb.sheet_names();
    for name in names {
        // Reserved sheets in the *output* would be a bug, but the
        // expected workbooks of the conformance corpus never contain
        // them — they were already stripped at render time.
        let range = wb
            .worksheet_range(&name)
            .with_context(|| format!("read sheet {name}"))?;
        let (rows, cols) = range.get_size();
        let mut sheet_rows = Vec::with_capacity(rows);
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols);
            for c in 0..cols {
                let v = range
                    .get((r, c))
                    .map(|d| match d {
                        Data::Empty => Value::Empty,
                        Data::String(s) => Value::String(s.clone()),
                        Data::Float(f) => Value::Number(*f),
                        Data::Int(i) => Value::Number(*i as f64),
                        Data::Bool(b) => Value::Bool(*b),
                        Data::DateTime(dt) => Value::Number(dt.as_f64()),
                        Data::DateTimeIso(s) | Data::DurationIso(s) => Value::String(s.clone()),
                        Data::Error(_) => Value::Empty,
                    })
                    .unwrap_or(Value::Empty);
                row.push(v);
            }
            sheet_rows.push(row);
        }
        out.insert(name, sheet_rows);
    }
    Ok(out)
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Empty, Value::Empty) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => {
            // Tolerant: bit-exact for finite, NaN == NaN OK for spec.
            if x.is_nan() && y.is_nan() {
                true
            } else {
                (x - y).abs() < 1e-9 || x == y
            }
        }
        _ => false,
    }
}

#[test]
fn fixture_001_bracket_substitution() {
    run_fixture("001-bracket-substitution").expect("fixture 001 should pass");
}
