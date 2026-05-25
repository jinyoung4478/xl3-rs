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
use xl3_core::{
    render::{render_from_paths_with_inputs},
    value::Value,
};

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
    let expected_single = dir.join("expected.xlsx");
    let expected_dir = dir.join("expected");

    let meta_yaml = std::fs::read_to_string(dir.join("meta.yaml")).unwrap_or_default();
    if meta_yaml
        .lines()
        .any(|l| l.trim().starts_with("comparison_stage:") && l.contains('2'))
    {
        eprintln!("[skip stage-2] {fixture_name}");
        return Ok(());
    }

    let expected_error = meta_field(&meta_yaml, "expected_error");
    let expected_dynamic = meta_field(&meta_yaml, "expected_dynamic");
    let host_inputs = parse_meta_inputs(&meta_yaml);

    // 1. expected_error: render must fail and message must include
    //    the declared substring.
    if let Some(needle) = expected_error {
        match xl3_core::render::render_from_paths_to_files_with_inputs(
            &template,
            &data,
            &host_inputs,
        ) {
            Ok(_) => {
                return Err(anyhow!(
                    "fixture {fixture_name}: expected error {needle:?} but render succeeded"
                ))
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if !msg.contains(&needle) {
                    return Err(anyhow!(
                        "fixture {fixture_name}: error message {msg:?} does not contain {needle:?}"
                    ));
                }
                return Ok(());
            }
        }
    }

    // 2. expected_dynamic: render must succeed; named cells equal
    //    the dynamic value (today only — `utc_today`).
    if let Some(kind) = expected_dynamic {
        if kind != "utc_today" {
            eprintln!("[skip dynamic-{kind}] {fixture_name}");
            return Ok(());
        }
        let files = xl3_core::render::render_from_paths_to_files_with_inputs(
            &template,
            &data,
            &host_inputs,
        )
        .with_context(|| format!("render fixture {fixture_name}"))?;
        let first = files
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("fixture {fixture_name}: render produced no files"))?;
        let actual = read_all_cells_from_bytes(&first.data)?;
        let dynamic_cells = parse_dynamic_cells(&meta_yaml);
        let today = utc_today_iso();
        for (sheet, cell_ref, _format) in &dynamic_cells {
            let (row, col) = parse_cell_ref(cell_ref).ok_or_else(|| {
                anyhow!("fixture {fixture_name}: bad dynamic cell ref {cell_ref}")
            })?;
            let rows = actual.get(sheet).ok_or_else(|| {
                anyhow!("fixture {fixture_name}: sheet {sheet:?} missing from output")
            })?;
            let got = rows
                .get(row)
                .and_then(|r| r.get(col))
                .cloned()
                .unwrap_or(Value::Empty);
            let got_str = got.canonical();
            if !got_str.starts_with(&today) {
                return Err(anyhow!(
                    "fixture {fixture_name}: dynamic cell {sheet}!{cell_ref} = {got_str:?}, expected utc_today {today:?}"
                ));
            }
        }
        return Ok(());
    }

    // 3. expected/ directory: multi-file output comparison.
    if expected_dir.is_dir() {
        let files = xl3_core::render::render_from_paths_to_files_with_inputs(
            &template,
            &data,
            &host_inputs,
        )
        .with_context(|| format!("render fixture {fixture_name}"))?;
        return compare_multi_file(&files, &expected_dir)
            .with_context(|| format!("compare fixture {fixture_name}"));
    }

    // 4. expected.xlsx: single-file path (the common case).
    if !expected_single.exists() {
        return Err(anyhow!(
            "fixture {fixture_name} has no expected.xlsx / expected/ and no expected_error / expected_dynamic — unrecognised fixture shape"
        ));
    }

    let actual_bytes = render_from_paths_with_inputs(&template, &data, &host_inputs)
        .with_context(|| format!("render fixture {fixture_name}"))?;

    compare_workbooks_stage1(&actual_bytes, &expected_single)
        .with_context(|| format!("compare fixture {fixture_name}"))
}

fn meta_field(yaml: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    yaml.lines()
        .map(str::trim_start)
        .find(|l| l.starts_with(&prefix))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| strip_yaml_scalar(v))
        .filter(|s| !s.is_empty())
}

