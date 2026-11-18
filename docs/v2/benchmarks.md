# v2 per-phase benchmarks and exit criteria

Status: plan. This is the 90-day plan's item 10. Every phase has one scenario, one metric, one command, one pass criterion, and a CI home. Programs live in `examples/v2/`; the driver is `scripts/v2bench.py <phase> [--json]` `(proposed)` which builds/runs the scenario and checks the criterion, printing JSON for [`performance-budget.md`](performance-budget.md). A phase is done only when its exit criterion is green in CI.

Commands assume `cargo build --release --manifest-path compiler/Cargo.toml` has run; `umay` is `target/release/umay`. Set `UMAY_CACHE_DIR` to a throwaway directory in perf steps. Gate names are the canonical identifiers in [`specification.md`](specification.md) §3–§7 and are reserved in [`feature-flags.md`](feature-flags.md); `proc_macro` gates no syntax in `2.0.0`.

## Phase map (quick)

| Phase | Gate | Blocks merge when green in | CI home |
| --- | --- | --- | --- |
| 1 | `closures` | `v2-features`, `v2-ui` | `ubuntu-latest` |
| 2 | `traits`, `dyn_trait` | `v2-features`, `v2-ui`, `aarch64` + ABI | `ubuntu-latest`, qemu |
| 3 | `extern_abi` (bindgen tool) | `v2-features`, `aarch64`, `windows-arm` | `ubuntu-latest`, qemu, ARM64 |
| 4 | `async_await` | `v2-features`, perf | `ubuntu-latest` |
| 5 | GUI (ADR 0020) | desktop smoke + packaging | `ubuntu-latest`, `windows-latest` |
| 6 | mobile (ADR 0021) | mobile workflow (informational → gate at GA) | macOS sim, Linux emu, devices |
| 7 | game | desktop smoke + perf | `ubuntu-latest`, optional GPU runner |
| 8 | proc-macro `(proposed gate)` | `v2-features`, `v2-ui` | `ubuntu-latest` |

## Phase 1 — closures

- **Scenario:** `examples/v2/p1_closures.um` — `map`/`filter`/`fold` over `Vec<i64>`; a GUI callback prototype (function-pointer callback invoked from a C stub); an async combinator prototype (chain of callbacks) that must compile. Plus a compile-fail case that moves a captured value illegally.
- **Metric:** compiles, borrowck runs (not bypassed), output correct, illegal capture diagnosed.
- **Command:** `umay check examples/v2/p1_closures.um && umay run examples/v2/p1_closures.um`; `cargo test --locked --features closures --test ui --manifest-path compiler/Cargo.toml`.
- **Pass:** all compile; program prints expected integers; UI snapshot shows a borrow error for the illegal capture. No `unsafe`/raw-pointer escape hatch in the program.
- **CI:** `v2-features` + `v2-ui` on `ubuntu-latest`.
- **Status of this slice:** covered by Rust integration tests (`phase10_closures.rs`, `phase12_borrow_captures.rs`); `examples/v2/p1_closures.um` is the intended scenario but is not checked in — the file path above describes the planned example, not an existing one.

## Phase 2 — traits / dyn

- **Scenario:** `examples/v2/p2_dyn.um` — `Vec<Box<dyn Renderer>>` with two impls, `dyn EventHandler`, `dyn Future`; indirect calls through each. Compile-fail case: object-safety violation (generic method or associated const on a trait used as `dyn`).
- **Metric:** compiles on both backends; vtable calls return correct values; violation diagnosed with an E-code and source span.
- **Command:** `umay build --target x86_64-unknown-linux-gnu examples/v2/p2_dyn.um && ./dist/p2_dyn`; `umay build --target aarch64-unknown-linux-gnu examples/v2/p2_dyn.um && qemu-aarch64 -L /usr/aarch64-linux-gnu ./dist/p2_dyn`; `cargo test --locked --features "traits dyn_trait" --test ui --manifest-path compiler/Cargo.toml`.
- **Pass:** identical stdout on x86_64 and AArch64; UI snapshot matches the object-safety diagnostic.
- **CI:** `v2-features` + `v2-ui` + vtable row of the ABI suite in `aarch64`, `macos`, `windows`, `windows-arm`. Covers the object-safety and vtable-call tests required by ADR 0018 §7.
- **Status of this slice:** covered by Rust integration tests (`phase12_dyn_trait.rs`, `phase14_dyn_dispatch.rs`, `phase15_impl_trait.rs`); `examples/v2/p2_dyn.um` is the intended scenario but is not checked in — the file path above describes the planned example, not an existing one.

