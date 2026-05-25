# Phase 0 Task 0.3 — WASM boundary 비용 측정

> 측정일: 2026-05-25
> 머신: macOS 24.6 (darwin), Apple Silicon
> Toolchain: rustc 1.90.0, wasm-pack 0.13.1, Node 22.22.0 (V8)
> 빌드: `wasm-pack build crates/xl3-wasm --target nodejs --release -- --features debug`
> 측정 스크립트: `scripts/measure-wasm.mjs`

---

## 1. 측정 대상

같은 70MB 현대차 메뉴 파일에 대해 **WASM 으로 라운드트립** 을 수행하고, Native 베이스라인 (`docs/native-baseline.md`) 과 비교.

핵심 경로:

```
fs.readFile (Node)
  → Uint8Array (Node heap)
    → passArray8ToWasm0  (JS → WASM linear memory copy)
      → wasm.roundtrip   (calamine read + rust_xlsxwriter write, all in wasm)
        → getArrayU8FromWasm0(...).slice()  (WASM → Uint8Array copy)
          → return to caller
```

---

## 2. 빌드 산출물

| 항목 | 값 | 비고 |
|---|---|---|
| `xl3_wasm_bg.wasm` | **1.3 MB** | `wasm-opt -Oz` 적용 후 |
| KPI (PLAN.md §6) | < 2 MB | ✓ 통과 |
| crate-type | cdylib + rlib | |
| features | `debug` (panic hook on) | release 본 배포는 default features 사용 예정 |

빌드 사이즈 KPI 는 여유 있게 통과. 향후 Phase 1 의 평가 엔진과 매니페스트 디코더가 추가되면 +200-500KB 예상이지만 2MB 안 유지 가능.

---

## 3. 측정 결과

5회 연속 호출 (warm-up 없이):

| run | roundtrip (ms) | output (MB) | RSS (MB) |
|---:|---:|---:|---:|
| 1 (cold) | 9408.8 | 19.0 | 1140 |
| 2 | 5680.0 | 19.0 | 1172 |
| 3 | 5749.8 | 19.0 | 1172 |
| 4 | 5738.6 | 19.0 | 1172 |
| 5 | 5731.0 | 19.0 | 1172 |
| **mean (all)** | **6461.6** | — | — |
| **mean (warm, runs 2-5)** | **5724.8** | — | — |

부차 비용:

| 항목 | 시간 | 비고 |
|---|---:|---|
| Module load + WASM instantiate | 4.7 ms | sync compile + instantiate (Node target) |
| fs.readFile (70MB) | 10.5 ms | macOS APFS, warm page cache |

---

## 4. Native 대비 비교

| 측정 | 시간 (s) | Native 대비 |
|---|---:|---:|
| Native (rust release, native FS) | 3.23 | 1.00× |
| **WASM warm (mean of runs 2-5)** | **5.72** | **1.78×** |
| WASM cold (run 1) | 9.41 | 2.91× |
| WASM mean (5 runs) | 6.46 | 2.00× |

### Gate 판정

PLAN.md §5 Phase 0 Task 0.3 의 **Gate: native 대비 < 2× → 진행, 5× 이상 → 아키텍처 재검토**.

- **Warm: 1.78× → 통과** ✓
- Cold: 2.91× → 5× 한참 아래. 워밍업 정책으로 충분히 가릴 수 있음
  - PLAN.md §2.3 의 "WASM 인스턴스 페이지 로드시 사전 워밍업" 정책이 이를 위한 것
  - Worker 부팅 시 1KB 더미 xlsx 한 번 라운드트립 → V8 TurboFan tiering 안정화 → 실제 사용자 클릭 시점에는 warm 상태

→ **Phase 0 Task 0.3 통과**.

---

## 5. KPI 시간 예산 시뮬레이션

PLAN.md §1 의 KPI 는 **70MB 라운드트립 3-8 초, 브라우저 메인 스레드 살아있음**.

현재 측정값을 KPI 예산에 맞춰보면:

```
warm WASM 라운드트립 (cells IO only)   5.7 s
+ Phase 1 스타일 / 머지 / 수식 보존     ~1.0 s (추정, rust_xlsxwriter 호출 O(영역))
+ 매니페스트 JSON 디코드 (TS → wasm)    ~0.1 s (수십 KB 매니페스트)
+ in/out Transferable ArrayBuffer 카피  ~0.05 s (Node 측정 기준)
─────────────────────────────────────
~6.9 s
```

