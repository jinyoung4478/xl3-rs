#!/usr/bin/env node
// Memory stability test: call wasm.roundtrip(buf) 100 times and watch RSS.
//
// Goal (PLAN.md §6 risk): no monotonic RSS growth across iterations that
// would indicate a leak in the calamine + rust_xlsxwriter pipeline or
// in the JS↔WASM ArrayBuffer marshalling.
//
// Usage:
//   node --expose-gc scripts/memory-stability.mjs <input.xlsx> [--runs=N]

import { performance } from 'node:perf_hooks';
import { readFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');

const args = process.argv.slice(2);
const positional = args.filter((a) => !a.startsWith('--'));
const flags = Object.fromEntries(
  args.filter((a) => a.startsWith('--')).map((a) => {
    const [k, v] = a.replace(/^--/, '').split('=');
    return [k, v ?? true];
  }),
);

if (positional.length < 1) {
  console.error('usage: memory-stability.mjs <input.xlsx> [--runs=N]');
  process.exit(1);
}

const input = positional[0];
const runs = Number(flags.runs ?? 100);

// ---- init wasm (web-target pkg → load bytes manually in Node) ----
const wasmJs = resolve(repoRoot, 'crates/xl3-wasm/pkg/xl3_wasm.js');
const wasmBg = resolve(repoRoot, 'crates/xl3-wasm/pkg/xl3_wasm_bg.wasm');
const wasm = await import(wasmJs);
if (typeof wasm.default === 'function') {
  const bytes = await readFile(wasmBg);
  await wasm.default({ module_or_path: bytes });
}

const buf = await readFile(input);
console.error(`input  : ${input}  (${(buf.length / 1024).toFixed(1)} KB)`);
console.error(`runs   : ${runs}`);
console.error(`node   : ${process.version}`);
console.error('---');

const rssMb = () => process.memoryUsage.rss() / (1024 * 1024);
// wasm-bindgen exposes the WebAssembly.Memory under a private key; we
// access it through the generated glue to split JS heap vs wasm linear
// memory contributions to RSS.
const wasmMemMb = () => {
  // The web-target glue stores the instance as `wasm` (module-private).
  // Look it up via the exported initSync / __wbindgen_exports if possible;
  // otherwise the import object retains a reference.
  const w = wasm.__wbindgen_export_0 ?? wasm.memory;
  if (w && w.buffer) return w.buffer.byteLength / (1024 * 1024);
  return -1;
};

// Warmup so V8 tier-up + wasm linear-memory growth settle before sampling.
for (let i = 0; i < 3; i++) wasm.roundtrip(buf);
if (global.gc) global.gc();

const baselineRss = rssMb();
const sampleEvery = Math.max(1, Math.floor(runs / 10));
const samples = [];

const tStart = performance.now();
for (let i = 1; i <= runs; i++) {
  const out = wasm.roundtrip(buf);
  if (out.length === 0) throw new Error('empty output');
  // Force GC every iteration so V8-side noise doesn't pollute the
  // measurement of wasm linear memory growth.
  if (global.gc) global.gc();
  if (i % sampleEvery === 0 || i === runs) {
    const rss = rssMb();
    const heap = process.memoryUsage().heapUsed / (1024 * 1024);
    const ext = process.memoryUsage().external / (1024 * 1024);
    const arr = process.memoryUsage().arrayBuffers / (1024 * 1024);
    samples.push({ iter: i, rssMb: rss, deltaMb: rss - baselineRss });
    console.error(
      `iter ${String(i).padStart(4)}: rss=${rss.toFixed(1)}  heap=${heap.toFixed(1)}  ext=${ext.toFixed(1)}  arrBuf=${arr.toFixed(1)}  Δrss=${(rss - baselineRss).toFixed(1)} MB`,
    );
  }
}
const totalMs = performance.now() - tStart;

console.error('---');
console.error(`baseline rss   : ${baselineRss.toFixed(1)} MB (after 3 warmup)`);
const last = samples[samples.length - 1];
console.error(`final rss      : ${last.rssMb.toFixed(1)} MB`);
console.error(`growth         : ${last.deltaMb.toFixed(1)} MB over ${runs} iterations`);
console.error(`per-iteration  : ${(last.deltaMb / runs * 1024).toFixed(1)} KB/iter`);
console.error(`total time     : ${(totalMs / 1000).toFixed(2)} s  (mean ${(totalMs / runs).toFixed(1)} ms/iter)`);

// Verdict: < 1 MB total growth over 100 iters is well within noise.
const verdict = last.deltaMb < 1 ? 'STABLE' : last.deltaMb < 5 ? 'BORDERLINE' : 'LEAK SUSPECTED';
console.error(`verdict        : ${verdict}`);
