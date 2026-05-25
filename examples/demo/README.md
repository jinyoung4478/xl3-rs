# xl3-rs browser demo

Single-page demo that runs the three benchmark scenarios from
`crates/xl3-core/examples/bench.rs` (wide-flat / multi-sheet /
multi-source-join) through the WASM artifact, in a Web Worker, so
the main thread stays responsive. PLAN.md §5 Phase 3 Task 3.1.

The page reads the wasm bundle straight from
`../../crates/xl3-wasm/pkg/xl3_wasm.js` (the `--target web` output of
`wasm-pack build`), so the workflow is:

```bash
# 1. Build the wasm bundle once.
cd crates/xl3-wasm
wasm-pack build --release --target web

# 2. Serve the repo root with any static file server. (file://
#    can't load .wasm modules; you need a real HTTP origin.)
cd ../..
python3 -m http.server 8000
# or `npx serve .` etc.

# 3. Open http://localhost:8000/examples/demo/ and click a Run
#    button. Each scenario builds its template + data workbook
#    procedurally in the worker, then times three render() calls
#    and reports the median.
```

## What this demonstrates

- **Web Worker isolation** — Phase 2 Task 2.3 / PLAN.md §6: the WASM
  call never touches the main thread, so the UI stays interactive
  even during the multi-source-join scenario.
- **No bundler required** — `import init, { convert } from
  'xl3-wasm'` works out of the box with the `--target web` build.
  Production hosts using Vite / webpack / esbuild get the same shape
  via a normal package install (`npm install xl3-wasm`).
- **Cross-impl reference numbers** — the labels under each result
  carry xl3 (TS)'s baseline so the relative speedup is obvious at a
  glance.

The demo does NOT depend on exceljs or the conformance corpus. The
worker writes a minimum-viable XLSX (store-only zip, no styles)
directly so the timed loop measures the renderer rather than the
workbook generator.
