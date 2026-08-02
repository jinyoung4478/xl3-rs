# xl3-core CHANGELOG

## Unreleased

Re-synced against the xl3 corpus as of upstream `7b0ce42` (2026-08-02,
169 stage-1 fixtures, up from 154). Stage 1 via `--engine=wasm`:
**160 / 169 passed · 2 failed · 7 stage-2 skipped**, against a JS
reference baseline of 162 / 169. The two failures are still 063
(blank-vs-value) and 143 (shared-formula markers) — both rust_xlsxwriter
writer-side limits.

### Fixed

- **ADR-0066 grouped side cells** (fixture 157, [xl3-rs#3]): outside-block
  cells next to a `@group`/`@subtotal` block were composed per group
  iteration, so a side summary was duplicated once per group and any
  side row past the block's last data row was dropped. `side_rows` is
  now keyed by the block's *output* offset — subtotal rows included —
  which is what pins outside cells to their original row position.
- **ADR-0073 / ADR-0046 formula-cache markers** (fixture 160): a native
  formula's cached `<v>` was read as template text. A `@subtotal` label
  formula whose cache happened to hold `{{ [Col] }} / Subtotal` demoted
  its own row to a second data-row template (upstream issue xl3#66's
  self-corruption path). Marker and directive recognition now skips
  formula cells entirely, matching the renderer.

[xl3-rs#3]: https://github.com/xl3-lang/xl3-rs/issues/3

## 0.2.0 — 2026-06-07

Conformance jump: **119 → 146 / 148** xl3 stage-1 fixtures pass via
the wasm path (vs the xl3 0.9.0-rc.1 corpus). The two remaining
fixtures (063 blank-vs-value compare, 143 shared-formula markers) are
blocked on rust_xlsxwriter writer-side limitations — tracked in
[xl3-rs#1](https://github.com/xl3-lang/xl3-rs/issues/1).

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