fn parse_dynamic_cells(yaml: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut in_block = false;
    let (mut sheet, mut cell, mut fmt): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = (None, None, None);
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent == 0 && !trimmed.is_empty() {
            in_block = trimmed.starts_with("dynamic_cells:");
            continue;
        }
        if !in_block {
            continue;
        }
        let body = trimmed.trim_start_matches('-').trim_start();
        if let Some(rest) = body.strip_prefix("sheet:") {
            // start of new entry
            if let (Some(s), Some(c)) = (sheet.take(), cell.take()) {
                out.push((s, c, fmt.take().unwrap_or_default()));
            }
            sheet = Some(strip_yaml_scalar(rest));
        } else if let Some(rest) = body.strip_prefix("cell:") {
            cell = Some(strip_yaml_scalar(rest));
        } else if let Some(rest) = body.strip_prefix("format:") {
            fmt = Some(strip_yaml_scalar(rest));
        }
    }
    if let (Some(s), Some(c)) = (sheet, cell) {
        out.push((s, c, fmt.unwrap_or_default()));
    }
    out
}

fn utc_today_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs / 86_400;
    // Convert days since 1970-01-01 (UTC) to civil date (Howard
    // Hinnant's algorithm — matches functions.rs).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn parse_cell_ref(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut col: usize = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        let c = bytes[i].to_ascii_uppercase();
        col = col * 26 + (c - b'A' + 1) as usize;
        i += 1;
    }
    if col == 0 || i == bytes.len() {
        return None;
    }
    let row: usize = std::str::from_utf8(&bytes[i..]).ok()?.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

fn compare_multi_file(files: &[xl3_core::OutputFile], expected_dir: &Path) -> Result<()> {
    let mut expected_files: Vec<std::path::PathBuf> = std::fs::read_dir(expected_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "xlsx").unwrap_or(false))
        .collect();
    expected_files.sort();
    if files.len() != expected_files.len() {
        return Err(anyhow!(
            "file count mismatch — expected {}, got {}",
            expected_files.len(),
            files.len()
        ));
    }
    // Match by filename (basename).
    use std::collections::HashMap;
    let actual_by_name: HashMap<&str, &[u8]> = files
        .iter()
        .map(|f| (f.filename.as_str(), f.data.as_slice()))
        .collect();
    for expected_path in &expected_files {
        let name = expected_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("bad expected filename"))?;
        let actual_bytes = actual_by_name.get(name).ok_or_else(|| {
            anyhow!(
                "expected file {name} missing from actual output (got {:?})",
                actual_by_name.keys().collect::<Vec<_>>()
            )
        })?;
        compare_workbooks_stage1(actual_bytes, expected_path)
            .with_context(|| format!("compare file {name}"))?;
    }
    Ok(())
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

/// Tiny YAML reader that lifts a fixture's `inputs:` block into a
/// `name → Value::String` map. The corpus uses a single uniform shape:
///
///   inputs:
///     - name: region
///       value: Seoul
///
/// We don't need general YAML support — recognise indented `- name:` /
/// `name:` and `value:` lines under a top-level `inputs:` key.
fn parse_meta_inputs(yaml: &str) -> HashMap<String, Value> {
    let mut out: HashMap<String, Value> = HashMap::new();
    let mut in_inputs = false;
    let mut current_name: Option<String> = None;
    for line in yaml.lines() {
        let raw = line;
        let trimmed = raw.trim_start();
        let indent = raw.len() - trimmed.len();
        if indent == 0 && !trimmed.is_empty() {
            // top-level key resets the inputs block
            in_inputs = trimmed.starts_with("inputs:");
            current_name = None;
            continue;
        }
        if !in_inputs {
            continue;
        }
        let body = trimmed.trim_start_matches('-').trim_start();
        if let Some(rest) = body.strip_prefix("name:") {
            current_name = Some(strip_yaml_scalar(rest));
        } else if let Some(rest) = body.strip_prefix("value:") {
            if let Some(name) = &current_name {
                out.insert(name.clone(), Value::String(strip_yaml_scalar(rest)));
            }
        }
    }
    out
}

fn strip_yaml_scalar(s: &str) -> String {
    let t = s.trim();
    let stripped = t
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| t.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(t);
    stripped.to_string()
}

