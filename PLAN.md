# xl3-rs — 압도적 퍼포먼스 가속 계획

> 작성일: 2026-05-25
> 상태: 사전 설계 (Pre-design)
> 작성 맥락: xl3 (TS) 가 다축 워크로드 (시트 × 수식 × 스타일) 에서 브라우저 가용 한계에 부딪히는 문제 해결

---

## 1. 목표

**브라우저에서 압도적 변환 퍼포먼스.** 점진 개선이 아니라 "보는 순간 결정나는 데모" 수준의 카테고리 차이.

구체 KPI:

| 워크로드 | 현재 TS+exceljs | 목표 (xl3-rs 가속) |
|---|---|---|
| 36k 행 다축 (12시트, 수식 6개/행, CF/머지/numFmt) | 2522 ms + UI 멈춤 | **200-400 ms, UI 부드러움** |
| 70MB / 6M 셀 라운드트립 (현대차 메뉴 류) | 67초 + 브라우저 위태 | **3-8초, 메인 스레드 살아있음** |
| 메모리 사용 | 900MB+ 객체 폭발 | **~100MB packed 메모리** |

위 셋 중 어느 것도 JS 최적화 (exceljs 교체 등) 로는 못 닿는 카테고리. 가속의 정당성은 "JS로는 불가능한 영역을 가능하게" 임.

---

## 2. 사전 조사 결과 (2026-05-25 프로파일링)

### 2.1 단축 워크로드 — 베이스라인

행 수만 늘린 합성 워크로드 (1시트, 수식 2-4개):

| rows | convert (ms) | μs/row |
|---|---|---|
| 1k | 34 | 34.3 |
| 5k | 113 | 22.7 |
| 10k | 208 | 20.8 |
| 50k | 1042 | 20.8 |
| 200k | 4727 | 23.6 |

**관찰** — 단축 워크로드에서는 깔끔하게 선형. "사이즈에 비례해서 느려진다"는 체감의 정체는 super-linear 가 아니었음.

### 2.2 60k+ 행 스택 오버플로 — 수정 완료

이전엔 60k 행 근처에서 `sheet.spliceRows(start, deleteCount, ...rows)` 의 spread 가 V8 argument 한계 초과로 죽었음.

**수정**: `src/excel-document.ts`, `src/renderer.ts:524`, 테스트 1건. `...rows: unknown[][]` rest 시그니처를 `rows: unknown[][]` 배열로 변경하고, 내부에서 4096개씩 청크로 splice. 200k 행까지 동작 확인.

### 2.3 다축 워크로드 — 진짜 비용

12시트 × 3k 행 = 36k 행, 행당 수식 6개, 다양한 스타일/머지/CF/numFmt:

| 영역 | 비중 | 정체 |
|---|---|---|
| exceljs XML 직렬화 (출력) | **45%** | `_addStyle`, `_addFont`, `_addBorder`, `addStyleModel` |
| GC 압박 | 15% | exceljs 셀당 객체 폭발 |
| xl3 평가 (자체 코드) | 24% | 수식 6개/행 × 12회 expansion |
| zip deflate (pako) | 10% | `longest_match`, `deflate_slow` |
| 기타 (jszip/saxes) | 6% | crc32, XML 파싱 |

같은 행 수 (36k 다축 vs 50k 단축) 환산하면 **다축이 3.3배 비쌈**. 사용자 도메인의 진짜 비용은 다축에서 나옴.

### 2.4 실 파일 — 70MB 현대차 간식 메뉴

```
file: 2026.현대차 간식서비스 메뉴(우린_양재)_26년 3월.xlsx
size: 70.09 MB
sheets:           107
total cells:      6,357,132
formula cells:    5,953,016  (93.6%)
unique styles:    475
merged ranges:    4,273
unique fonts:     33
```

