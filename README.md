# xl3-rs

Rust + WebAssembly acceleration for [xl3](https://github.com/jinyoung4478/xl3) — the Excel template renderer.

> **상태**: 사전 설계 단계. 본격 구현은 Phase 0 (Feasibility) 통과 후 시작.

## 목표

브라우저에서 압도적 변환 퍼포먼스. 점진 개선이 아니라 카테고리 차이.

- 36k 행 다축 워크로드: 2.5초 → **200-400ms**
- 70MB / 6M 셀 라운드트립: 67초 → **3-8초**
- 메모리: 900MB+ → **~100MB packed**

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

자세한 내용은 [`PLAN.md`](./PLAN.md) 참고.

## 디렉토리 구조 (예정)

```
xl3-rs/
├── PLAN.md                  # 본 작업 계획
├── README.md                # 이 파일
├── Cargo.toml               # workspace 루트
├── crates/
│   ├── xl3-core/            # Layer 1 — 순수 Rust (wasm 의존 0)
│   │   └── src/             #   source.rs, plan.rs, eval.rs, output.rs, render.rs
│   └── xl3-wasm/            # Layer 2 — wasm-bindgen 래퍼
│       └── src/             #   lib.rs, manifest_json.rs, buffer.rs
├── examples/                # Rust 단독 사용 예
│   └── roundtrip.rs
└── docs/
    ├── feature-matrix.md    # 기능 spot-test 보고서 (Phase 0/1)
    ├── native-baseline.md   # Rust native 라운드트립 측정 (Phase 0)
    └── memory-profile.md    # WASM 메모리 프로파일 (Phase 0)
```

## 배포

- **NPM**: `@jinyoung4478/xl3-wasm` (wasm-pack 출력) — xl3 가 옵셔널 디펜던시로 사용
- **crates.io** (후속): `xl3-core` — Rust 단독 사용자용

## 라이선스

추후 결정 (xl3 와 동일하게 MIT 예상).
