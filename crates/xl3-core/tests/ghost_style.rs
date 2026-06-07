//! ADR-0066 ghost-style regression — issue #2 (upstream xl3 0.8.1,
//! commit jinyoung4478/xl3@174bba1).
//!
//! The JS engine's splice-then-restore pass moved outside-block
//! ("side") cells back to their original rows after an expansion
//! splice, but cleared only the VALUE at the shifted position — the
//! style (borders, fills) stayed behind. Large expansions rendered an
//! empty, fully-inked ghost copy of the side summary block below the
//! data (production case: 348-row settlement sheet, 10-row ghost).
//!
//! The wasm core composes output rows directly instead
//! (`render.rs::compose_iteration_cells`): a cell's style index
//! travels WITH the cell through composition and `CellSource::Empty`
//! never carries one, so the ghost is impossible by construction.
//! These tests pin that property so a future writer or composition
//! change can't reintroduce it.
//!
//! Property (mirrors upstream's
//! `renderer-outside-block-ghost-style.test.ts`): after rendering a
//! template with a side summary block next to a large expansion, no
//! cell in the side columns may carry ink (fill / border) without a
//! value. Covers the plain and the `@group`/`@subtotal` render paths.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::events::{BytesStart, Event};
use xl3_core::calamine::{Data, Reader, Xlsx};
use xl3_core::rust_xlsxwriter::Workbook;
use xl3_core::{
    render_from_bytes_to_files_full, FillPattern, FillSpec, StyleManifest, StyleSpec, Value,
};

/// Side summary columns P / Q (0-based), mirroring the upstream test.
const SIDE_COLS: [u32; 2] = [15, 16];

// ---- template / data builders --------------------------------------

fn add_config(wb: &mut Workbook) {
    let ws = wb.add_worksheet();
    ws.set_name("__config__").unwrap();
    ws.write_string(0, 0, "key").unwrap();
    ws.write_string(0, 1, "value").unwrap();
    ws.write_string(1, 0, "source_sheet").unwrap();
    ws.write_string(1, 1, "Raw").unwrap();
    ws.write_string(2, 0, "source_table").unwrap();
    ws.write_string(2, 1, "1").unwrap();
}

/// Headers + a column-scoped A:B data block, with a side summary
/// block BELOW the block's template row. The side cells reference
/// `__inputs__` (not the source row) so they stay side rows in the
/// planner AND get a manifest style index stamped — `CellSource::
/// Literal` doesn't carry one, so a literal-only side block would
/// make the ghost check vacuous.
fn plain_template() -> Vec<u8> {
    let mut wb = Workbook::new();
    add_config(&mut wb);
    let ws = wb.add_worksheet();
    ws.set_name("Main").unwrap();
    ws.write_string(2, 0, "a").unwrap();
    ws.write_string(2, 1, "b").unwrap();
    ws.write_string(3, 0, "{{ [a] }}").unwrap();
    ws.write_string(3, 1, "{{ [b] }}").unwrap();
    // Side summary (template rows 5-6, cols P/Q).
    ws.write_string(4, 15, "{{ __inputs__[label] }}").unwrap();
    ws.write_string(4, 16, "{{ __inputs__[total] }}").unwrap();
    ws.write_string(5, 15, "TAX").unwrap();
    ws.write_number(5, 16, 550.0).unwrap();
    wb.save_to_buffer().unwrap()
}

/// Same shape with `@sort` + `@group` directives and a `@subtotal`
/// row, exercising the grouped render path.
fn grouped_template() -> Vec<u8> {
    let mut wb = Workbook::new();
    add_config(&mut wb);
    let ws = wb.add_worksheet();
    ws.set_name("Main").unwrap();
    ws.write_string(0, 0, "a").unwrap();
    ws.write_string(0, 1, "b").unwrap();
    ws.write_string(1, 0, "{{ @sort [a] }}").unwrap();
    ws.write_string(2, 0, "{{ @group [a] }}").unwrap();
    ws.write_string(3, 0, "{{ [a] }}").unwrap();
    ws.write_string(3, 1, "{{ [b] }}").unwrap();
    ws.write_string(4, 0, "Subtotal").unwrap();
    ws.write_string(4, 1, "{{ @subtotal SUM([b]) }}").unwrap();
    // Side summary below the block (template row 6, cols P/Q).
    ws.write_string(5, 15, "{{ __inputs__[label] }}").unwrap();
    ws.write_string(5, 16, "{{ __inputs__[total] }}").unwrap();
    wb.save_to_buffer().unwrap()
}

fn data_workbook(n: usize) -> Vec<u8> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Raw").unwrap();
    ws.write_string(0, 0, "a").unwrap();
    ws.write_string(0, 1, "b").unwrap();
    for i in 0..n {
        let group = if i % 2 == 0 { "group-1" } else { "group-2" };
        ws.write_string((i + 1) as u32, 0, group).unwrap();
        ws.write_number((i + 1) as u32, 1, (100 * (i + 1)) as f64)
            .unwrap();
    }
    wb.save_to_buffer().unwrap()
}

