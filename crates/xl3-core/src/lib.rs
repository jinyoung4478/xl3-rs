//! xl3-core — pure-Rust XLSX template rendering engine.
//!
//! Phase 1 milestone P1-A: only the minimum needed to pass
//! `conformance/fixtures/001-bracket-substitution`. The pipeline runs
//! in three steps:
//!
//!   1. `plan`   — parse the template workbook + `__config__` sheet into a
//!                 `WorkbookPlan` of static and expansion rows.
//!   2. `source` — read the data workbook into row records.
//!   3. `render` — walk the plan, evaluating each cell through `eval`,
//!                 emitting cells through `output`.
//!
//! Module skeletons (manifest preservation, full XTL evaluator, multi-source
//! support, etc.) will grow as later fixtures are wired in. See `PLAN.md`
//! §5 Phase 1 for the broader roadmap.

pub mod directives;
pub mod errors;
pub mod eval;
pub mod functions;
pub mod introspect;
pub mod output;
pub mod output_model;
pub mod plan;
pub mod render;
pub mod source;
pub mod styles;
pub mod value;

pub use calamine;
pub use rust_xlsxwriter;

pub use errors::{is_xtl_error, XtlError};
pub use introspect::{
    preview, read_template_inputs, InputKind, InputSpec, PreviewFile, PreviewResult,
    PreviewSheet, PreviewSource,
};
pub use output_model::{OutputFile, XtlWarning};
pub use plan::{CellSource, RowPlan, SheetPlan, WorkbookPlan};
pub use render::{render, render_to_files};
pub use source::{CalamineSourceReader, SourceData, SourceReader};
pub use value::Value;
