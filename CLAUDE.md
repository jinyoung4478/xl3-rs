# xl3-rs — Claude Code 세션 컨텍스트

이 레포는 [xl3](https://github.com/xl3-lang/xl3) (TS Excel 템플릿 엔진) 의 **Rust + WebAssembly 가속 구현**.

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
  - npm `xl3-wasm` 0.1.0 + `@jinyoung4478/xl3` 0.9.0-rc.1 (당시 이름, 현재 `@xl3-lang/xl3`) (rc tag, latest 0.8.0 유지)
  - GitHub Release `v0.9.0-rc.1` (prerelease) + tags `xl3-core-v0.1.0`, `xl3-wasm-v0.1.0`
  - End-to-end smoke test 통과 (js/wasm/auto 3 모드)
  - docs.rs 빌드 성공
  - xl3 (TS) README/IMPLEMENTATIONS/examples 갱신
- **Group A — 21 validation error codes** (2026-05-28) — issue #1. 17 신규 코드 상수, source/cell/eval/filename/inputs/subtotal 경로. xl3-core `4584a89` push. xl3 TS 측 변경 없음 (wasm-bridge 의 prefix 파서가 이미 0.9.0-rc.1 에서 받음)
- **Group B — 6 of 8 conformance fixtures** (2026-05-28) — xl3-core `44a0953`. `Value::Error` / `Value::Hyperlink` 신규 variants, `coerce_for_num_fmt` Empty→"" 보정, file-group `(blank)` 치환, zero-row source → 0 files, wasm32 `Date.now()` 라우팅, `Value::DateNumber` 기본 numFmt 첨부, write_formula t="e" / write_url_with_text 경로. 통과: 023 031 106 107 125 126.
- **부가** P2-A~H — multi-file API, preview/inputs, XtlError, runner 확장, cross-impl bench, numFmt 출력, hash @join (528ms→28ms), file-group splitting
- **Issue #2 검증 — ADR-0066 ghost-style** (2026-06-07) — wasm core 는 `compose_iteration_cells` row composition 이라 고스트 구조적 불가 확인 (업스트림 0.8.1 주장 검증 완료). 회귀 테스트 3건 `tests/ghost_style.rs` (plain / grouped / 348행, styles.xml+sheet XML 레벨 ink 검사). 부수 발견:
  - **수정됨** — planner 의 styles/manifest 조회가 value-range 상대 좌표 사용 (`fef5238`). A1 비시작 템플릿에서 numFmt/manifest 스타일 전부 손실되던 버그.
  - **미해결 (issue #3)** — grouped 경로 `render_grouped` 가 side_rows 를 그룹별 iter_idx 로 합성 → side summary 가 그룹 수만큼 중복. JS 는 원래 행에 1회 복원. 값 레이어 divergence. 업스트림 fixture 요청 **xl3#51** 등록 (2026-06-07) — fixture 착지 후 수정, 다음 patch 탑승 목표.
  - **수정됨** (`19a79bd`) — `CellSource::Literal` 도 manifest style_idx 를 받음 → literal 헤더/사이드 셀 스타일 보존 (stage-2 gap 해소). **breaking** (variant 형태 변경) → 0.2.0 사유.
  - 참고: native conformance 하니스가 sibling xl3 checkout (Phase 2 / 0.8.x 상태로 이동) 기준 142/143/144 실패 — 이번 변경 전부터 동일 (변경 전후 66/69). corpus 버전 흔들림 주의.
- **0.2.0 publish 라운드** (2026-06-07) — crates.io `xl3-core` 0.2.0 + npm `xl3-wasm` 0.2.0 (latest). 146/148 라인이 공개 패키지에 도달. 0.1.1 예정이었으나 Literal variant 변경이 semver-breaking 이라 0.2.0 으로. wasm 번들 1.81MB raw / 0.75MB gz (KPI <2MB 통과), Node 스모크 통과. tags `xl3-core-v0.2.0` / `xl3-wasm-v0.2.0` + GitHub Release 페이지 2건. issue #1 에 publish 보고 + close 제안 (063/143 분리 issue 제안 포함).
- **상류 재싱크 라운드** (2026-08-02) — 2개월 (xl3 main `22d9c2f` → `7b0ce42`, 70 커밋) 만에 corpus/네이밍 동기화:
  - **org / 패키지 rename 반영** — GitHub `jinyoung4478/*` → **`xl3-lang/*`**, npm `@jinyoung4478/xl3` → **`@xl3-lang/xl3`** (0.11.0, homepage xl3.io). 우리 npm 이름은 그대로 `xl3-wasm` (unscoped). Cargo.toml `repository`, README/README.ko/crate README/CHANGELOG 링크 + git remote 갱신
  - **corpus 154 → 169** — 신규 156~170 (156 static native value, 157 ADR-0066 grouped side cells, 158 chained arithmetic, 159/160 subtotal ADR-0073, 161 ADR-0074, 162~170 G24 data-loss)
  - **fixture 157 통과 (issue #3 해결)** — `render_grouped` 가 side_rows 를 그룹별 iter_idx 대신 **블록 output offset** 으로 조회. plan 쪽은 subtotal 행이 소비한 offset 자리에 빈 placeholder 를 넣어 side_rows 밀도 유지 (offset == index+1 불변식)
  - **fixture 160 통과 (ADR-0073/0046)** — planner 가 수식 셀의 캐시값을 마커/디렉티브 텍스트로 읽던 문제. formula 분기를 캐시값 검사보다 먼저 두어 `{{ [Col] }} / Subtotal` 캐시가 subtotal 행을 데이터 행으로 강등시키던 self-corruption 차단
  - **native 하니스 확장** — 69 → 79 테스트 (156/157/158/160 + G24 6건). 160 은 값 전용 comparator 로 표현 불가 (expected.xlsx 수식 셀에 캐시 없음) 라 plan 레벨 assert 로 고정
- **0.2.1 publish 라운드** (2026-08-02) — crates.io `xl3-core` 0.2.1 + npm `xl3-wasm` 0.2.1 (latest). 공개 API 변경 없는 동작 수정이라 patch. 번들 1.73MB raw / 0.73MB gz. 실제 배포될 **web-target 아티팩트로** conformance 재확인 (160/169) 후 배포. docs.rs 빌드 성공. tags `xl3-core-v0.2.1` / `xl3-wasm-v0.2.1` + GitHub Release 2건. **issue #3 close** (fixture 157 fix 보고). npm 은 OTP 브라우저 인증이 필요해 사용자가 직접 `npm publish` 실행

**현재 conformance** (upstream `7b0ce42`, 169 fixtures 기준): `--engine=wasm` **160/169 통과 · 2 실패 · 7 skip (stage 2 요구)**. js baseline 162/169. 즉 **JS 대비 gap 은 063/143 두 건뿐**. stage 2 는 158/169 (11 실패, manifest/style parity 미완).

주의 — 러너는 `engine: 'wasm'` 이어도 **템플릿 파싱을 JS 로 먼저** 한다 (manifest 추출 목적, `impl/js/src/index.ts` convert). 따라서 error 계열 픽스처(151/152/153/159/161 등)는 **Rust 에러 경로를 검증하지 않는다**. Rust 는 `xl3/subtotal/{mixed-row, explicit-block-unsupported}` 등 상류 신규 코드를 아직 안 갖고 있음 (errors.rs 미정의) — 표에 안 잡히는 실제 gap.

남은 2건 — 둘 다 rust_xlsxwriter writer-side 한계:
- **063 blank vs value compare** — eval / `coerce_for_num_fmt` 는 이미 `Empty → String("")` 변환을 만들어 두었지만 rust_xlsxwriter `store_string` 이 빈 문자열을 무조건 drop. 해결: XML post-process 레이어 또는 writer 교체.
- **143 shared-formula `shared:Ref` marker** — rust_xlsxwriter 는 shared-formula write API 가 없음 (write_formula / write_array_formula 뿐). calamine 도 read 시 shared-formula 슬레이브를 full formula text 로 풀어 버려서 share 메타데이터가 우리 파이프라인에서 손실됨. 둘 다 패치해야 함.

상세는 PLAN.md §5, issue #1.

---

## 다음 세션 시작 시 결정 사항

1. **issue #1 close + 분리** — 여전히 열림. 남은 gap 이 063/143 로 좁아졌으니 (원래 29건) 제목/범위가 낡음. 063/143 writer-side epic + stage-2/manifest parity (border) 로 분리 후 close 제안, 사용자 승인 대기.

2. **에러 코드 gap (신규 발견)** — 상류 language.md 가 정의한 코드 중 Rust `errors.rs` 에 없는 것들: `xl3/subtotal/{mixed-row, explicit-block-unsupported, bad-aggregate}`, `xl3/block/{overlap, empty-table}`, `xl3/directive/{orphan, invalid-syntax}`, `xl3/expression/{row-outside-block, bracket-outside-block}`, `xl3/eval/{no-match, bad-aggregate-arg}`, `xl3/group/missing-key`, `xl3/source/undeclared`, `xl3/parser/unbalanced-literal`. TS 러너가 파싱을 선행해서 conformance 로는 안 잡힘 — 별도 검증 필요 (native 하니스 또는 wasm 직접 호출).

3. **남은 2건 — rust_xlsxwriter 한계 작업** (변동 없음)
   - 063 / 143 둘 다 rust_xlsxwriter 의 빈 문자열 drop / shared-formula write API 부재가 블로커.
   - 짧게 가는 길: XLSX XML post-process 레이어 (xl3-core/src/output.rs 뒷단). store_string drop 회피 + shared-formula 슬레이브 `<c><f t="shared" si="..."/></c>` 주입.
   - 길게 가는 길: rust_xlsxwriter PR 또는 writer 교체 후보 (umya-spreadsheet?). 별도 epic.

4. **stage-2 후보** — border manifest 지원 (Rust `StyleSpec` 에 border 없음 + TS 추출기도 미전송, 양쪽 공동 작업). 상류 fixture 170 (`@repeat` 확장 전체 행의 numFmt 상속, #96) 도 stage-2 라 현재 skip — stage 2 를 건드릴 때 같이 본다.

5. **JSON source (ADR-0075, xl3 0.11.0)** — `convertJson` / `previewJson` 는 상류가 **wasm 명시적 미지원** (`engine: 'wasm'` + JSON 이면 throw). conformance fixture 도 없음. 우리가 대응할지는 선택 — 안 하면 그대로 JS 전용 경로.

6. **native 하니스 3건 실패는 comparator 한계** — 142/143/144. 값 전용 비교라 (a) 후행 빈 셀 폭 차이, (b) expected.xlsx 수식 셀에 캐시가 없어 우리 캐시값과 어긋남 이 원인. 142/144 는 상류 러너에서 **통과**한다. 정본은 `--engine=wasm` 러너 결과.

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

- 형제 레포: `/Users/wefun/workspaces/playground/xl3` (TS 본체). 0.11.0 부터 JS 구현이 **`impl/js/`** npm workspace 로 이동 (루트는 표준/spec/corpus)
- `@xl3-lang/xl3` 가 `xl3-wasm` 을 런타임 `import()` 로 옵셔널 로드 (package.json 의존성 아님)
- 런타임 가용성 감지 후 가속 경로 또는 기존 exceljs 폴백
- conformance 는 항상 xl3 (TS) 가 정의. Rust 는 bit-exact 재현
- Python 포트 (xtl-py, ax-exform G15) 와 동시 진행 시 conformance 흔들림 주의 — Python 1.0 안정화 후 Rust 본격화 권장

### 가속 경로 conformance 돌리는 법 (형제 레포 미오염)

형제 체크아웃은 사용자 작업 공간이라 빌드/설치로 건드리지 않는다. 스크래치패드에
`impl/js` 를 복사하고 node_modules 만 심볼릭 링크해서 돌린다:

```bash
wasm-pack build crates/xl3-wasm --target nodejs --release --out-dir pkg-node
S=<scratchpad>/xl3sync; mkdir -p "$S/js"
(cd ../xl3/impl/js && tar cf - --exclude node_modules --exclude dist .) | (cd "$S/js" && tar xf -)
ln -s ../xl3/impl/js/node_modules "$S/js/node_modules"   # workspace-local
ln -s ../xl3/node_modules "$S/node_modules"              # hoisted (exceljs 등)
mkdir -p "$S/js/dist/node_modules" && ln -s <xl3-rs>/crates/xl3-wasm/pkg-node "$S/js/dist/node_modules/xl3-wasm"
(cd "$S/js" && ../../xl3/node_modules/.bin/tsc -p tsconfig.build.json)   # 타입 에러는 무시 (emit 됨)
node dist/bin/conformance.js --fixture-dir=<xl3>/conformance/fixtures --engine=wasm
```

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