/// Manifest with one solid-fill style stamped on the given template
/// cells of sheet "Main" — the "ink" the ghost check hunts for.
fn side_fill_manifest(side_cells: &[(u32, u32)]) -> StyleManifest {
    let mut main: HashMap<(u32, u32), usize> = HashMap::new();
    for &rc in side_cells {
        main.insert(rc, 0);
    }
    StyleManifest {
        styles: vec![StyleSpec {
            fill: Some(FillSpec {
                pattern: FillPattern::Solid,
                color: "FFDDEBF7".to_string(),
            }),
            ..Default::default()
        }],
        cells: HashMap::from([("Main".to_string(), main)]),
        ..Default::default()
    }
}

fn render(template: Vec<u8>, manifest: StyleManifest, n_rows: usize) -> Vec<u8> {
    let inputs = HashMap::from([
        ("label".to_string(), Value::String("TOTAL".to_string())),
        ("total".to_string(), Value::Number(5500.0)),
    ]);
    let files =
        render_from_bytes_to_files_full(&template, data_workbook(n_rows), &inputs, Some(manifest))
            .expect("render");
    assert_eq!(files.len(), 1, "expected a single output file");
    files.into_iter().next().unwrap().data
}

// ---- output inspection ----------------------------------------------

fn read_zip_entry(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).expect("open output zip");
    let mut file = archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("zip entry {name} missing"));
    let mut s = String::new();
    file.read_to_string(&mut s).expect("read zip entry");
    s
}

fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

