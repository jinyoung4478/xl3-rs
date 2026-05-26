# xl3-core CHANGELOG

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