## Phase 3 — FFI / bindgen

- **Scenario:** bind SDL3, miniaudio and SQLite from C headers with `umay bindgen --header … --out …` — no hand-written `extern` wrapper in the program. Exercise a callback-based C API (e.g. `sqlite3_exec` callback or `qsort` comparator). Aggregate-by-value round trip on Windows ARM64 (Microsoft rules).
- **Metric:** bindings generated and compile; callback result correct; Windows ARM64 aggregate interop prints expected value.
- **Command:** `umay bindgen --header third_party/sqlite3.h --out src/sqlite_gen.um`; `umay build examples/v2/p3_ffi.um && ./dist/p3_ffi`; Windows ARM64: extend the existing `windows-arm` HFA step with the Microsoft-aggregate C helper and exact-output diff.
- **Pass:** scenario source contains zero manual `extern` declarations for the bound functions; stdout matches; WinARM64 aggregate test green.
- **CI:** `v2-features` (`extern_abi`) on `ubuntu-latest`; callback test in `aarch64`; aggregate test in `windows-arm`. Note: spec §5 targets the bindgen tool at `2.1.0`, so the "no hand wrappers" criterion depends on that tooling landing; until then this phase's FFI correctness runs with generated declarations checked in.

## Phase 4 — async

- **Scenario:** `examples/v2/p4_async_http.um` — extend `examples/http-router.um` with an async accept/request loop; stress cancellation and timeout; document the blocking-escape hatch (`block_on` / `spawn_blocking`) in `docs/v2/async.md`.
- **Metric:** thousands of concurrent connections handled with bounded memory; cancellation/timeout deterministic; escape hatch documented.
- **Command:** `umay build examples/v2/p4_async_http.um`; `python3 scripts/v2bench.py async-http --connections 4096`; `cargo test --locked --features async_await --test async_stress --manifest-path compiler/Cargo.toml`.
- **Pass:** ≥4,096 concurrent connections completed with no crash and peak RSS within the generated-program budget; cancellation/timeout tests green; `docs/v2/async.md` exists and names the blocking escape hatch.
- **CI:** `v2-features` + perf job on `ubuntu-latest`; throughput charts informational.
- **Status of this slice:** covered by Rust integration tests (`phase19_async.rs`, `phase19_async_syntax.rs`, `phase19_future_stdlib.rs`); `examples/v2/p4_async_http.um` is the intended scenario but is not checked in — the file path above describes the planned example, not an existing one.

## Phase 5 — GUI

- **Scenario:** SDL3 todo app (`examples/v2/p5_todo_sdl.um`) and a webview/HTML version; add / toggle / delete via a `--selftest` mode; package for Linux (tar/AppImage) and Windows (zip) with `scripts/package.py`.
- **Metric:** self-test state correct; both packages produced non-empty artifacts.
- **Command:** `SDL_VIDEODRIVER=dummy umay run examples/v2/p5_todo_sdl.um --selftest`; `scripts/package.py --label x86_64-linux --binary dist/p5_todo_sdl --out out`; Windows: `python scripts/package.py --label x86_64-windows --binary dist/p5_todo_sdl.exe --out out`.
- **Pass:** self-test exits 0 with expected final state; each packaging step emits a non-empty artifact; headless driver used so CI needs no display.
- **CI:** `ubuntu-latest` + `windows-latest`; bundle sizes recorded per budget §4.

