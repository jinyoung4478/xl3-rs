# xl3-wasm

WebAssembly wrapper around [`xl3-core`](https://crates.io/crates/xl3-core)
— the pure-Rust XLSX template rendering engine — packaged for browser
and Node hosts. Used by the [TypeScript engine](https://www.npmjs.com/package/@jinyoung4478/xl3)
as an optional acceleration path.

## Install

```bash
npm install xl3-wasm
# or
npm install xl3-wasm @jinyoung4478/xl3
```

When `xl3-wasm` is present alongside `@jinyoung4478/xl3`, the engine
auto-detects the acceleration path; otherwise it falls back to the
ExcelJS implementation.

## KPI

| Workload | TS + ExcelJS | xl3-wasm (warm) |
|---|---:|---:|
| 36k-row multi-sheet report | 2.5 s | ~0.3 s |
| 70 MB / 6 M cells round-trip | 67 s | ~5.8 s |

(May 2026, Node 22 / Apple Silicon. Browser timings track Node closely
on V8.)

## Surface

```ts
import init, { convert, preview, readTemplateInputs } from 'xl3-wasm';

await init();  // browsers: auto-fetch the .wasm; Node hosts: see below

const out = convert(templateBytes, sourceBytes, /* inputs */ {}, /* manifest */ undefined);
// out: [{ filename, data: Uint8Array, warnings: [{ message }] }]
```

In Node, the web-target init can't fetch a `file://` URL — read the
.wasm bytes and pass them in:

```ts
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import init from 'xl3-wasm';

const require = createRequire(import.meta.url);
const wasmPath = require.resolve('xl3-wasm').replace(/xl3_wasm\.js$/, 'xl3_wasm_bg.wasm');
await init({ module_or_path: await readFile(wasmPath) });
```

`@jinyoung4478/xl3` handles both paths internally; consumers driving
the TS engine don't need this snippet.

## Errors

The Rust core surfaces structured errors as `[xl3/<ns>/<name>] message`.
The TS engine lifts the bracketed prefix onto the thrown JS `Error`'s
`.code` property; hosts driving `xl3-wasm` directly can do the same:

```ts
try {
  convert(...);
} catch (e) {
  const m = /^\[(xl3\/[^\]]+)\]\s*(.*)$/.exec(e.message);
  if (m) (e as any).code = m[1];
  throw e;
}
```

## License

MIT.

## Related

- [`@jinyoung4478/xl3`](https://www.npmjs.com/package/@jinyoung4478/xl3)
  — TypeScript engine (template parser + acceleration shell)
- [`xl3-core`](https://crates.io/crates/xl3-core) — pure-Rust engine,
  for native Rust / Tauri / PyO3 hosts
- [Repository](https://github.com/jinyoung4478/xl3-rs)