| 단계 (TS+exceljs, Node 8GB 힙) | 시간 |
|---|---|
| 로드만 | 24.7 초 |
| 쓰기만 | 41.9 초 |
| 라운드트립 | **66.6 초** |
| xl3.convert 추정 (eval 24% 추가) | **~88 초** |

**브라우저에서**: 6M 셀 × ~150 바이트 객체 오버헤드 = 900MB+ JS 객체. 탭 메모리 한계 (2-4GB) 위태. 사용자가 직접 경험한 "JS 에서 브라우저 뻗는" 시나리오의 정량적 정체.

---

## 3. 의사결정 — 풀 포트 vs 하이브리드

세 옵션 검토:

| 옵션 | 일정 | 압도적 데모 가능? | conformance 부담 |
|---|---|---|---|
| A. 풀 Rust 포트 (xl3-rs as standalone) | 6-12 개월 | ○ | TS/Py/Rust 3 구현 동기화 |
| B. 하이브리드 (TS shell + WASM accel) | **2-3 개월** | **○ (거의 동등)** | TS conformance 유지, Rust는 bit-exact 검증만 |
| C. JS 최적화 (exceljs 교체) | 1-2 개월 | ✗ (2-3x 개선 그침) | 부담 없음 |

**선택: B 하이브리드.**

근거:
- C 는 "압도적" 못 만들어냄 — 2-3x 는 데모가 안 됨
- A 는 일정 길고, 가장 어려운 부분 (보존 매트릭스 재구현) 을 새로 해야 함
- B 는 보존을 exceljs 에 맡기고 (작은 템플릿이라 비용 거의 없음), 비싼 부분 (출력 직렬화, 데이터 IO, 수식 평가) 만 Rust 로 넘김 → "압도적 80%+ 가속" 을 빠르게 달성

**Rust 측 레이어 분리** (확정 2026-05-25):
- Rust 코드를 **두 crate 로 분리** — 순수 Rust 코어 (`xl3-core`) + WASM 얇은 래퍼 (`xl3-wasm`)
- 코어는 `wasm-bindgen` 의존 0. 평범한 Rust API
- 추가 비용 거의 없으면서 미래 옵션 (Tauri, CLI, 서버 라이브러리, PyO3) 자동 확보
- 디렉토리 이름 `xl3-rs` 가 정직해짐 — "Rust 구현 (현재 WASM 소비)" 으로 명확

가정 확정:
- **템플릿은 항상 작음** (수십 KB) → exceljs 파싱 비용 < 100ms, 무시 가능
- **데이터 소스는 클 수 있음** → Rust calamine 으로 lazy/streaming
- **출력은 매우 클 수 있음** (70MB) → Rust rust_xlsxwriter bulk write

---

## 4. 아키텍처

```
┌────────────────────────────────────────────────────────────────────┐
│                       xl3 (TS, 기존)                               │
│  - 템플릿 파싱 (exceljs)                                            │
│  - 보존 매니페스트 추출:                                            │
│      styles, CF, DV, merges, named ranges, drawings, charts ref    │
│  - 템플릿 plan 생성:                                                │
│      확장 리전, 수식, 데이터 바인딩                                │
│  - 가속 가용 여부 분기:                                             │
│      가용 → xl3-wasm 호출                                           │
│      불가 → 기존 exceljs 경로 (폴백)                                │
└───────────────────────────┬────────────────────────────────────────┘
                            │ structured handoff
                            │ { manifest, plan, source_buffer }
                            ↓
┌────────────────────────────────────────────────────────────────────┐
│  Layer 2 — xl3-wasm (얇은 WASM 래퍼)                              │
│  - wasm-bindgen ↔ JS interop                                       │
│  - ArrayBuffer ↔ Vec<u8> 변환, Transferable 처리                  │
│  - JSON 매니페스트 → Rust 타입 디코딩                             │
│  - 호출 위임만 담당. 로직 없음 (~100-300 lines)                  │
└───────────────────────────┬────────────────────────────────────────┘
                            │ pure Rust API
                            │ render(plan, source_reader, writer)
                            ↓
┌────────────────────────────────────────────────────────────────────┐
│  Layer 1 — xl3-core (순수 Rust crate, wasm-bindgen 의존 0)        │
│  - calamine: 소스 데이터 lazy read                                  │
│  - 자체 평가 엔진: packed 메모리, GC 없음                           │
│  - rust_xlsxwriter: 출력 생성 (매니페스트 적용)                     │
│  - native flate: zip 압축                                           │
│  → Tauri / CLI / 서버 / PyO3 컨슈머 모두 이 crate 직접 사용 가능    │
└───────────────────────────┬────────────────────────────────────────┘
                            │
                            ↓
                  output xlsx buffer
```

