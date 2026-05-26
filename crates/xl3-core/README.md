# xl3-core

Pure-Rust XLSX template rendering engine — the acceleration core for
[xl3](https://github.com/jinyoung4478/xl3). Reads an Excel template
plus a data workbook, evaluates the [XTL](https://xl3.io) expressions
inside template cells, and emits a rendered XLSX buffer.

This crate is the standalone Rust foundation that drives:
- the npm package [`xl3-wasm`](https://www.npmjs.com/package/xl3-wasm)
  (browser / Node acceleration for the TS engine)
- native consumers (CLI tools, Tauri apps, server batch jobs, PyO3
  bindings) that want xl3 semantics without a JavaScript runtime

## Status

- **0.1**: pre-release. Public types compile but the surface is still
  evolving; expect breaking changes between minor versions.
- Conformance against the canonical TS suite: 119 / 148 fixtures pass
  (Stage 1, May 2026). Outstanding gaps are tracked in the parent
  repository's `PLAN.md`.

## Quick start

```rust
use xl3_core::{render_from_bytes_to_files, OutputFile};

let template = std::fs::read("template.xlsx")?;
let data = std::fs::read("data.xlsx")?;
let files: Vec<OutputFile> = render_from_bytes_to_files(&template, &data)?;
std::fs::write(&files[0].filename, &files[0].data)?;
```

For host applications that already extracted a style manifest (e.g.
xl3 TS via exceljs), pass it through `render_from_bytes_to_files_full`
to preserve fonts, fills, alignments, merges and column widths.

## Design

```
Template (.xlsx) ┐
                 ├──▶  plan       ──▶  render       ──▶  output (.xlsx)
Data     (.xlsx) ┘    (parse)         (eval cells)        (rust_xlsxwriter)
```

- `plan::parse_template_bytes` reads the template via `calamine`,
  classifies each cell as literal / template expression / native
  formula / `@subtotal`, and groups rows into static / expand-down /
  expand-right plans.
- `render::render_from_bytes_to_files` walks the plan, evaluates XTL
  expressions through `eval`, and writes the result through
  `rust_xlsxwriter`. Native Excel formulas (`=UPPER(A1)`, etc.) are
  preserved verbatim per ADR-0021 / ADR-0046.
- `errors::XtlError` is the stable, code-bearing error surface that
  mirrors xl3 (TS)'s `XtlError`. Hosts can branch on `.code` strings
  like `xl3/eval/arity-mismatch`.

## Non-goals

- **Not** an Excel formula calculator — XTL expressions inside `{{ }}`
  are evaluated; native cell formulas round-trip without evaluation.
- **Not** a styles editor — fonts, fills, borders, merges and so on
  arrive via the host-supplied `StyleManifest`. The core preserves
  what it receives; it does not synthesize new styles.

## License

MIT.
