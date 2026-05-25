# xl3-rs

> A Rust + WebAssembly acceleration core for **XTL (Excel Template Language)** — taking an `.xlsx` template plus a data workbook and rendering one or more output workbooks. Sister to [`xl3`](https://github.com/jinyoung4478/xl3) (TypeScript reference) and [`xl3-py`](https://github.com/jinyoung4478/xl3-py) (Python reference); built for the browser path where TS hits memory and time walls on million-cell workbooks.

한국어 문서: [`README.ko.md`](./README.ko.md). Detailed planning lives in [`PLAN.md`](./PLAN.md) (Korean).

## Status

**Pre-1.0 / alpha.** Phase 0 (feasibility) passed; Phase 1 (the pure-Rust core) is in progress.

| Gate (PLAN.md §1, §5) | Target | Measured | |
|---|---|---|---|
| 70 MB workbook roundtrip, Rust native | 3 – 8 s | **3.23 s** | ✓ |
| WASM boundary cost (warm) | < 2× native | **1.78×** | ✓ |
| WASM bundle size | < 2 MB | **1.3 MB** | ✓ |

See [`docs/native-baseline.md`](./docs/native-baseline.md) and [`docs/wasm-boundary.md`](./docs/wasm-boundary.md) for the full Phase 0 reports.

## Why this exists

TS + `exceljs` is fine up to a few hundred thousand cells. Past that — the workloads where users actually feel friction — it cracks: a 70 MB / 6 M-cell workbook spends **67 s** in node and pushes a browser tab past 900 MB. That isn't a constant-factor problem; you can't optimize `exceljs` into "fast". xl3-rs replaces the hot path (cell read, evaluation, XLSX write, deflate) with a Rust pipeline compiled to WebAssembly. The TS shell keeps owning template preservation (the part that's small and intricate), and hands the heavy slab off via JSON manifest + ArrayBuffer.

KPI (vs. TS baseline measured in xl3):

| Workload | TS + exceljs | xl3-rs target |
|---|---:|---:|
| 36k-row multi-axis (12 sheets × formulas × CF) | 2.5 s | **0.2 – 0.4 s** |
| 70 MB / 6 M-cell roundtrip | 66.6 s | **3 – 8 s** |
| Browser-tab memory | 900 MB+ | **~100 MB packed** |

## Architecture

Hybrid + layered. The TS shell keeps template preservation (styles, conditional formatting, merges, drawings, defined names). The Rust pipeline is split into two crates so it isn't married to WebAssembly:

```
xl3 (TS)                              xl3-rs (Rust)
─────────────                         ───────────────────────────────────
template parsing (exceljs)            Layer 2: xl3-wasm
preservation manifest    ── JSON ─►   wasm-bindgen / JSON decode / buffers
extraction                            (thin, a few hundred lines, no logic)
                                                │
                                                │ plain Rust API
                                                ▼
                                      Layer 1: xl3-core
                                      calamine + evaluator + rust_xlsxwriter
                                      (zero wasm-bindgen dependency)
receive output buffer    ◄─────────── native flate2 compression
```

- **`xl3-core`** is **pure Rust**. Tauri, CLI, server, and PyO3 consumers can link against it directly later (no wasm-bindgen drag).
- **`xl3-wasm`** is the thin adapter that owns the JSON ↔ Rust types decoding and the `ArrayBuffer` ↔ `Vec<u8>` plumbing. No business logic.

## Project layout

```
xl3-rs/
├── PLAN.md                   # full planning doc (Korean)
├── README.md                 # this file
├── README.ko.md              # Korean mirror
├── Cargo.toml                # workspace root
├── crates/
│   ├── xl3-core/             # Layer 1 — pure Rust (no wasm deps)
│   │   ├── src/              # source.rs, plan.rs, eval.rs, output.rs, render.rs
│   │   └── examples/         # roundtrip.rs (Phase 0 measurement)
│   └── xl3-wasm/             # Layer 2 — wasm-bindgen wrapper
│       └── src/              # lib.rs (#[wasm_bindgen] entry points)
├── scripts/
│   └── measure-wasm.mjs      # Node V8 WASM measurement harness
└── docs/
    ├── native-baseline.md    # Phase 0 Task 0.2 report
    └── wasm-boundary.md      # Phase 0 Task 0.3 report
```

## Build & measure (current)

```bash
# Rust native roundtrip on a real workbook (Phase 0 measurement)
cargo build --release -p xl3-core --example roundtrip
./target/release/examples/roundtrip path/to/input.xlsx out/output.xlsx

# WASM build (Node target, used by the measurement script)
wasm-pack build crates/xl3-wasm --target nodejs --release

# Same roundtrip through WebAssembly, with split timings
node --expose-gc scripts/measure-wasm.mjs path/to/input.xlsx out/wasm.xlsx --runs=5
```

Phase 0 deliberately ignores style / merge / formula preservation — it's the upper bound on cell I/O. Preservation lands in Phase 1.

## Distribution (planned)

- **npm**: `@jinyoung4478/xl3-wasm` — `wasm-pack` output. `xl3` (TS) consumes it as an optional dependency and falls back to the existing `exceljs` path when WASM isn't available.
- **crates.io** (later): `xl3-core` — for Tauri / CLI / server consumers who want pure Rust.

## Conformance

The XTL spec and golden fixtures live in [`xl3`](https://github.com/jinyoung4478/xl3) — `conformance/fixtures/` — and the TS implementation is the reference. xl3-rs targets the same corpus (154 fixtures at time of writing); xl3-py already passes 148 / 148 stage-1 fixtures and is the model for tracking conformance progress.

Stage 1 (cell-value comparison) is the primary bar. Stage 2 (canonical OOXML byte comparison) is deferred along with the spec.

## License

MIT (planned, matching xl3 and xl3-py).