### 4.1 책임 분담

| 역할 | TS / exceljs | Rust / WASM |
|---|---|---|
| 템플릿 읽기 | ✓ | |
| 보존 매니페스트 추출 | ✓ | |
| 데이터 소스 읽기 | | ✓ (calamine) |
| 템플릿 plan 평가 | | ✓ |
| 출력 직렬화 | | ✓ (rust_xlsxwriter) |
| zip 압축 | | ✓ (flate2) |

### 4.2 핵심 비용 절감

| 비용 | 이전 | 이후 |
|---|---|---|
| exceljs XML 직렬화 (45%) | TS 메인 스레드 | Rust 모듈, ~5-10x 빠름 |
| GC stall (15%) | 메인 스레드 100-500ms 멈춤 | 사라짐 (수동 메모리) |
| 객체 폭발 | 6M 셀 × 150B = 900MB | 6M 셀 × 16B packed = 96MB |
| zip deflate (10%) | pako (JS) | flate2 (Rust native) ~2-3x |

### 4.3 패키지 구조

```
xl3-rs/                          # 이 레포
├── Cargo.toml                   # workspace 루트
├── crates/
│   ├── xl3-core/                # Layer 1 — 순수 Rust
│   │   ├── Cargo.toml           #   deps: calamine, rust_xlsxwriter, flate2
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── source.rs        #   calamine 기반 SourceReader
│   │   │   ├── plan.rs          #   TemplatePlan / Manifest 타입 (전송 무관)
│   │   │   ├── eval.rs          #   XTL 평가기 (packed cells)
│   │   │   ├── output.rs        #   rust_xlsxwriter 기반 XlsxWriter
│   │   │   └── render.rs        #   render(plan, source, writer) 진입점
│   │   └── tests/               #   native rustc 테스트
│   │
│   └── xl3-wasm/                # Layer 2 — 얇은 WASM 래퍼
│       ├── Cargo.toml           #   deps: xl3-core, wasm-bindgen, js-sys
│       ├── src/
│       │   ├── lib.rs           #   #[wasm_bindgen] 진입점
│       │   ├── manifest_json.rs #   JSON → core 타입 디코더
│       │   └── buffer.rs        #   Uint8Array ↔ Vec<u8>
│       └── pkg/                 #   wasm-pack 출력 (gitignore)
│
├── examples/                    # Rust 단독 사용 예 (CLI 풍 데모)
│   └── roundtrip.rs
├── docs/
└── README.md

# xl3 (TS) 쪽 변경:
xl3/
├── src/
│   ├── accel/                   # 가속 어댑터 (신규)
│   │   ├── detect.ts            # WASM 가용성 + worker 셋업
│   │   ├── manifest.ts          # 보존 매니페스트 추출
│   │   └── handoff.ts           # WASM 호출 + 결과 수신
│   └── (기존 코드)
```

배포:
- **NPM**: `@jinyoung4478/xl3-wasm` — wasm-pack 결과물 (`xl3-wasm` crate)
- **crates.io** (선택): `xl3-core` — 순수 Rust crate. Tauri / CLI / 서버 / PyO3 컨슈머용. 첫 릴리즈는 후속 페이즈
- TS 쪽 `@jinyoung4478/xl3` 가 `@jinyoung4478/xl3-wasm` 을 옵셔널 디펜던시로 참조, 런타임 가용성 감지 후 사용