/// Per-xf "carries visible ink" flags from the output's styles.xml:
/// a patterned fill that isn't `none`, or any border edge with a
/// style. Indexed by cellXfs position (= the `s` attribute on cells).
fn inked_xfs(styles_xml: &str) -> Vec<bool> {
    let mut fills: Vec<bool> = Vec::new();
    let mut borders: Vec<bool> = Vec::new();
    let mut xfs: Vec<(usize, usize)> = Vec::new(); // (fillId, borderId)

    let mut in_fills = false;
    let mut in_borders = false;
    let mut in_cell_xfs = false;
    let mut in_border = false;

    let mut reader = quick_xml::Reader::from_str(styles_xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf).expect("parse styles.xml");
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let is_empty = matches!(&event, Event::Empty(_));
                match e.name().as_ref() {
                    b"fills" => in_fills = true,
                    b"borders" => in_borders = true,
                    b"cellXfs" => in_cell_xfs = true,
                    b"fill" if in_fills => fills.push(false),
                    b"patternFill" if in_fills => {
                        let inked = attr(e, b"patternType").map(|p| p != "none").unwrap_or(false);
                        if let Some(last) = fills.last_mut() {
                            *last = *last || inked;
                        }
                    }
                    b"border" if in_borders => {
                        borders.push(false);
                        in_border = !is_empty;
                    }
                    b"left" | b"right" | b"top" | b"bottom" | b"diagonal" if in_border => {
                        if attr(e, b"style").is_some() {
                            if let Some(last) = borders.last_mut() {
                                *last = true;
                            }
                        }
                    }
                    b"xf" if in_cell_xfs => {
                        let id = |k: &[u8]| {
                            attr(e, k).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0)
                        };
                        xfs.push((id(b"fillId"), id(b"borderId")));
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"fills" => in_fills = false,
                b"borders" => in_borders = false,
                b"cellXfs" => in_cell_xfs = false,
                b"border" => in_border = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    xfs.iter()
        .map(|&(f, b)| {
            fills.get(f).copied().unwrap_or(false) || borders.get(b).copied().unwrap_or(false)
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct OutCell {
    has_value: bool,
    inked: bool,
}

/// Every `<c>` element of the output's (single) worksheet, keyed by
/// 0-based (row, col): does it hold a value, and does its xf carry
/// ink? Blank-but-styled cells appear here too — exactly the shape
/// a ghost would take.
fn xml_cells(output: &[u8]) -> HashMap<(u32, u32), OutCell> {
    let inked = inked_xfs(&read_zip_entry(output, "xl/styles.xml"));
    let sheet_xml = read_zip_entry(output, "xl/worksheets/sheet1.xml");

    let mut out: HashMap<(u32, u32), OutCell> = HashMap::new();
    let mut current: Option<(u32, u32)> = None;
    let mut reader = quick_xml::Reader::from_str(&sheet_xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf).expect("parse sheet xml");
        match &event {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"c" => {
                    let r = attr(e, b"r").expect("cell ref");
                    let pos = parse_a1(&r).expect("a1 ref");
                    let cell_inked = attr(e, b"s")
                        .and_then(|s| s.parse::<usize>().ok())
                        .map(|i| inked.get(i).copied().unwrap_or(false))
                        .unwrap_or(false);
                    out.insert(
                        pos,
                        OutCell {
                            has_value: false,
                            inked: cell_inked,
                        },
                    );
                    current = if matches!(&event, Event::Start(_)) {
                        Some(pos)
                    } else {
                        None
                    };
                }
                b"v" | b"is" => {
                    if let Some(pos) = current {
                        if let Some(cell) = out.get_mut(&pos) {
                            cell.has_value = true;
                        }
                    }
                }
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == b"c" => current = None,
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// `B3` → 0-based (row, col).
fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut col: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        col = col * 26 + (bytes[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
    }
    if col == 0 || i == 0 || i == bytes.len() {
        return None;
    }
    let row: u32 = std::str::from_utf8(&bytes[i..]).ok()?.parse().ok()?;
    Some((row - 1, col - 1))
}

/// Non-empty output cell values via calamine, keyed by absolute
/// 0-based (row, col).
fn read_main_cells(bytes: &[u8]) -> HashMap<(u32, u32), Data> {
    let mut wb: Xlsx<_> = Xlsx::new(Cursor::new(bytes.to_vec())).expect("open output workbook");
    let range = wb.worksheet_range("Main").expect("Main sheet in output");
    let (start_r, start_c) = range.start().unwrap_or((0, 0));
    let (rows, cols) = range.get_size();
    let mut out = HashMap::new();
    for r in 0..rows {
        for c in 0..cols {
            if let Some(d) = range.get((r, c)) {
                if !matches!(d, Data::Empty) {
                    out.insert((start_r + r as u32, start_c + c as u32), d.clone());
                }
            }
        }
    }
    out
}

/// The upstream property: no cell in the side columns may carry ink
/// without a value.
fn assert_no_side_ghosts(output: &[u8]) {
    let ghosts: Vec<String> = xml_cells(output)
        .iter()
        .filter(|((_, col), cell)| SIDE_COLS.contains(col) && cell.inked && !cell.has_value)
        .map(|((row, col), _)| format!("(r{row}, c{col})"))
        .collect();
    assert!(
        ghosts.is_empty(),
        "style-only ghost cells in side columns: {ghosts:?}"
    );
}

// ---- tests -----------------------------------------------------------

#[test]
fn plain_expansion_leaves_no_side_ghost() {
    let output = render(
        plain_template(),
        side_fill_manifest(&[(4, 15), (4, 16), (5, 15), (5, 16)]),
        10,
    );
    assert_no_side_ghosts(&output);

    // Non-vacuity: the side summary survives exactly once, with value
    // AND ink together — otherwise the ghost check passes trivially
    // because no side cell got styled at all.
    let values = read_main_cells(&output);
    let totals: Vec<(u32, u32)> = values
        .iter()
        .filter(|(_, d)| matches!(d, Data::String(s) if s == "TOTAL"))
        .map(|(pos, _)| *pos)
        .collect();
    assert_eq!(totals.len(), 1, "TOTAL must land exactly once: {totals:?}");
    let (row, col) = totals[0];
    assert_eq!(col, 15, "TOTAL must stay in column P");
    assert!(
        matches!(values.get(&(row, 16)), Some(Data::Float(f)) if *f == 5500.0),
        "companion 5500 must sit next to TOTAL"
    );
    let cells = xml_cells(&output);
    assert!(
        cells.get(&(row, 15)).is_some_and(|c| c.inked && c.has_value),
        "the restored TOTAL cell must keep its fill"
    );
}

#[test]
fn grouped_expansion_leaves_no_side_ghost() {
    let output = render(
        grouped_template(),
        side_fill_manifest(&[(5, 15), (5, 16)]),
        10,
    );
    assert_no_side_ghosts(&output);

    // Non-vacuity on the grouped path: TOTAL must survive valued AND
    // inked — wherever it lands, it is never a style-only ghost.
    //
    // KNOWN GAP (out of scope here): the grouped leaf composes
    // `side_rows` against the per-group iteration index
    // (`render_grouped` in render.rs), so the side summary repeats
    // once per group — the JS engine restores it exactly once at its
    // original row. That's a value-layer parity gap, tracked
    // separately from this style-ghost regression; we deliberately
    // don't pin a count here.
    let values = read_main_cells(&output);
    let totals: Vec<(u32, u32)> = values
        .iter()
        .filter(|(_, d)| matches!(d, Data::String(s) if s == "TOTAL"))
        .map(|(pos, _)| *pos)
        .collect();
    assert!(!totals.is_empty(), "TOTAL must survive the grouped render");
    let cells = xml_cells(&output);
    for (row, col) in totals {
        assert_eq!(col, 15, "TOTAL must stay in column P");
        assert!(
            matches!(values.get(&(row, 16)), Some(Data::Float(f)) if *f == 5500.0),
            "companion 5500 must sit next to TOTAL at row {row}"
        );
        assert!(
            cells.get(&(row, 15)).is_some_and(|c| c.inked && c.has_value),
            "the TOTAL cell at row {row} must keep its fill"
        );
    }
}

/// Production-scale variant of the plain path: the original ghost
/// only became visible on large expansions (348 data rows left a
/// 10-row ghost). Composition is size-independent, but pin it anyway.
#[test]
fn large_expansion_leaves_no_side_ghost() {
    let output = render(
        plain_template(),
        side_fill_manifest(&[(4, 15), (4, 16), (5, 15), (5, 16)]),
        348,
    );
    assert_no_side_ghosts(&output);
}