fn value_eq(a: &Value, b: &Value) -> bool {
    // ADR-0007 / ADR-0009: Empty and "" (and whitespace-only strings)
    // are semantically the same "blank" value. Our renderer emits
    // Empty for a missing cell; xl3's reference output sometimes
    // encodes the same blank as a zero-length shared string. Treat
    // both shapes as equivalent at the cell-value comparison layer.
    if xl3_core::source::is_blank_value(a) && xl3_core::source::is_blank_value(b) {
        return true;
    }
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

#[test]
fn fixture_002_if_function() {
    run_fixture("002-if-function").expect("fixture 002 should pass");
}

#[test]
fn fixture_004_repeat_right_default() {
    run_fixture("004-repeat-right-default").expect("fixture 004 should pass");
}

#[test]
fn fixture_044_sort_and_top_order() {
    run_fixture("044-sort-and-top-order").expect("fixture 044 should pass");
}

#[test]
fn fixture_047_aggregate_functions() {
    run_fixture("047-aggregate-functions").expect("fixture 047 should pass");
}

#[test]
fn fixture_041_row_function_inside_repeat_block() {
    run_fixture("041-row-function-inside-repeat-block")
        .expect("fixture 041 should pass");
}

#[test]
fn fixture_065_input_text_default_applied() {
    run_fixture("065-input-text-default-applied").expect("fixture 065 should pass");
}

#[test]
fn fixture_003_list_sheet_filter() {
    run_fixture("003-list-sheet-filter").expect("fixture 003 should pass");
}

#[test]
fn fixture_045_list_sheet_not_in_filter() {
    run_fixture("045-list-sheet-not-in-filter").expect("fixture 045 should pass");
}

#[test]
fn fixture_011_text_date_format() {
    run_fixture("011-text-date-format").expect("fixture 011 should pass");
}

#[test]
fn fixture_012_text_number_format() {
    run_fixture("012-text-number-format").expect("fixture 012 should pass");
}

#[test]
fn fixture_053_empty_row_skip_whitespace_only() {
    run_fixture("053-empty-row-skip-whitespace-only")
        .expect("fixture 053 should pass");
}

#[test]
fn fixture_028_source_table_row_shorthand() {
    run_fixture("028-source-table-row-shorthand").expect("fixture 028 should pass");
}

#[test]
fn fixture_030_source_table_finite_range() {
    run_fixture("030-source-table-finite-range").expect("fixture 030 should pass");
}

#[test]
fn fixture_029_source_table_open_range() {
    run_fixture("029-source-table-open-range").expect("fixture 029 should pass");
}

#[test]
fn fixture_046_count_field_non_empty() {
    run_fixture("046-count-field-non-empty").expect("fixture 046 should pass");
}

#[test]
fn fixture_052_empty_count_field_whitespace_zero_false() {
    run_fixture("052-empty-count-field-whitespace-zero-false")
        .expect("fixture 052 should pass");
}

#[test]
fn fixture_130_isblank_function() {
    run_fixture("130-isblank-function").expect("fixture 130 should pass");
}

#[test]
fn fixture_100_arithmetic_string_coerces_to_number() {
    run_fixture("100-arithmetic-string-coerces-to-number")
        .expect("fixture 100 should pass");
}

#[test]
fn fixture_069_source_multi_declaration() {
    run_fixture("069-source-multi-declaration").expect("fixture 069 should pass");
}

#[test]
fn fixture_070_source_aggregate_cross_source() {
    run_fixture("070-source-aggregate-cross-source")
        .expect("fixture 070 should pass");
}

#[test]
fn fixture_071_source_directive_active() {
    run_fixture("071-source-directive-active").expect("fixture 071 should pass");
}

#[test]
fn fixture_074_xlookup_basic() {
    run_fixture("074-xlookup-basic").expect("fixture 074 should pass");
}

#[test]
fn fixture_075_xlookup_fallback() {
    run_fixture("075-xlookup-fallback").expect("fixture 075 should pass");
}

#[test]
fn fixture_125_hyperlink_function() {
    run_fixture("125-hyperlink-function").expect("fixture 125 should pass");
}

#[test]
fn fixture_080_join_no_match_dropped() {
    run_fixture("080-join-no-match-dropped").expect("fixture 080 should pass");
}

#[test]
fn fixture_035_source_table_rich_text_header() {
    run_fixture("035-source-table-rich-text-header").expect("fixture 035 should pass");
}

#[test]
fn fixture_036_source_table_formula_header() {
    run_fixture("036-source-table-formula-header").expect("fixture 036 should pass");
}

#[test]
fn fixture_015_source_sheet_prefix_first_match() {
    run_fixture("015-source-sheet-prefix-first-match")
        .expect("fixture 015 should pass");
}

#[test]
fn fixture_106_division_by_zero_produces_error_cell() {
    run_fixture("106-division-by-zero-produces-error-cell")
        .expect("fixture 106 should pass");
}

#[test]
fn fixture_088_date_comparison_equality() {
    run_fixture("088-date-comparison-equality").expect("fixture 088 should pass");
}

#[test]
fn fixture_054_empty_list_membership() {
    run_fixture("054-empty-list-membership").expect("fixture 054 should pass");
}

#[test]
fn fixture_063_compare_empty_vs_value() {
    run_fixture("063-compare-empty-vs-value").expect("fixture 063 should pass");
}

#[test]
fn fixture_132_group_single_level_subtotal() {
    run_fixture("132-group-single-level-subtotal")
        .expect("fixture 132 should pass");
}

#[test]
fn fixture_133_group_two_level_nested_subtotal() {
    run_fixture("133-group-two-level-nested-subtotal")
        .expect("fixture 133 should pass");
}

#[test]
fn fixture_134_group_grand_total_via_outermost_subtotal() {
    run_fixture("134-group-grand-total-via-outermost-subtotal")
        .expect("fixture 134 should pass");
}

#[test]
fn fixture_068_input_select_host_supplied() {
    run_fixture("068-input-select-host-supplied")
        .expect("fixture 068 should pass");
}

#[test]
fn fixture_086_sheet_group_first_seen_order() {
    run_fixture("086-sheet-group-first-seen-order")
        .expect("fixture 086 should pass");
}

#[test]
fn fixture_108_group_key_empty_blank_placeholder_sheet() {
    run_fixture("108-group-key-empty-blank-placeholder-sheet")
        .expect("fixture 108 should pass");
}

#[test]
fn fixture_092_composed_multi_source_join_filter_sort() {
    run_fixture("092-composed-multi-source-join-filter-sort")
        .expect("fixture 092 should pass");
}

#[test]
fn fixture_121_source_merged_header() {
    run_fixture("121-source-merged-header").expect("fixture 121 should pass");
}

#[test]
fn fixture_124_source_2d_merge_header() {
    run_fixture("124-source-2d-merge-header").expect("fixture 124 should pass");
}

#[test]
fn fixture_126_date_arithmetic_functions() {
    run_fixture("126-date-arithmetic-functions").expect("fixture 126 should pass");
}

#[test]
fn fixture_128_function_batch_0044() {
    run_fixture("128-function-batch-0044").expect("fixture 128 should pass");
}

#[test]
fn fixture_055_if_truthy_zero_and_empty() {
    run_fixture("055-if-truthy-zero-and-empty").expect("fixture 055 should pass");
}

#[test]
fn fixture_062_concat_empty_stringifies_to_empty() {
    run_fixture("062-concat-empty-stringifies-to-empty")
        .expect("fixture 062 should pass");
}

#[test]
fn fixture_008_numfmt_numeric_string_coercion() {
    run_fixture("008-numfmt-numeric-string-coercion")
        .expect("fixture 008 should pass");
}

#[test]
fn fixture_009_numfmt_date_string_coercion() {
    run_fixture("009-numfmt-date-string-coercion")
        .expect("fixture 009 should pass");
}

#[test]
fn fixture_010_numfmt_text_format_coercion() {
    run_fixture("010-numfmt-text-format-coercion")
        .expect("fixture 010 should pass");
}

#[test]
fn fixture_141_block_column_scoped_side_cells() {
    run_fixture("141-block-column-scoped-side-cells")
        .expect("fixture 141 should pass");
}

#[test]
fn fixture_142_block_column_scoped_side_formulas() {
    run_fixture("142-block-column-scoped-side-formulas")
        .expect("fixture 142 should pass");
}

#[test]
fn fixture_143_block_shared_formula_side_cells() {
    run_fixture("143-block-shared-formula-side-cells")
        .expect("fixture 143 should pass");
}

#[test]
fn fixture_084_sort_multi_stable_priority() {
    run_fixture("084-sort-multi-stable-priority")
        .expect("fixture 084 should pass");
}

#[test]
fn fixture_096_canonical_number_scientific_boundary() {
    run_fixture("096-canonical-number-scientific-boundary")
        .expect("fixture 096 should pass");
}

#[test]
fn fixture_131_inputs_with_xtl_default() {
    run_fixture("131-inputs-with-xtl-default").expect("fixture 131 should pass");
}

#[test]
fn fixture_087_date_canonical_string_concat() {
    run_fixture("087-date-canonical-string-concat")
        .expect("fixture 087 should pass");
}

#[test]
fn fixture_097_native_formula_static_cell_preserved() {
    run_fixture("097-native-formula-static-cell-preserved")
        .expect("fixture 097 should pass");
}

#[test]
fn fixture_144_block_side_cells_after_block() {
    run_fixture("144-block-side-cells-after-block")
        .expect("fixture 144 should pass");
}

#[test]
fn fixture_147_multi_block_different_sources() {
    run_fixture("147-multi-block-different-sources")
        .expect("fixture 147 should pass");
}

#[test]
fn fixture_154_multi_block_per_block_filter() {
    run_fixture("154-multi-block-per-block-filter")
        .expect("fixture 154 should pass");
}


#[test]
fn fixture_005_round_half_away_from_zero() {
    run_fixture("005-round-half-away-from-zero").expect("fixture 005 should pass");
}

#[test]
fn fixture_017_source_sheet_prefix_no_match_error() {
    run_fixture("017-source-sheet-prefix-no-match-error")
        .expect("fixture 017 should surface the expected error message");
}

#[test]
fn fixture_023_today_utc_dynamic() {
    run_fixture("023-today-utc-dynamic")
        .expect("fixture 023 should produce today's UTC date in the dynamic cell");
}

// fixture 006 (filename-forbidden-chars) exercises both filename
// sanitisation (done — see render::sanitize_filename) and per-file
// rendering with `output_file_pattern` group keys injected into the
// static ctx (pending — needs the file-group splitter). Re-enable once
// the file-group splitter lands.

/// Walk every fixture, classify pass / fail / skip, and print a summary.
/// Always passes — purely informational. The targeted `fixture_NNN_*`
/// tests above are the ones that gate the build; this one is what we use
/// to spot the next set of fixtures to wire in.
///
/// Run with `cargo test -p xl3-core --tests fixture_corpus_overview -- --nocapture`
/// to see the breakdown.
#[test]
fn fixture_corpus_overview() {
    let Some(root) = conformance_root() else {
        eprintln!("[skip] xl3 conformance corpus not found");
        return;
    };
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .expect("read fixtures dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut skip_no_expected = 0usize;
    let mut error_pass = 0usize;
    let mut error_fail = 0usize;
    let mut dynamic_pass = 0usize;
    let mut dynamic_fail = 0usize;
    let mut multifile_pass = 0usize;
    let mut multifile_fail = 0usize;
    let mut fail_examples: Vec<(String, String)> = Vec::new();

    for name in &names {
        let dir = root.join(name);
        let template = dir.join("template.xlsx");
        let data = dir.join("data.xlsx");
        let expected = dir.join("expected.xlsx");

        if !template.exists() || !data.exists() {
            skip_no_expected += 1;
            continue;
        }
        let meta = std::fs::read_to_string(dir.join("meta.yaml")).unwrap_or_default();
        // The non-cell-comparison fixture shapes (error / dynamic /
        // multi-output) get their own buckets — run_fixture knows how
        // to evaluate each.
        if !expected.exists() {
            if meta_field(&meta, "expected_error").is_some() {
                match run_fixture(name) {
                    Ok(()) => error_pass += 1,
                    Err(_) => error_fail += 1,
                }
                continue;
            }
            if meta_field(&meta, "expected_dynamic").is_some() {
                match run_fixture(name) {
                    Ok(()) => dynamic_pass += 1,
                    Err(_) => dynamic_fail += 1,
                }
                continue;
            }
            if dir.join("expected").is_dir() {
                match run_fixture(name) {
                    Ok(()) => multifile_pass += 1,
                    Err(_) => multifile_fail += 1,
                }
                continue;
            }
            skip_no_expected += 1;
            continue;
        }
        // Stage 2 fixtures are out of scope for cell-value comparison.
        if meta
            .lines()
            .any(|l| l.trim().starts_with("comparison_stage:") && l.contains('2'))
        {
            skip_no_expected += 1;
            continue;
        }
        let inputs = parse_meta_inputs(&meta);
        match xl3_core::render::render_from_paths_with_inputs(&template, &data, &inputs) {
            Ok(bytes) => match compare_workbooks_stage1(&bytes, &expected) {
                Ok(()) => pass += 1,
                Err(e) => {
                    fail += 1;
                    if fail_examples.len() < 5 {
                        fail_examples.push((name.clone(), format!("{e}")));
                    }
                }
            },
            Err(e) => {
                fail += 1;
                if fail_examples.len() < 5 {
                    fail_examples.push((name.clone(), format!("render: {e}")));
                }
            }
        }
    }

    eprintln!(
        "conformance corpus: {} fixtures, pass={pass} fail={fail} skip(no expected)={skip_no_expected}",
        names.len()
    );
    eprintln!(
        "  error fixtures:     pass={error_pass} fail={error_fail}"
    );
    eprintln!(
        "  dynamic fixtures:   pass={dynamic_pass} fail={dynamic_fail}"
    );
    eprintln!(
        "  multi-file fixtures: pass={multifile_pass} fail={multifile_fail}"
    );
    if !fail_examples.is_empty() {
        eprintln!("first failures:");
        for (n, e) in &fail_examples {
            let trimmed: String = e.chars().take(180).collect();
            eprintln!("  - {n}: {trimmed}");
        }
    }
}

#[test]
fn fixture_failure_taxonomy() {
    let Some(root) = conformance_root() else {
        eprintln!("[skip] xl3 conformance corpus not found");
        return;
    };
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .expect("read fixtures dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    let mut buckets: std::collections::BTreeMap<&'static str, Vec<String>> =
        std::collections::BTreeMap::new();
    for name in &names {
        let dir = root.join(name);
        let template = dir.join("template.xlsx");
        let data = dir.join("data.xlsx");
        let expected = dir.join("expected.xlsx");
        if !template.exists() || !data.exists() || !expected.exists() {
            continue;
        }
        let meta = std::fs::read_to_string(dir.join("meta.yaml")).unwrap_or_default();
        if meta
            .lines()
            .any(|l| l.trim().starts_with("comparison_stage:") && l.contains('2'))
        {
            continue;
        }
        let inputs = parse_meta_inputs(&meta);
        match xl3_core::render::render_from_paths_with_inputs(&template, &data, &inputs) {
            Ok(bytes) => match compare_workbooks_stage1(&bytes, &expected) {
                Ok(()) => {}
                Err(e) => {
                    let msg = format!("{e}");
                    let bucket = if msg.contains("row count mismatch") {
                        "row-count-mismatch"
                    } else if msg.contains("sheet count mismatch") {
                        "sheet-count-mismatch"
                    } else if msg.contains("col count mismatch") {
                        "col-count-mismatch"
                    } else if msg.contains("cell") && msg.contains("mismatch") {
                        "cell-value-mismatch"
                    } else {
                        "compare-other"
                    };
                    buckets.entry(bucket).or_default().push(name.clone());
                }
            },
            Err(e) => {
                let msg = format!("{e}");
                let bucket = if msg.contains("'@'") || msg.contains("@filter") || msg.contains("@repeat") {
                    "directive-at"
                } else if msg.contains("Source[") || msg.contains("source") {
                    "source-or-cross-source"
                } else if msg.contains("unknown function") {
                    let fname = msg
                        .split("unknown function ")
                        .nth(1)
                        .map(|s| s.split_whitespace().next().unwrap_or(""))
                        .unwrap_or("");
                    eprintln!("  [unknown-function] {name}: {fname}");
                    "unknown-function"
                } else if msg.contains("Source[") || msg.contains("source") {
                    "source-or-cross-source"
                } else if msg.contains("unexpected character") {
                    "lex-other"
                } else if msg.contains("unsupported expression") {
                    "unsupported-expression"
                } else if msg.contains("xl3/") {
                    "xtl-error-propagated"
                } else {
                    "render-other"
                };
                buckets.entry(bucket).or_default().push(name.clone());
            }
        }
    }

    eprintln!("\nfailure taxonomy (top 5 in each bucket):");
    for (bucket, fixtures) in &buckets {
        eprintln!("  {} ({}):", bucket, fixtures.len());
        for n in fixtures.iter().take(5) {
            eprintln!("    {n}");
        }
    }
}
