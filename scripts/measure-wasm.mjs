#!/usr/bin/env node
// Phase 0 Task 0.3 — measure WASM boundary cost against the native baseline.
//
// Runs the same roundtrip the native example does, but through the
// wasm-pack nodejs build (V8 WebAssembly). Splits the cost into:
//   - module load + WASM instantiation (one-shot)
//   - input copy (JS Uint8Array → WASM linear memory)
//   - roundtrip call (calamine + rust_xlsxwriter inside WASM)
//   - output copy (WASM linear memory → JS Uint8Array)
//
// Usage:
//   node scripts/measure-wasm.mjs <input.xlsx> [output.xlsx] [--runs=N]
//
// Note: nodejs wasm-pack target uses synchronous `WebAssembly.Module` /
// `Instance`, so the first call cost includes compile + instantiate.

import { performance } from 'node:perf_hooks';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const repoRoot = resolve(__dirname, '..');

const args = process.argv.slice(2);
const positional = args.filter((a) => !a.startsWith('--'));
const flags = Object.fromEntries(
  args
    .filter((a) => a.startsWith('--'))
    .map((a) => {
      const [k, v] = a.replace(/^--/, '').split('=');
      return [k, v ?? true];
    })
);

if (positional.length < 1) {
  console.error('usage: measure-wasm.mjs <input.xlsx> [output.xlsx] [--runs=N]');
  process.exit(1);
}

const input = positional[0];
const output = positional[1] ?? resolve(repoRoot, 'out/wasm-roundtrip.xlsx');
const runs = Number(flags.runs ?? 4);

const fmt = (ms) => ms.toFixed(1).padStart(7) + ' ms';
const mb = (bytes) => (bytes / (1024 * 1024)).toFixed(1);
const rssMb = () => (process.memoryUsage.rss() / (1024 * 1024)).toFixed(0);

console.error(`input : ${input}`);
console.error(`output: ${output}`);
console.error(`runs  : ${runs}`);
console.error(`node  : ${process.version}`);
console.error('---');

// ---- Step 1: module load + WASM instantiation ----
const t_mod_start = performance.now();
const wasmModulePath = resolve(repoRoot, 'crates/xl3-wasm/pkg/xl3_wasm.js');
const wasm = await import(wasmModulePath);
if (typeof wasm.default === 'function') {
  // wasm-pack web target — in Node we feed the .wasm bytes manually
  // since fetch(file://) is unsupported. The browser path works
  // because the demo bundler / page serves the asset over http.
  const wasmBytes = await readFile(resolve(repoRoot, 'crates/xl3-wasm/pkg/xl3_wasm_bg.wasm'));
  await wasm.default({ module_or_path: wasmBytes });
}
const t_mod_ms = performance.now() - t_mod_start;
console.error(`module load+instantiate : ${fmt(t_mod_ms)}  rss=${rssMb()} MB`);

// ---- Step 2: read input file ----
const t_read_start = performance.now();
const buf = await readFile(input);
const t_read_ms = performance.now() - t_read_start;
console.error(`fs.readFile             : ${fmt(t_read_ms)}  (${mb(buf.length)} MB)  rss=${rssMb()} MB`);
console.error('---');

// ---- Step 3: roundtrip calls ----
const results = [];
for (let i = 1; i <= runs; i++) {
  // Force GC if available so each run starts from a comparable heap.
  if (global.gc) global.gc();

  const t_call_start = performance.now();
  const out = wasm.roundtrip(buf);
  const t_call_ms = performance.now() - t_call_start;
  results.push({ ms: t_call_ms, outLen: out.length });

  console.error(
    `run ${i}: roundtrip ${fmt(t_call_ms)}  out=${mb(out.length)} MB  rss=${rssMb()} MB`
  );

  if (i === 1) {
    await writeFile(output, out);
  }
}

// ---- Summary ----
const times = results.map((r) => r.ms);
const mean = times.reduce((a, b) => a + b, 0) / times.length;
const warmTimes = times.slice(1);
const warmMean = warmTimes.length
  ? warmTimes.reduce((a, b) => a + b, 0) / warmTimes.length
  : times[0];

console.error('---');
console.error(`mean (all runs)      : ${fmt(mean)}`);
console.error(`mean (warm, ex run1) : ${fmt(warmMean)}`);
console.error(`output size          : ${mb(results[0].outLen)} MB`);
console.error(`final rss            : ${rssMb()} MB`);