API 경계 원칙:
- `xl3-core` API 는 **JSON/JsValue 와 무관**. 평범한 Rust 타입 (`&[u8]`, `TemplatePlan`, `Manifest`, trait `SourceReader`/`XlsxWriter`)
- `xl3-wasm` 만이 JSON 디코딩 + JsValue 변환 담당
- 잘못된 예: `fn render_from_ts(json: JsValue)` 를 core 에 두기. → 항상 wasm 측에 두고, core 는 디코드된 타입 받음

---

## 5. 실행 계획

### Phase 0 — Feasibility 검증 (1주차)

목표: "압도적 퍼포먼스가 실제로 만들어질 수 있는가" 확정.

**Task 0.1 — 출력 기능 매트릭스 sanity-check** (간소화 2026-05-25)
- 비목표 확정 후 매트릭스 리스크는 거의 해소됨 (피벗/매크로/OLE 안 만듦)
- 남은 sanity-check: rust_xlsxwriter 가 도메인의 **표준** 기능들 (numFmt 변형, 머지된 헤더 + 스타일, IF/ROUND 등 일반 수식, hidden sheet, defined names) 을 정확히 재현하는지 spot-test
- 사용자 도메인 출력 1-2 파일 spot-test → `docs/feature-matrix.md` (가벼운 리포트)
- **Gate 폐기** — 진행 차단 사유 아님. 발견 사항은 Phase 1 작업 항목으로 흡수

**Task 0.2 — Rust native 상한선 측정**
- `calamine` + `rust_xlsxwriter` 로 70MB 현대차 파일 라운드트립 Rust 단독 측정
- 목표: 3-8초 범위 확인
- 결과 → `docs/native-baseline.md`

**Task 0.3 — WASM boundary 비용 측정**
- 같은 작업을 wasm-pack 빌드로 브라우저에서 실행
- WASM 인스턴스화, 버퍼 in/out 카피 비용 분리 측정
- **Gate**: native 대비 < 2x 오버헤드 → 진행, 5x 이상 → 아키텍처 재검토

**Task 0.4 — 메모리 프로파일링**
- WASM linear memory 사용량 측정 (단발 + 연속 100회)
- arena/슬랩 패턴 필요성 판단
- 결과 → `docs/memory-profile.md`

### Phase 1 — xl3-core 구현 (2-4주차, Layer 1)

이 페이즈는 **순수 Rust** 만 다룸. WASM/JS 일절 손대지 않음. `cargo test` 로 검증.

**Task 1.1 — 코어 타입 + SourceReader**
- crate: `xl3-core`
- `TemplatePlan`, `Manifest` 정의 (전송 포맷 무관)
- `trait SourceReader` + calamine 구현체
- xl3 의 source_sheet/source_table 의미 보존

**Task 1.2 — 평가 엔진 포팅**
- xl3 의 XTL 표현식 평가기 (parseFunctionCall, evalExpression, evalCell) Rust 포팅
- packed 셀 데이터 구조 (cell_idx, style_idx, value)
- xl3 conformance suite 적용 (TS 결과와 bit-exact 검증)
- `examples/roundtrip.rs` 로 native 실행 검증

**Task 1.3 — 출력 합성**
- `trait XlsxWriter` + rust_xlsxwriter 구현체
- 보존 매니페스트 → 호출 매핑
- 확장 리전: 데이터 행 bulk write
- 비확장 리전: 매니페스트의 raw 데이터 그대로 적용
- `render(plan, source, writer)` 진입점 안정화

### Phase 2 — xl3-wasm + TS 통합 (5-6주차, Layer 2 + TS)

