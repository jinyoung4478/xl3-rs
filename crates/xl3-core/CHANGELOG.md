# xl3-core CHANGELOG

## 0.2.0 — 2026-06-07

Conformance jump: **119 → 146 / 148** xl3 stage-1 fixtures pass via
the wasm path (vs the xl3 0.9.0-rc.1 corpus). The two remaining
fixtures (063 blank-vs-value compare, 143 shared-formula markers) are
blocked on rust_xlsxwriter writer-side limitations — tracked in
[xl3-rs#1](https://github.com/jinyoung4478/xl3-rs/issues/1).

### Breaking

- `CellSource::Literal` is now a struct variant
  (`Literal { value, style_idx }`) so literal cells carry the host
  manifest's style index like `Template` / `CellFormula` already did.
  Code pattern-matching `Literal(v)` needs a one-line update. This is
  why this release is 0.2.0 rather than 0.1.1.

### Error surface — 17 new validation codes (Group A)

All Group A validation paths now throw structured `XtlError`s at the
canonical sites: `xl3/source/{missing-header, duplicate-name,
unknown-column, row-cross-block, reserved-column-name}`,
`xl3/config/invalid-source-table`, `xl3/cell/{formula-no-cache,
numfmt-coercion, row-outside-repeat}`, `xl3/eval/{operand-coercion,
unsupported-syntax}`, `xl3/xlookup/no-match`, `xl3/parser/empty-block`,
`xl3/filename/{empty, too-long, collision}`,
`xl3/inputs/missing-required`, `xl3/subtotal/outside-group`.

### Features (Group B)

- `Value::Error` — `#DIV/0!`-style error cells round-trip as
  `<c t="e">` so exceljs readers see `{ error }`.
- `Value::Hyperlink` — `HYPERLINK()` emits a real hyperlink cell via
  `write_url_with_text`.
- `Value::DateNumber` gets a default `yyyy-mm-dd` numFmt when no
  override format exists, so date cells read back as dates.
- Empty/whitespace file-group keys substitute the `(blank)`
  placeholder (ADR-0026) instead of failing filename validation.
- Zero-row source with no file-group keys renders zero output files,
  matching the JS grouper.
- `TODAY()` works on `wasm32-unknown-unknown` (routed through
  `js_sys::Date::now()`).
- `coerce_for_num_fmt` maps `Empty → ""` (eval-side half of
  blank-vs-value comparison).

### Fixes

- Planner styles.xml / style-manifest lookups now translate to
  absolute sheet coordinates; templates whose used range doesn't
  start at A1 no longer lose numFmt coercion and manifest styles.
- ADR-0066 ghost-style regression suite (`tests/ghost_style.rs`)
  pins that expanded blocks never leave style-only ghost cells in
  side columns — the row-composition design makes the upstream xl3
  0.8.1 JS bug impossible here by construction, now verified.

## 0.1.0 — 2026-05-26

First public release. The crate has been driving `xl3-wasm` since
Phase 2; the 0.1 cut formalizes the Rust surface, freezes naming for
the major entry points, and lifts crate metadata to publish-ready.

### Highlights

- **Render pipeline** — `render_from_bytes_to_files` (one-shot) and
  `render_from_bytes_to_files_full` (with host-supplied
  `StyleManifest`) are the canonical entry points.
- **Native Excel formula preservation** (ADR-0021 / ADR-0046). Static
  cell formulas (`=UPPER(A1)`) round-trip with their cached result;
  formulas inside `@repeat` expansion rows are cloned verbatim per
  iteration.
- **Manifest application** — fonts, fills, alignments, merges, column
  widths and per-cell numFmt from the host's `StyleManifest`.
- **Stable error surface** — `XtlError { code, message }` mirrors xl3
  (TS) and xl3-py. New codes emitted in 0.1: `xl3/eval/arity-mismatch`,
  `xl3/eval/operand-coercion`, `xl3/xlookup/bare-bracket`,
  `xl3/xlookup/source-mismatch`.
- **Conformance** — 119 / 148 xl3 fixtures pass via the wasm path
  (Stage 1, May 2026). Outstanding gaps tracked in `PLAN.md`.

### Re-exports

- `calamine` and `rust_xlsxwriter` are publicly re-exported so
  downstream crates don't have to chase version skew.

### Known gaps (tracked for 0.2+)

- HYPERLINK XTL function — eval returns the label only; cell
  hyperlink metadata isn't emitted yet.
- Shared formulas — calamine resolves the shared reference to its
  expanded text, but xl3 (TS) emits a `shared:Ref` marker; round-trip
  parity is pending.
- Conditional formatting, data validation, defined names — the
  `StyleManifest` schema is the next surface to grow.