KPI **8 초 안** ✓. 단, 다음 항목들로 더 줄일 수 있음:

1. **V8 SIMD / Liftoff Off**: wasm-pack 의 default V8 옵션은 Liftoff (빠른 baseline JIT) → TurboFan 으로 tier-up. 호출 1번에 자동 tier-up 됨. Liftoff 끄면 cold 비용은 늘지만 일관성 ↑
2. **calamine streaming**: 현재는 모든 시트 `Range<Data>` 를 메모리에 동시 보유 → linear memory 1.1GB. Phase 1 의 packed layout (cell_idx + style_idx + value tag) 으로 줄이면 캐시 효율 ↑ → CPU 도 빠름
3. **wasm-opt SIMD 활성화**: SIMD 명령 활성화 시 일부 hot loop 의 가속 가능

→ Phase 1/2 의 작업으로 5-6 초까지 줄어들 여지가 있음. 8 초 KPI 안전.

---

## 6. 메모리 관찰

| 항목 | 값 |
|---|---|
| 호출 전 RSS | 128 MB (Node + Uint8Array buf) |
| 호출 후 RSS | ~1172 MB (5회 호출 후 안정) |
| Native RSS | ~1158 MB |

WASM linear memory 가 native heap 과 거의 동등. 호출 2회차부터 RSS 안정 → linear memory 의 allocator 가 호출 사이에 재사용. **연속 호출 시 누수 없음 (5회 기준)**.

PLAN.md §6 의 "메모리 누수 (연속 변환 시 fragmenting)" 리스크는 5회 기준 관찰 안 됨. Task 0.4 (연속 100회 누수 점검) 는 Phase 1 의 packed layout 작업과 합쳐서 진행.

---

## 7. 함정 (만나본 것 + 회피)

### 7.1 `time not implemented on this platform` panic

`rust_xlsxwriter` 가 workbook metadata 의 modified/creation time 을 위해 `std::time::SystemTime::now()` 호출. wasm32-unknown-unknown 에서는 이 함수가 panic.

**해결**: `xl3-wasm` 의 의존성에 `rust_xlsxwriter = { features = ["wasm"] }` 추가. crate 의 `wasm` feature 가 `js_sys::Date::now()` 로 라우팅. cargo 가 `xl3-core` 의 transitive dep 와 unify.

### 7.2 wasm-pack nodejs target = synchronous compile

Node target 은 `new WebAssembly.Module(bytes)` 동기 호출 → 모듈 로딩 비용이 module require 시점에 발생. 우리 경우 4.7ms 로 무시 가능.

브라우저는 `WebAssembly.instantiateStreaming(fetch(...))` 비동기 가능 → cold 비용을 fetch / 백그라운드와 겹쳐서 가릴 수 있음.

### 7.3 input/output 둘 다 카피

wasm-bindgen 의 `&[u8]` 인자 + `Vec<u8>` 반환 패턴은 양쪽 다 카피.
- in: JS Uint8Array → WASM malloc → 70MB 카피 (~25ms)
- out: WASM linear memory → `.slice()` → 19MB 카피 (~6ms)

총 ~30ms 카피 오버헤드. 5.7초의 0.5% 라 무시 가능.
zero-copy 패턴 (SharedArrayBuffer + WASM grow) 은 Phase 2 의 worker 통합에서 검토 가치 있음. 다만 ROI 낮음.

---

## 8. 명령어 (재현용)

```bash
# Build (default features, no panic hook — production 빌드)
wasm-pack build crates/xl3-wasm --target nodejs --release

# Build with panic hook (디버그)
wasm-pack build crates/xl3-wasm --target nodejs --release -- --features debug

# Measure
node --expose-gc scripts/measure-wasm.mjs \
    "/path/to/2026.현대차 ... .xlsx" \
    out/wasm-roundtrip.xlsx \
    --runs=5
```

---

## 9. 다음 단계

- **Phase 0 통과 확정** (Task 0.2 + Task 0.3 모두 KPI 안)
- **Task 0.4 (연속 100회 누수)** 는 Phase 1 의 packed layout 작업과 합쳐서 진행 — 단발 5회 기준은 이미 안정
- **브라우저 (Worker) 측정** 은 Phase 2 의 Worker 통합 시 동시 진행. Node V8 결과로 충분히 추정 가능 (V8 동일 엔진)
- **Phase 1 진입**: `xl3-core` 의 본 구현 (TemplatePlan / Manifest 타입, eval, 스타일/머지/수식 보존 출력)