## Phase 6 — mobile

- **Scenario:** one shared business-logic module (the todo model) compiled into an iOS app and an Android app; run the same test vectors on simulator and emulator; produce signed builds on tags.
- **Metric:** both platforms install and pass identical vectors; signed device/simulator builds produced.
- **Command:** `xcodebuild -scheme UmayTodo -configuration Debug -sdk iphonesimulator -destination 'platform=iOS Simulator,name=iPhone 16' test`; `./gradlew connectedAndroidTest` (emulator on Linux with KVM); on tags, `xcodebuild archive` + `./gradlew assembleRelease`.
- **Pass:** iOS and Android test targets green with byte-identical vector outputs; `.ipa` / `.apk` artifacts uploaded on tags.
- **CI:** `.github/workflows/mobile.yml` — `macos-15-intel` (simulator), `ubuntu-latest` + KVM (emulator), self-hosted devices; informational until GA, then gates mobile-touching PRs. Requires Apple Developer Program membership and signing secrets (see [`ci-matrix.md`](ci-matrix.md) §6).

## Phase 7 — game

- **Scenario:** 2D sprite game (`examples/v2/p7_sprite.um`) and 3D demo (`examples/v2/p7_3d.um`) with sound, input and packaging; built-in frame-time profiler (`--bench 10s`).
- **Metric:** frame time p99 ≤ 16.6 ms (60 FPS); input/sound self-test passes; packages produced.
- **Command:** `umay build examples/v2/p7_sprite.um && ./dist/p7_sprite --bench 10s`; same for `p7_3d`; `SDL_VIDEODRIVER=dummy` for smoke, real GPU runner for the FPS number.
- **Pass:** p99 frame time ≤ 16.6 ms on the pinned GPU runner; self-test exits 0; packages non-empty. Without a GPU runner the FPS criterion is informational, not a gate.
- **CI:** `ubuntu-latest` headless smoke (blocking); perf job records frame time; real-FPS gate needs a self-hosted GPU runner (prerequisite).

## Phase 8 — proc-macro

- **Scenario:** a derive-like macro (`#[derive(Debug)]`-style) and a codegen macro that emits a function; a case where a macro-generated error must point at the user's source line/column; expansion must be byte-identical across runs.
- **Metric:** reproducible expansion, correct error spans.
- **Command:** `umay expand examples/v2/p8_derive.um > /tmp/out1.um && umay expand examples/v2/p8_derive.um > /tmp/out2.um && cmp /tmp/out1.um /tmp/out2.um`; `cargo test --locked --features proc_macro --test phase23_expand_cli --manifest-path compiler/Cargo.toml`.
- **Pass:** `cmp` clean; the integration test asserts reproducible expansion, a diagnostic on a malformed macro, and a no-macro round trip; reproducibility matches `build-system.md` §7 (no timestamps/paths in output).
- **CI:** `v2-features` (`proc_macro`) + `v2-ui` on `ubuntu-latest`.
- **Status of this slice:** `umay expand`, the `proc_macro` Cargo feature (reserved, gates no code yet) and `compiler/tests/phase23_expand_cli.rs` exist. The `examples/v2/p8_derive.um` scenario is now checked in: a real `#[derive(Copy)]` struct plus a `macro_rules!` definition/invocation that prints `42`, verified with `umay expand` (byte-identical across runs) and `umay build` + run. There is **no** `--test ui` harness and no UI snapshot for the `file:line:col` of a macro-generated error yet — that is still planned.

## Harness notes

- `scripts/v2bench.py` and feature names are proposals; wire them into the jobs named in [`ci-matrix.md`](ci-matrix.md) so a phase exit criterion failing in CI is what defines "done", not a local run.
- Where a phase installs a system library (SDL3, miniaudio, SQLite), the CI job installs it via the platform package manager explicitly; a missing library fails the job rather than being skipped.
