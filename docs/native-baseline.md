# Phase 0 Task 0.2 — Rust native 라운드트립 측정

> 측정일: 2026-05-25
> 머신: macOS 24.6 (darwin), Apple Silicon
> Toolchain: rustc 1.90.0, cargo 1.90.0
> 빌드: `cargo build --release` (LTO=thin, codegen-units=1, opt-level=3)

---

## 1. 측정 대상

| 항목 | 값 |
|---|---|
| 입력 파일 | `2026.현대차 간식서비스 메뉴(우린_양재)_26년 3월.xlsx` |
| 입력 크기 | 70.09 MB |
| 시트 수 | 107 |
| 사용 영역 셀 (range area) | 7,650,051 |
| 비공백 셀 (written) | 5,673,218 |
| 라이브러리 | calamine 0.35.0 (read) + rust_xlsxwriter 0.95.0 (write) |
| 코드 | `crates/xl3-core/examples/roundtrip.rs` |

---

## 2. 결과

| run | load (ms) | write (ms) | total (ms) | RSS peak |
|---|---:|---:|---:|---:|
| 1 (cold) | 2035 | 1231 | **3266** | 1.16 GB |
| 2 | 2013 | 1212 | **3225** | 1.16 GB |
| 3 | 2000 | 1197 | **3197** | 1.16 GB |
| 4 | 2022 | 1210 | **3232** | 1.16 GB |
| **mean** | **2018** | **1213** | **3230** | **1.16 GB** |

출력 파일: 19 MB (입력 70 MB 의 ~27%) — 수식이 캐시값으로 치환되고 스타일/머지/CF 가 빠졌기 때문.

---

## 3. 비교

| 환경 | 라운드트립 (s) | 비고 |
|---|---:|---|
| TS + exceljs (Node 8GB heap) | 66.6 | xl3 의 사전 측정 (PLAN.md §2.4) |
| TS + exceljs + xl3 eval (추정) | ~88 | eval 24% 가중 |
| **Rust native (현재)** | **3.23** | calamine + rust_xlsxwriter |

→ TS baseline 대비 **약 20.6×** 빠름. PLAN.md 목표 KPI **3-8 초 범위에 명중** (Phase 0 Task 0.2 통과).

---

## 4. 주의 사항 (fair-comparison disclaimer)

이 측정은 **셀 IO 의 상한선** 만 본 것. 다음을 의도적으로 무시함:

- 스타일 (font, fill, border, numFmt) 보존
- 머지된 셀 보존
- Conditional Formatting / Data Validation
- 차트, drawings, 이미지
- 수식 보존 (cached value 로만 출력)
- defined names, hidden sheets, sheet 순서 / 이름

따라서 Phase 1 에서 보존 매니페스트를 추가하면 라운드트립 시간이 늘어남.
다만 다음 근거로 KPI **3-8 초** 범위 안에서 흡수 가능하다고 본다:

1. **출력 직렬화 자체는 1.2초**. rust_xlsxwriter 가 매니페스트의 스타일 등록(475 styles)을 처리해도, 셀당 추가 비용은 O(1) lookup 한 번.
2. **수식 보존은 추가 메모리만 ↑, 시간 ↑은 작음**. 수식 문자열은 calamine 에서 `Xlsx::formula` 로 따로 읽을 수 있고, rust_xlsxwriter 의 `write_formula` 비용도 셀당 ~µs.
3. **머지/CF/DV 는 셀 수가 아니라 영역 수에 비례**. 4,273 머지 / 수십 개 CF rule 로 추가 비용은 무시할 수준.

→ Phase 1 통합 후에도 **5-7 초** 안쪽 예상. Phase 2 의 WASM boundary 오버헤드 (목표 < 2×) 를 얹어도 KPI 8 초 안.

---

## 5. 메모리 관찰

| 항목 | 값 | 비고 |
|---|---|---|
| RSS peak | ~1.16 GB | calamine 의 `Range<Data>` × 107 시트 모두 메모리에 동시 보유 |
| 입력 셀당 평균 | ~150 bytes | `Data` enum (24 B) + String 페이로드 + Range sparse 오버헤드 |

PLAN.md 의 **목표 "~100MB packed"** 와 큰 차이가 있다. 이는 예상된 결과로,
Phase 1 의 **packed cell layout** (cell_idx + style_idx + value tag) 으로 해결할 영역.
calamine 의 default model 은 dense `Range<Data>` 라 sparse 시트에서 메모리 낭비가 큼.

Phase 0 의 결론에는 영향 없음: KPI 시간 목표는 명중. 메모리 목표는 Phase 1 의 작업 항목.

---

## 6. 명령어 (재현용)

```bash
# Build
cargo build --release -p xl3-core --example roundtrip

# Run
./target/release/examples/roundtrip \
    "/path/to/2026.현대차 간식서비스 메뉴(우린_양재)_26년 3월.xlsx" \
    out/hyundai.xlsx

# With memory stats
/usr/bin/time -l ./target/release/examples/roundtrip \
    "/path/to/...xlsx" out/hyundai.xlsx
```

---

## 7. 다음 단계

- **Phase 0 Task 0.3** — wasm-pack 으로 동일 라운드트립을 브라우저 Worker 에서 실행, native 대비 < 2× 오버헤드 확인 (Gate)
- **Phase 0 Task 0.4** — WASM linear memory 사용량 + 연속 100회 실행 누수 점검
- 두 게이트 통과 후 Phase 1 (xl3-core 본 구현) 진입