**Task 2.1 — WASM 래퍼**
- crate: `xl3-wasm`
- `xl3-core` 의 `TemplatePlan` / `Manifest` 에 대한 JSON 디코더
- `#[wasm_bindgen]` 진입점 (단일 함수 `render(plan_json, source_buf): Uint8Array`)
- 100-300 lines 안에서 끝나야 함 (로직 없음)
- wasm-pack 으로 npm 패키지 빌드

**Task 2.2 — 보존 매니페스트 추출 (TS)**
- exceljs 워크북에서 매니페스트 JSON 생성
- 스키마는 `xl3-core` 의 `Manifest` 타입과 거울

**Task 2.3 — Web Worker 통합**
- 메인 스레드 절대 금지 — Worker 격리
- WASM 인스턴스 페이지 로드시 사전 워밍업
- 메시지 패싱 또는 SharedArrayBuffer (브라우저 호환성 확인 후 결정)

**Task 2.4 — 가용성 감지 + 폴백**
- 런타임에서 WASM 가능 여부 + 매트릭스 외 기능 사용 여부 검사
- 가능 → 가속, 불가 → 기존 exceljs 경로

### Phase 3 — 데모 + 검증 (7-8주차)

**Task 3.1 — 데모 페이지**
- 36k 다축 데모: "버튼 클릭 → 0.3초 변환 완료" 시각화
- 70MB 다운로드 데모: 변환 중에도 UI 인터랙션 가능함 보이기
- 메모리/CPU 차트 동시 표시

**Task 3.2 — Conformance 통합**
- xl3 conformance fixture 를 가속 경로로도 통과 확인
- 차이 발생 시 bit-exact 까지 추적 (스펙은 TS 가 우선)

**Task 3.3 — 번들 크기 최적화**
- `wasm-opt -Oz`
- 사용하지 않는 calamine/rust_xlsxwriter 기능 trim
- 목표: < 2MB (gzip)

---

## 6. 리스크 / 미해결

| 리스크 | 영향 | 완화책 |
|---|---|---|
| ~~`rust_xlsxwriter` 기능 매트릭스에 본인 도메인 기능이 빠져있음~~ (해소 2026-05-25) | ~~출력 못 만듦~~ | ~~Phase 0 Gate 에서 차단~~ → 도메인 출력은 rust_xlsxwriter 범위 안에서만 (피벗/매크로/OLE 등 비목표 확정) |
| WASM 번들 크기 5MB+ | 첫 인상 손상 | `wasm-opt -Oz`, 기능 trim, 스트리밍 인스턴스화 |
| 메인 스레드 멈춤 (작은 파일에도 60ms 잡으면 UX 흠집) | 데모 인상 깨짐 | 처음부터 Web Worker 격리. 메인 스레드는 WASM 호출 자체를 안 함 |
| Boundary copy 비용 (70MB output buffer 메인 스레드로 전송) | 50-100ms 추가 | Transferable ArrayBuffer 사용, 또는 OPFS 경유 |
| 메모리 누수 (연속 변환 시 fragmenting) | 장시간 사용 시 OOM | arena/슬랩 패턴, instance 리사이클 |
| Conformance 비트 정확도 (TS vs Rust 출력 동일성) | 두 경로 결과 다르면 사양 균열 | xl3 conformance fixture 양쪽 통과 필수. 차이 발생 시 TS 우선 |
| Python 포트 (G15) 와 동시 진행 부담 | 세 갈래 conformance 흔들림 | Python 1.0 안정화 후 Rust 본격화 권장. Phase 0/1 은 병행 가능 (conformance 영향 없음) |

---

## 7. 명시적 비목표 (Non-Goals)

이번 작업에서 **하지 않을 것**:

