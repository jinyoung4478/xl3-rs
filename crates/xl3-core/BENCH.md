# xl3-rs performance baseline

`cargo run --release -p xl3-core --example bench` mirrors xl3 (TS)'s
`scripts/bench.mjs` — same three scenarios, same data shapes — so the
two implementations can be compared directly.

## Reference baseline

Recorded 2026-05-25 on Apple M1, `cargo --release`, no other heavy
load. Median of three runs.

| Scenario | xl3-rs | xl3 (TS) | ratio |
|---|---|---|---|
| wide-flat (10k rows × 4 cols, IF + ROUND per row) | ~56 ms | ~220 ms | **3.9× faster** |
| multi-sheet (5k rows split across 5 sheet groups) | ~23 ms | ~70 ms | **3.0× faster** |
| multi-source-join (5k Renewals × 1k Customers, inner join) | ~523 ms | ~70 ms | **7.5× slower** |

## What each scenario stresses

- **wide-flat** — row-iteration hot path, single source, per-cell
  template eval. Most representative of bulk reporting workloads.
- **multi-sheet** — group-by + per-sheet rendering. Sheet-name
  templating + per-group context build.
- **multi-source-join** — `@join` index build + per-row matched
  lookup. Tests the cross-source resolution path.

## Findings

The first two scenarios already clear the Phase 0 KPI by 3×.

The join scenario regresses against TS — TS uses a WeakMap-cached
lookup index (ADR-0014); xl3-rs currently rebuilds the lookup per row
(O(N×M) instead of O(N+M)). Wiring a hash-keyed join index into
`render::resolve_block_rows` is the next perf target.

## When to update this file

Update the table above when:

- A correctness fix changes the median by more than 10% in either
  direction. Regressions of >2× are bugs; improvements of >2× are
  worth recording so they aren't lost.
- The reference hardware changes.

Do NOT update this file every commit — the goal is a stable
reference, not a living dashboard.
