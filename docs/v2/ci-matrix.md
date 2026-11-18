# v2 test and CI matrix

Status: plan. Extends the jobs in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml); does not replace them. Job names below that already exist (`test`, `macos`, `aarch64`, `macos-arm`, `windows`, `windows-arm`) are kept. New jobs are appended to the same workflow or placed in `.github/workflows/mobile.yml` (section 6).

Every job uses `dtolnay/rust-toolchain@stable`, `--locked`, and `--manifest-path compiler/Cargo.toml`. All proposed names marked `(proposed)` are decisions to confirm.

## 1. Base target matrix (exists today)

| Target | Runner label | Execution | What runs | Key commands |
| --- | --- | --- | --- | --- |
| Linux x86_64 | `ubuntu-latest` | native | `test`: unit tests, clippy (`-D warnings`), release build; link-and-run smoke | `cargo test --locked …`, `cargo clippy --all-targets … -- -D warnings`, `cargo build --release --locked …` |
| Linux AArch64 | `ubuntu-latest` + `qemu-user`, `gcc-aarch64-linux-gnu` | emulated | `aarch64`: cross-compile UMAY program to `aarch64-unknown-linux-gnu`, run under qemu; HFA/C-aggregate interop; build `aarch64-unknown-linux-musl` compiler and run under qemu | `umay build --target aarch64-unknown-linux-gnu`, `qemu-aarch64 -L /usr/aarch64-linux-gnu …` |
| Windows x86_64 | `windows-latest` | native | `windows`: `cargo test --locked --lib`, release build, MinGW link-and-run smoke | `umay.exe build main.um`, `.\dist\main.exe` |
| Windows ARM64 | `windows-11-arm` | native | `windows-arm`: build compiler on ARM64, assemble/link with llvm-mingw `aarch64-w64-mingw32`, run PE | `umay.exe build --target aarch64-w64-mingw32`, `.\dist\main.exe` |
| macOS x86_64 | `macos-15-intel` | native | `macos`: build compiler, link and run a real x86_64 Mach-O | `umay build main.um`, `./dist/main` |
| macOS ARM64 | `macos-15-intel` | cross-build only | `macos-arm`: cross-build compiler to `aarch64-apple-darwin`, assemble an arm64 object, check `file` output, package artifact | `cargo build --release --locked --target aarch64-apple-darwin`, `clang -c --target=arm64-apple-darwin` |

**Why macOS ARM is cross-only:** this repo is private, so GitHub does not offer the standard arm64 macOS runner; `macos-26` queued and was cancelled after 15 minutes. Native execution is covered by AArch64 Linux under qemu and by native Windows ARM64. A real Apple Silicon run needs a runner this repository can obtain (self-hosted Mac with Apple Silicon). Deferred until then — see section 6.

