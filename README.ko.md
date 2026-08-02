# xl3-rs

Rust + WebAssembly 가속기. [xl3](https://github.com/xl3-lang/xl3) (TypeScript) — Excel 템플릿 렌더러 — 의 브라우저 변환 퍼포먼스를 카테고리 단위로 끌어올리기 위한 형제 구현.

> **상태**: Phase 0 (Feasibility) 통과. Phase 1 (xl3-core 본 구현) 진행 중.

## 목표

브라우저에서 압도적 변환 퍼포먼스. 점진 개선이 아니라 카테고리 차이.

- 36k 행 다축 워크로드: 2.5초 → **200-400ms**
- 70MB / 6M 셀 라운드트립: 67초 → **3-8초** (Phase 0 native 측정: **3.23초 명중**)
- 메모리: 900MB+ → **~100MB packed**

자세한 사전 조사와 KPI 근거는 [`PLAN.md`](./PLAN.md), 측정 보고서는 [`docs/`](./docs/) 참고.

## 아키텍처 (요약)

하이브리드 + 레이어드. TS 측 `xl3` 가 템플릿 보존을 담당, Rust 측이 비싼 작업을 담당. Rust 측은 다시 두 레이어로 분리.

```
xl3 (TS)                              xl3-rs (Rust)
─────────────                         ───────────────────────────────────
템플릿 파싱 (exceljs)                  Layer 2: xl3-wasm
보존 매니페스트 추출   ───── JSON ─►    wasm-bindgen / JSON 디코드 / 버퍼
                                       (얇음, ~수백 줄)
                                                  │
                                                  │ pure Rust API
                                                  ▼
                                       Layer 1: xl3-core
                                       calamine + 평가기 + rust_xlsxwriter
                                       (wasm 의존 0, 단독 사용 가능)
출력 버퍼 수신          ◄─────────────  flate2 native 압축
```

핵심 아이디어:
- `xl3-core` 는 **순수 Rust**. 미래에 Tauri/CLI/서버/PyO3 컨슈머 모두 그대로 사용 가능
- `xl3-wasm` 은 얇은 어댑터. 로직 없음

## 디렉토리 구조

```
xl3-rs/
├── PLAN.md                  # 작업 계획 전체
├── README.md                # 영문 (정본)
├── README.ko.md             # 본 파일
├── Cargo.toml               # workspace 루트
├── crates/
│   ├── xl3-core/            # Layer 1 — 순수 Rust (wasm 의존 0)
│   │   ├── src/             #   source.rs, plan.rs, eval.rs, output.rs, render.rs
│   │   └── examples/        #   roundtrip.rs (Phase 0 측정)
│   └── xl3-wasm/            # Layer 2 — wasm-bindgen 래퍼
│       └── src/             #   lib.rs (#[wasm_bindgen] 진입점)
├── scripts/
│   └── measure-wasm.mjs     # Node V8 WASM 측정
└── docs/
    ├── native-baseline.md   # Phase 0 Task 0.2: Rust native 라운드트립 측정
    └── wasm-boundary.md     # Phase 0 Task 0.3: WASM boundary 비용 측정
```

## 배포

- **NPM**: [`xl3-wasm`](https://www.npmjs.com/package/xl3-wasm) (wasm-pack 출력) — `@xl3-lang/xl3` 가 런타임에 옵셔널로 로드
- **crates.io**: [`xl3-core`](https://crates.io/crates/xl3-core) — Rust 단독 사용자용

## Conformance

[`xl3`](https://github.com/xl3-lang/xl3) (TS) 의 `conformance/fixtures/` 가 spec 의 정본.
Rust 측은 동일한 fixture set 을 통과하는 것을 목표로 함. Stage 1 (셀 값 비교) 우선,
Stage 2 (OOXML 정규화 후 바이트 일치) 는 후속.

상류 러너로 가속 경로를 검증한다:

```bash
npm run conformance -- --engine=wasm   # xl3 체크아웃에서 xl3-wasm 이 resolve 되는 상태로
```

현재 통과 현황은 [`CLAUDE.md`](./CLAUDE.md) 에 기록. 픽스처가 상류에 추가될 때마다
숫자가 움직이므로 README 에는 고정하지 않는다.

자매 구현체: [xl3-py](https://github.com/xl3-lang/xl3-py) (Python).

## 라이선스

MIT.
