# xl3-rs — Claude Code 세션 컨텍스트

이 레포는 [xl3](https://github.com/jinyoung4478/xl3) (TS Excel 템플릿 엔진) 의 **Rust + WebAssembly 가속 구현**.

세션 시작하면 먼저 **반드시 다음 두 파일 정독**:
1. [`PLAN.md`](./PLAN.md) — 작업 계획 전체 (목표, 아키텍처, 8주 일정, 리스크)
2. [`README.md`](./README.md) — 디렉토리 구조, 배포 형태

---

## 빠른 컨텍스트 (30초)

**왜 만드나** — 브라우저에서 압도적 변환 퍼포먼스. xl3 (TS) 가 다축 워크로드 (시트 × 수식 × 스타일) 에서 한계 (70MB 파일 67초, 브라우저 위태). Rust+WASM 으로 카테고리 점프.

**무엇을 만드나** — 하이브리드 가속기.
- TS 측 `xl3` 는 템플릿 보존 담당 (exceljs, 그대로 둠)
- Rust 측은 두 레이어:
  - `xl3-core` — 순수 Rust crate (`wasm-bindgen` 의존 0). calamine + 평가기 + rust_xlsxwriter.
  - `xl3-wasm` — 얇은 wasm-bindgen 래퍼 (~수백 줄, 로직 없음)
- 추후 Tauri/CLI/PyO3 컨슈머가 `xl3-core` 직접 사용 가능 (의도된 부산물)

**무엇을 안 만드나** (확정 비목표)
- 풀 Rust 단독 런타임 (production 릴리즈는 후속)
- exceljs 자체 교체 (보존은 TS 가 그대로)
- 고급 출력 기능 작성: 피벗 테이블, VBA 매크로, OLE 임베디드, 일부 sparkline 등 — rust_xlsxwriter 범위 밖. 입력에 박혀 있어도 무시하고 통과 (calamine 이 셀 값만 읽음)

**KPI**
- 36k 다축: 2522ms → **200-400ms** (TS 측정 → Rust+WASM 추정)
- 70MB / 6M 셀: 67초 → **3-8초**
- 메모리: 900MB+ → **~100MB packed**

---

## 현재 상태

**Phase**: Pre-implementation (계획만 수립됨)
**다음 작업**: Phase 0 — Feasibility 검증
  1. Rust toolchain 셋업 (rustup, cargo workspace 초기화)
  2. `crates/xl3-core` 스켈레톤 (calamine + rust_xlsxwriter 의존 추가)
  3. 70MB 현대차 메뉴 파일 (`/Users/wefun/workspaces/ax-tf/admin-excel-converter/assets/2026.현대차 간식서비스 메뉴(우린_양재)_26년 3월.xlsx`) 의 Rust native 라운드트립 측정
  4. wasm-pack 빌드 후 브라우저 측정 (boundary 비용 분리)

상세는 PLAN.md §5 Phase 0 / §9 첫 행동.

---

## 핵심 설계 원칙

### `xl3-core` API 는 JsValue/JSON 무관

```rust
// ✓ 좋음 — 평범한 Rust 타입
pub fn render(
    plan: TemplatePlan,
    source: impl SourceReader,
    writer: impl XlsxWriter,
) -> Result<Vec<u8>>

// ✗ 나쁨 — core 에 wasm 종속 새지 마라
pub fn render_from_ts_manifest(json: JsValue) -> JsValue
```

JSON 디코딩, `JsValue` 변환은 **`xl3-wasm` 측에서만**. 이 경계가 깨지면 미래 컨슈머 (Tauri/CLI/PyO3) 가 core 못 씀.

### 브라우저 데모 함정 (절대 어기지 말 것)

- **메인 스레드 절대 금지** — 처음부터 Web Worker. 60ms 도 잡히면 데모 인상 깨짐
- **WASM 번들 < 2MB** — `wasm-opt -Oz`, 기능 trim 필수
- **Transferable ArrayBuffer** — 70MB 결과를 메인 스레드로 복사하면 50-100ms 추가 발생
- **연속 변환 메모리 안정** — arena/슬랩, 처음 100회 돌려서 누수 없어야 함

상세: PLAN.md §6 리스크.

---

## xl3 (본 레포) 와의 관계

- 형제 레포: `/Users/wefun/workspaces/playground/xl3` (TS 본체)
- xl3 가 `@jinyoung4478/xl3-wasm` 을 옵셔널 디펜던시로 import
- 런타임 가용성 감지 후 가속 경로 또는 기존 exceljs 폴백
- conformance 는 항상 xl3 (TS) 가 정의. Rust 는 bit-exact 재현
- Python 포트 (xtl-py, ax-exform G15) 와 동시 진행 시 conformance 흔들림 주의 — Python 1.0 안정화 후 Rust 본격화 권장

---

## 작업 스타일

- 대화 언어: 한국어
- 문서 언어: 한국어 (PLAN.md 등 내부 문서). 향후 공개 시 영어 보강
- 코드 주석: 영어
- 커밋: Conventional commits, 한국어/영어 혼용 가능. 본문은 짧고 의도 중심
- 매 결정마다 PLAN.md / 본 파일 업데이트

---

## 프로파일 데이터 보관 위치

xl3 (TS) 레포의 `scripts/profile-*.mjs` 스크립트들이 측정 인프라:
- `profile-scaling.mjs` — 행 수 스케일링
- `profile-realistic.mjs` — 다축 워크로드 (12시트 × 수식 × 스타일)
- `profile-real-file.mjs` — 실 파일 feature density + 라운드트립
- `profile-analyze.mjs` — CPU 프로파일 분석

xl3-rs 가속 결과는 위 스크립트의 baseline 과 비교해서 보고.
