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
    let expected = dir.join("expected.xlsx");

    if !expected.exists() {
        return Err(anyhow!(
            "fixture {fixture_name} has no expected.xlsx — error/dynamic fixtures not yet supported by the Rust runner"
        ));
    }

    let meta_yaml = std::fs::read_to_string(dir.join("meta.yaml")).unwrap_or_default();
    let host_inputs = parse_meta_inputs(&meta_yaml);
    let actual_bytes = render_from_paths_with_inputs(&template, &data, &host_inputs)
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
fn fixture_005_round_half_away_from_zero() {
    run_fixture("005-round-half-away-from-zero").expect("fixture 005 should pass");
}

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
        if !expected.exists() {
            // Error / dynamic / multi-output fixtures: skip for now.
            skip_no_expected += 1;
            continue;
        }
        match xl3_core::render::render_from_paths(&template, &data) {
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
        match xl3_core::render::render_from_paths(&template, &data) {
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