**Target triples in use:** `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `aarch64-w64-mingw32`; host builds use the runner default (`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`/MinGW, `x86_64-apple-darwin`).

## 2. v2 feature-gate job sets (new)

Gate identifiers are the canonical ones in [`specification.md`](specification.md) §3–§7; `feature-flags.md` (pending) owns how a gate maps to a cargo feature or compiler flag. One host job runs all four suites; each platform job enables only the gate relevant to it. The `--features` spellings below assume a cargo feature of the same name as the gate.

| Gate | Job | Runner | Command | Also runs in |
| --- | --- | --- | --- | --- |
| `closures` | `v2-features` | `ubuntu-latest` | `cargo test --locked --features closures --manifest-path compiler/Cargo.toml` | `v2-ui` (capture/borrow cases) |
| `traits`, `dyn_trait` | `v2-features` | `ubuntu-latest` | `cargo test --locked --features "traits dyn_trait" --manifest-path compiler/Cargo.toml` | `aarch64`, `macos`, `windows`, `windows-arm` (vtable ABI, section 4) |
| `async_await` | `v2-features` | `ubuntu-latest` | `cargo test --locked --features async_await --manifest-path compiler/Cargo.toml` | perf job (`python3 scripts/bench.py --json` — the only benchmark driver in the tree today; the per-phase `scripts/v2bench.py async-http` driver in [`benchmarks.md`](benchmarks.md) is still proposed and not present) |
| `extern_abi` (bindgen is a tool, spec §5) | `v2-features` | `ubuntu-latest` | `cargo test --locked --features extern_abi --manifest-path compiler/Cargo.toml` | `aarch64`, `windows-arm` (callback + aggregates, section 4) |

`v2-features` is a single job with four steps; a failing step fails the job. Gates default off, so pre-v2 jobs are unchanged.

## 3. Compile-fail / UI test job (implemented)

| Job | Runner | Harness | Command |
| --- | --- | --- | --- |
| `v2-ui` | `ubuntu-latest` | `compiler/tests/ui.rs` — a dependency-free snapshot runner. For each `compiler/tests/ui/<case>.um` in sorted order it invokes the built compiler (`env!("CARGO_BIN_EXE_umay")`) as `umay check --color never <case>.um` with `current_dir` set to `compiler/tests/ui/` (stable relative source names/spans), then diffs the captured output against `<case>.stderr`. No feature flags are needed and no dependency is added — the compiler crate stays dependency-free. | `cargo test --locked --manifest-path compiler/Cargo.toml --test ui -q` |

**What is captured (important):** `umay check` writes the rendered diagnostics to **stdout** and only the `<file>: N error(s), M warning(s)` summary to **stderr** ([`cli.rs`](../../compiler/src/cli.rs) §check). The harness concatenates stdout followed by stderr, so each `.stderr` snapshot holds the full rendered report plus the summary line. The files are still named `.stderr` for documentation compatibility, but they are **not** raw stderr. Snapshots are normalised by stripping trailing whitespace from each line only; every other character (paths, spans, codes, blank lines) is compared verbatim.

**Updating a snapshot:** run `BLESS=1 cargo test --locked --manifest-path compiler/Cargo.toml --test ui -q` to rewrite mismatching snapshots, then review the resulting diff. Without `BLESS`, a mismatch prints a small line diff naming the case and fails the test; a bare red snapshot blocks merge.

**Seeded cases and pinned codes** (all under `compiler/tests/ui/`):

| Case | Pinned diagnostic |
| --- | --- |
| `closure_move_use_after_move.um` | `E0201` — use after move into a closure (`move \|\| p.x` then reuse of `p`) |
| `closure_unannotated_param.um` | `E1019` — closure parameter type cannot be inferred |
| `dyn_object_safety.um` | `E1023` — method returning `Self` is not object-safe |
| `dyn_unknown_trait.um` | `E1022` — unknown trait in a `dyn` bound |
| `macro_malformed_definition.um` | `E0125` — unterminated macro definition |
| `macro_malformed_invocation.um` | `E0126` — missing closing delimiter (plus the `E0343` parser cascade) |
| `v1_regression_clean.um` | no diagnostics — a v1 program that must stay clean |

Together these cover object-safety violations, illegal closure captures, unknown `dyn` traits, macro error spans, and v1 no-regression (Phases 1/2/8 of [`benchmarks.md`](benchmarks.md)).

## 4. ABI conformance jobs (new/extended)

Two suites, run on every backend. Each is a UMAY program plus, where a native link is possible, a C helper compiled by the target toolchain; expected stdout is diffed exactly.

| Suite | Linux x86_64 (`ubuntu-latest`) | Linux AArch64 (`aarch64`, qemu) | Windows x86_64 (`windows-latest`) | Windows ARM64 (`windows-arm`) | macOS x86_64 (`macos`) | macOS ARM64 (`macos-arm`) |
| --- | --- | --- | --- | --- | --- | --- |
| **vtable calls** — `Vec<Box<dyn Renderer>>`, two impls, indirect method calls, trait-object size/align | native run | run under qemu | MinGW link + run | llvm-mingw link + run | native run | cross: assemble object, check arm64, no run (deferred) |
| **C struct-by-value aggregates** — small struct returned in registers, larger struct via hidden pointer, float aggregates | SysV AMD64: small struct in rax/rdx | AAPCS64 HFA: `f64` pair in `d0`–`d1` (existing HFA test) | Microsoft x64: aggregates returned via hidden pointer in rcx (caller-allocated) | **Microsoft ARM64 rules, not AAPCS:** aggregates of 1/2/4/8 bytes returned in `x0`; other sizes via hidden pointer in `x8`; aggregates >8 bytes passed by reference; **no AAPCS-style HFA float-register passing** | Darwin AMD64 SysV-like | cross: assemble object only |

Windows ARM64 is the high-risk row: its aggregate rules differ from the Linux AAPCS HFA test that already exists, so it gets its own C helper and exact-output diff in `windows-arm`, not a copy of the qemu test. (Flag: confirm the exact 1/2/4/8-byte and x8 thresholds against the current llvm-mingw toolchain before locking the snapshot.)

Commands follow the existing pattern: `umay build --target <triple> …`, then the target linker + runner (`qemu-aarch64 -L …`, `.\dist\main.exe`, `./dist/main`). Add the vtable/aggregate checks as named steps inside the existing jobs; no new runners needed.

## 5. Blocking vs informational

| Class | Contents | Effect |
| --- | --- | --- |
| **Blocks merge** | `test`, `windows`, `macos`, `aarch64`, `windows-arm` green; all `v2-features` suites; `v2-ui` snapshots; ABI conformance on all five executable backends; hard-fail performance deltas ([`performance-budget.md`](performance-budget.md)) | PR cannot merge; fix or granted exception required |
| **Informational** | `macos-arm` native execution (unavailable — cross-build + arch check only); mobile emulator/device workflows (section 6) until Phase 6 GA; warning-band performance deltas; benchmark tables and frame-time graphs | Annotate PR, report status, never block |
| **Release only** | `release`, `release-aarch64`, platform packaging in `macos`/`macos-arm`/`windows`/`windows-arm`, `publish` (tag `v*`) | Run on tags; not a PR gate |

Performance exceptions: see [`performance-budget.md`](performance-budget.md) §5 (compiler owner approval + linked issue + `perf-exception` label, expires per phase).

## 6. Separate mobile workflow plan

New file `.github/workflows/mobile.yml`. Triggers: `workflow_dispatch`, nightly schedule, and tags `v*` — **not** every PR. Status is informational until Phase 6 reaches GA, then simulator + emulator jobs become blocking for PRs that touch mobile code.

| Track | Runner label | What runs | Command |
| --- | --- | --- | --- |
| iOS simulator | `macos-15-intel` | Build and test a signed-for-simulator app; install on a named simulator via `xcrun simctl` | `xcodebuild -scheme UmayTodo -configuration Debug -sdk iphonesimulator -destination 'platform=iOS Simulator,name=iPhone 16' test` |
| Android emulator | `ubuntu-latest` with KVM (self-hosted Linux runner preferred) | Start AVD, install APK, run instrumentation tests | `android-actions/setup-android`, then `./gradlew connectedAndroidTest` (or `adb shell am instrument`) |
| iOS real device | self-hosted macOS with device attached | Install and run on physical device; signing required | `xcodebuild … -destination 'platform=iOS,id=<UDID>' test` |
| Android real device | self-hosted Linux with device attached via ADB | Install and run on physical device | `adb install … && adb shell am instrument …` |
| Packaging/signing | same runners, on tags only | Produce signed `.ipa` / `.apk` artifacts | `xcodebuild archive` + `xcrun notarize` (iOS); `./gradlew assembleRelease` (Android) |

**Hardware / account prerequisites (hard requirements, not optional):**

- Apple Developer Program membership (paid) — device provisioning profiles and distribution signing. Simulator builds do not need it; device builds and `.ipa` signing do.
- Signing certificates and provisioning profiles stored as GitHub Actions secrets; signing runs on tags/dispatch only, never on untrusted PR code.
- Xcode + an installed iOS simulator runtime on the macOS runner (`macos-15-intel` ships Xcode; pin the simulator name).
- Android SDK + JDK + emulator image. GitHub-hosted `ubuntu-latest` has **no KVM**, so emulator runs need software rendering (slow) or a self-hosted Linux runner with KVM exposed. Real-device tracks need physical devices permanently attached to self-hosted runners.
- A self-hosted Apple Silicon macOS runner is required to ever cover native macOS ARM execution; the private repo cannot obtain GitHub's arm64 macOS runner.

**Deferred (explicitly out of scope now):** native Apple Silicon CI execution; iOS device tests on GitHub-hosted runners (impossible — need a Mac with devices); a hosted Android device farm; code signing for pull requests; treating any mobile job as a merge gate before Phase 6 GA.

## 7. Conventions

- Runner labels are literal GitHub-hosted labels except `self-hosted`-tagged machines described above.
- Compiler binary: `target/release/umay` (Linux/macOS) or `target/release/umay.exe` (Windows); build with `--release --locked`.
- Generated programs are written to `$RUNNER_TEMP`, output diffed against an inline `expected.txt` — same pattern as the existing smoke steps.
- Cache isolation: set `UMAY_CACHE_DIR` to a throwaway directory in benchmark steps so results are not served from a warm cache.
