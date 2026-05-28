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

**Phase**: Phase 2 + Phase 3 코어 인프라 완료. 외부 검증 + 라이브러리화 1차 마무리.

완료:
- **Phase 0** — Feasibility 검증 (native 3.23s / WASM warm 1.78× / 번들 1.3MB)
- **Phase 1** — xl3-core stage-1 conformance 99/99 (P1-A ~ P1-V)
- **Phase 2 Task 2.1** — xl3-wasm `convert` / `readTemplateInputs` / `preview` 진입점 + bytes API (1.7MB raw / 0.71MB gz)
- **Phase 2 Task 2.2** — 매니페스트 추출 (TS, exceljs → JSON) + 적용 (Rust, font/alignment/fill/numFmt + merge ranges + column widths)
- **Phase 2 Task 2.3** — Web Worker 격리 (demo 로 충족)
- **Phase 2 Task 2.4** — xl3 (TS) 에 `engine: 'auto' | 'wasm' | 'js'` 옵션, optional wasm import + 자동 폴백 (xl3 TS 212/212 tests)
- **Phase 3 Task 3.1** — 브라우저 데모 (examples/demo, Web Worker + 3 시나리오)
- **Phase 3 Task 3.2** — conformance 가속 경로 인프라 (xl3 conformance-runner `--engine=wasm` flag)
- **Phase 3 Task 3.3** — 번들 최적화 (wasm-opt -Oz pin, 0.71 MB gz, KPI <2 MB 통과)
- **외부 검증 4건** (2026-05-26) — conformance 측정 (110/148→119/148), 매니페스트 stage 2 진단, 70MB 재측정 (퇴행 없음), 300 회 메모리 안정
- **Native formula preservation** (2026-05-26) — ADR-0021/0046. 097, 129, 142, 144 통과. `CellSource::CellFormula` 추가, calamine `worksheet_formula` 연동, iteration bounds 합집합, col_range 인접 확장
- **Error code 인프라** (2026-05-26) — `XtlError { code }` propagation 완성. arity / xlookup 코드 정착, wasm-bridge 가 `[xl3/...]` prefix → JS Error `.code` 변환
- **publish 라운드** (2026-05-26~27):
  - crates.io `xl3-core` 0.1.0 + `xl3` 0.0.1 (placeholder)
  - npm `xl3-wasm` 0.1.0 + `@jinyoung4478/xl3` 0.9.0-rc.1 (rc tag, latest 0.8.0 유지)
  - GitHub Release `v0.9.0-rc.1` (prerelease) + tags `xl3-core-v0.1.0`, `xl3-wasm-v0.1.0`
  - End-to-end smoke test 통과 (js/wasm/auto 3 모드)
  - docs.rs 빌드 성공
  - xl3 (TS) README/IMPLEMENTATIONS/examples 갱신
- **Group A — 21 validation error codes** (2026-05-28) — issue #1. 17 신규 코드 상수, source/cell/eval/filename/inputs/subtotal 경로. xl3-core `4584a89` push. xl3 TS 측 변경 없음 (wasm-bridge 의 prefix 파서가 이미 0.9.0-rc.1 에서 받음)
- **부가** P2-A~H — multi-file API, preview/inputs, XtlError, runner 확장, cross-impl bench, numFmt 출력, hash @join (528ms→28ms), file-group splitting

**현재 conformance**: `--engine=wasm` **140/148 (94.6%)** stage 1, js baseline 148/148.

남은 8건 (모두 issue #1 Group B — feature work, 별도 epic):
- 023 TODAY/clock injection
- 031 empty-range output filename
- 063 blank vs value compare
- 106 `#DIV/0!` error cell
- 107 `(blank)` group-key placeholder (현재 filename empty error 로 잘림)
- 125 HYPERLINK cell-link metadata
- 126 date arithmetic ISO output
- 143 shared-formula `shared:Ref` marker

상세는 PLAN.md §5, issue #1.

---

## 다음 세션 시작 시 결정 사항

1. **xl3-core 0.1.1 patch 즉시 publish?**
   - Group A 21건 통과 → 외부 사용자 가시 효과 큼
   - 또는 0.9.0 정식 cut 직전 (≤ 2026-06-02) 에 묶어서 한 번에
   - xl3-wasm 도 같이 0.1.1 publish 해야 효과 (xl3-core 만 올리면 wasm 경로 fix 안 닿음)
   - 권장: rc soak 종료 (2026-06-02) 직전 0.1.1 batch publish — 시간 절약 + 일관성

2. **xl3 TS 0.9.0 정식 cut**
   - 7-day rc soak 데드라인: **2026-06-02** (오늘 5/28 기준 5일 남음)
   - 외부 critical issue 없으면 latest 승격
   - 절차: RELEASING.md §"Final 1.0.0 cut" 의 minor variant

3. **Group B 8건**
   - 정식 0.9.0 이후 0.9.1 ~ 0.10.0 사이클로 점진
   - 125 HYPERLINK / 143 shared formula 는 features.md 에 spec 보강 필요할 수도
   - 107 은 Group A 의 filename-empty error 가 너무 적극적으로 발사된 부수 영향 — `(blank)` substitution 먼저 들어가야 자연 해소

4. **보안 정리 (사용자 액션)**
   - https://crates.io/settings/tokens — 이번 publish 토큰 revoke
   - https://www.npmjs.com/settings/jinyoung4478/tokens — npm 토큰 revoke

5. **유사 작업 후보**
   - xl3 (TS) 측 7 언어 README "What's new" 동기화 (ko / ja / zh-CN / zh-TW / es)
   - website/docusaurus Acceleration 가이드 페이지
   - xl3-rs 측 GitHub Release 노트 `xl3-core-v0.1.0`, `xl3-wasm-v0.1.0` (현재 tag 만, page 없음)

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
