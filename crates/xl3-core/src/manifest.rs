//! Style manifest model exchanged with the JS shell.
//!
//! Phase 2 Task 2.2 (PLAN.md §5): exceljs in xl3 (TS) already
//! parses every OOXML cell style; rather than ask `xl3-core` to
//! re-parse styles.xml in depth, the TS side ships a normalised
//! JSON shape and we apply it on the output.
//!
//! Boundary discipline (CLAUDE.md): this module is plain Rust with
//! no `wasm_bindgen` / `JsValue` dependency. The JSON decoder lives
//! in `xl3-wasm`, which constructs a `StyleManifest` and hands it to
//! the renderer.

use std::collections::HashMap;

/// One-shot bundle of all the styling information `xl3-core` needs
/// to reproduce an exceljs-parsed workbook's look.
#[derive(Debug, Default, Clone)]
pub struct StyleManifest {
    /// Deduplicated style table. Cells reference entries by index;
    /// matches OOXML `cellXfs` semantics.
    pub styles: Vec<StyleSpec>,
    /// Per-sheet, per-template-cell style index. Keys are sheet
    /// names; inner keys are zero-based `(row, col)` matching the
    /// planner's position math.
    pub cells: HashMap<String, HashMap<(u32, u32), usize>>,
    /// Merge ranges per sheet, A1 form (`"A1:B2"`). Applied to the
    /// rendered worksheet before saving.
    pub merges: HashMap<String, Vec<String>>,
    /// Per-sheet column widths in OOXML cw units. Only the
    /// non-default columns are listed; the rest inherit
    /// rust_xlsxwriter's defaults.
    pub columns: HashMap<String, Vec<ColumnWidth>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ColumnWidth {
    pub col: u32,
    pub width: f64,
}

#[derive(Debug, Default, Clone)]
pub struct StyleSpec {
    pub font: Option<FontSpec>,
    pub num_fmt: Option<String>,
    pub alignment: Option<AlignmentSpec>,
    pub fill: Option<FillSpec>,
}

#[derive(Debug, Default, Clone)]
pub struct FontSpec {
    pub name: Option<String>,
    pub size: Option<f64>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// ARGB hex (`"FF000000"`), `None` for the theme default.
    pub color: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AlignmentSpec {
    pub horizontal: Option<HorizontalAlign>,
    pub vertical: Option<VerticalAlign>,
    pub wrap_text: bool,
    pub indent: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalAlign {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalAlign {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone)]
pub struct FillSpec {
    /// Pattern type. Phase 2 supports `Solid` only — other patterns
    /// (gray125, etc.) are deferred along with conditional formatting.
    pub pattern: FillPattern,
    /// ARGB hex (`"FFFFFF00"` for yellow).
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillPattern {
    Solid,
}

impl StyleManifest {
    /// Look up a template cell's style index. `None` when the
    /// (sheet, row, col) triple wasn't in the manifest — the
    /// caller falls back to defaults.
    pub fn cell_style(&self, sheet: &str, row: u32, col: u32) -> Option<&StyleSpec> {
        let idx = *self.cells.get(sheet)?.get(&(row, col))?;
        self.styles.get(idx)
    }

    pub fn sheet_merges(&self, sheet: &str) -> &[String] {
        self.merges
            .get(sheet)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn sheet_columns(&self, sheet: &str) -> &[ColumnWidth] {
        self.columns
            .get(sheet)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}