- **풀 Rust 단독 런타임 (production)** — `xl3-core` 는 그 자체로 standalone 가능한 형태로 짜지만, **공식 릴리즈 (crates.io 배포, CLI 패키징, Tauri 통합 등) 는 후속**. 지금은 브라우저 가속이 1순위. 의도된 자연 부산물로서의 standalone 가능성은 유지
- **exceljs 자체 교체** — 보존을 exceljs 에 맡기는 게 핵심 전략. 교체하지 않음
- **신규 스펙 기능 추가** — xl3 0.x 스펙 그대로. Rust 측은 항상 TS 가 정의한 동작을 재현
- **xl3 TS 의 비핵심 영역 최적화** — 시간 95% 차지하는 부분만 Rust 로 이전. 나머지는 그대로
- **고급 출력 기능 작성** — 피벗 테이블, VBA 매크로, OLE 임베디드 객체, 일부 sparkline 변형 등은 출력에서 만들지 않음 (확정 2026-05-25). 입력에 포함된 경우는 무시하고 통과 (calamine 이 셀 값만 읽고 피벗 구조 등은 스킵)

---

## 8. 일정 요약

```
Week 1   ▶ Phase 0 — Feasibility 검증 (native 라운드트립, WASM boundary)
Week 2-4 ▶ Phase 1 — xl3-core (순수 Rust, calamine + 평가기 + rust_xlsxwriter)
Week 5-6 ▶ Phase 2 — xl3-wasm + TS 통합 (얇은 래퍼, 매니페스트, 워커, 폴백)
Week 7-8 ▶ Phase 3 — 데모 + conformance + 번들 최적화
```

총 약 **8주 (2개월)** 예상. Phase 0 결과가 추정치 (3-8초 라운드트립, < 2x boundary) 벗어나면 일정 재검토.

---

## 9. 첫 행동

매트릭스 리스크 해소로 Phase 0 Gate 가 폐기됨 → **바로 Phase 0 Task 0.2 (Rust native 상한선 측정) 부터 진입 가능**.

순서:

1. **Rust toolchain 셋업** — Cargo workspace, calamine + rust_xlsxwriter 의존 추가
2. **70MB 현대차 파일 라운드트립** — Rust 단독 (WASM 없이) 측정. 목표: 3-8 초
3. **wasm-pack 빌드 + 브라우저 측정** — boundary 비용 분리. 목표: native 대비 < 2x
4. 두 숫자 확정 후 Phase 1 코어 구현 진입

선택: 사용자 도메인 대표 출력 파일 1-2개 spot-test (Task 0.1) — Phase 1 어딘가에 끼워넣음, Phase 0 차단 아님.

---

## 부록 A — 프로파일 스크립트

이번 조사에서 사용한 스크립트들. 모두 xl3 레포 `scripts/` 에 보관됨:

- `scripts/profile-scaling.mjs` — 단축 워크로드, 행 수 스케일링
- `scripts/profile-cpu.mjs` — 단일 사이즈 CPU 프로파일 capture
- `scripts/profile-realistic.mjs` — 다축 워크로드 (시트 × 수식 × 스타일)
- `scripts/profile-real-file.mjs` — 실 파일 feature density + 라운드트립
- `scripts/profile-analyze.mjs` — `--cpu-prof` 출력 분석

xl3-rs 측에서는 위 스크립트들을 import 해서 Rust/WASM 측 결과와 직접 비교 가능.

## 부록 B — 참고 라이브러리

| 라이브러리 | 역할 | 비고 |
|---|---|---|
| [`calamine`](https://crates.io/crates/calamine) | XLSX/XLS read | lazy, streaming, no DOM |
| [`rust_xlsxwriter`](https://crates.io/crates/rust_xlsxwriter) | XLSX write | libxlsxwriter 후예, 고성능 |
| [`flate2`](https://crates.io/crates/flate2) | zip/deflate | miniz_oxide 백엔드 native 압축 |
| [`wasm-bindgen`](https://crates.io/crates/wasm-bindgen) | WASM ↔ JS interop | 표준 |
| [`wasm-pack`](https://github.com/rustwasm/wasm-pack) | 빌드/배포 도구 | npm 패키지 생성 |
| [`wasm-opt`](https://github.com/WebAssembly/binaryen) | WASM 사이즈 최적화 | `-Oz` 로 코드사이즈 |
