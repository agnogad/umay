# UMAY

[![CI](https://github.com/agnogad/umay/actions/workflows/ci.yml/badge.svg)](https://github.com/agnogad/umay/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.75+](https://img.shields.io/badge/rustc-1.75%2B-orange.svg)](#installing-from-source)

**A native systems language with Rust-style ownership, a compiler you can read,
and diagnostics built for humans and agents.**

```umay
fn main() {
    let name = "Umay"
    let mut version = 1

    print("Hello, {name}")

    version += 1
    print("Version: {version}")
}
```

> Safety without unnecessary complexity. Performance without unnecessary
> overhead. Diagnostics that explain, not merely complain.

- [Why UMAY](#why-umay)
- [Status](#status)
- [Installing](#installing)
- [Quick start](#quick-start)
- [Targets](#targets)
- [Tooling](#tooling)
- [Examples](#examples)
- [Documentation](#documentation)
- [Building and testing](#building-and-testing)
- [Not yet](#not-yet)
- [Contributing](#contributing)
- [License](#license)

## Why UMAY

- **Ownership without a second language.** Move and borrow checks run on MIR,
  the same intermediate representation the backends see. A borrow conflict names
  both origins; a limitation is reported under its own code rather than hidden.
- **A compiler without a large dependency tree.** The frontend, MIR, ownership
  checker, both native backends and the CLI are written in Rust with no
  crates.io dependencies. There is no LLVM or Cranelift in the production path.
- **Diagnostics as data.** Every code has a stable meaning, a human renderer, a
  versioned JSON renderer and an `umay explain` entry. Editors and agents can
  consume the same diagnostics a person reads.
- **The runtime is UMAY.** Allocation, strings, integer formatting and file
  access are UMAY source compiled into every program, so the language can be
  studied end to end in one repository.

## Status

| Area | State |
| --- | --- |
| v1 language and compiler | Stable. Lexer, parser, sema, HIR/MIR, ownership/borrowck, two backends, runtime, build system and LSP. |
| v2 specification | Separate, frozen-v1-compatible track. Feature-gated additions are opt-in with `#![feature(...)]`; enforcement is enabled with `--features v2-gates`. |
| v2 first slices | Closures, traits/`dyn Trait` with vtable dispatch, C FFI + `umay bindgen`, `async fn`/`.await`, SDL3 GUI and 2D game, compile-only mobile targets, token macros and `#[derive(Copy)]`. |
| Verification | `cargo test`: 996 passed, 1 ignored. `cargo clippy --all-targets -- -D warnings`: clean. UI snapshots and gated tests run in CI. |
| Active caveats | v2 additions are deliberately narrow; see [Not yet](#not-yet) and [ROADMAP.md](ROADMAP.md). |

v1 programs compile unchanged. Nothing in v2 changes an existing program's
source or observable behaviour unless it opts into a feature gate.

## Installing

### Prebuilt binaries

Linux x86_64, Linux ARM64, Intel macOS and Apple Silicon:

```sh
curl -fsSL https://raw.githubusercontent.com/agnogad/umay/main/install.sh | sh
```

The installer reads the latest release, verifies its SHA-256, and puts `umay`
in `~/.local/bin`. Pin a version with `UMAY_VERSION`, or change the destination
with `UMAY_INSTALL_DIR`. On Windows, download the `x86_64-windows` or
`aarch64-windows` zip from the [releases page](https://github.com/agnogad/umay/releases)
and add `umay.exe` to `PATH`. Every archive has a `.sha256` sibling.

### From source

Requires Rust 1.75 or newer and a C compiler/linker for generated programs.

```sh
git clone https://github.com/agnogad/umay.git
cd umay
cargo build --release
./target/release/umay version
```

## Quick start

```sh
umay new hello
cd hello
umay build src/main.um
./dist/main
```

`umay new` writes a `umay.toml` and `src/main.um`. UMAY projects never contain
a `target/` directory: compiler caches live in the OS user cache, and only final
artifacts appear in `dist/`.

```text
project/
├── umay.toml
├── src/
│   └── main.um
└── dist/
    └── application
```

## Targets

`--target` selects the system; without it the compiler builds for the host. All
ten triples below are recognised. Desktop objects can be linked on a host with
the matching SDK/CRT; mobile triples are **compile-only** here and require Xcode
or the Android NDK to produce an app.

| Triple | Arch | ABI | Object | Linking today |
| --- | --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | x86_64 | SysV | ELF | Native |
| `x86_64-apple-darwin` | x86_64 | SysV | Mach-O | macOS runner |
| `x86_64-pc-windows-gnu` | x86_64 | Win64 | COFF | MinGW-w64 |
| `aarch64-unknown-linux-gnu` | AArch64 | AAPCS64 | ELF | Native / qemu |
| `aarch64-apple-darwin` | AArch64 | AAPCS64 | Mach-O | Cross-built on Intel macOS |
| `aarch64-w64-mingw32` | AArch64 | Win64 | COFF | llvm-mingw |
| `aarch64-apple-ios` | AArch64 | AAPCS64 | Mach-O | Objects only (Xcode required) |
| `aarch64-apple-ios-sim` | AArch64 | AAPCS64 | Mach-O | Objects only (Xcode required) |
| `aarch64-linux-android` | AArch64 | AAPCS64 | ELF | Objects only (NDK required) |
| `x86_64-linux-android` | x86_64 | SysV | ELF | Objects only (NDK required) |

```sh
umay build --target aarch64-unknown-linux-gnu main.um
umay asm --target aarch64-apple-darwin main.um      # print, don't link
umay asm --emit-object-only --target aarch64-apple-ios main.um
```

Backend differences are confined to `compiler/src/target.rs`, `codegen.rs`,
`isel/` and `aarch64.rs`: symbol prefixing, object directives, calling
convention and the runtime's system layer. `runtime/runtime.um` is the portable
body; `runtime/platform/*.um` supplies per-system C symbols and constants.

## Tooling

```text
check <files...>        Parse and validate source (--format json, --explain)
build <file>            Compile to an executable in dist/
run <file>              Compile and run
asm <file>              Print generated assembly
ast <file>              Print the syntax tree as JSON
expand <file>           Print macro-expanded source (--format json for spans)
mir <file>              Print lowered MIR
fmt <files...>          Format source (--check to report only)
lint <files...>         Style findings (--rules lists them)
explain <CODE>          Explain a diagnostic code
doctor                  Toolchain and cache information
bindgen --header H --out O    C header -> UMAY extern declarations
new <name>              Scaffold a project
clean                   Remove build artifacts
version                 Print the compiler version
```

Common options: `-O0`..`-O3`, `--target <triple>`, `--emit-object-only`.
Exit codes: `0` success, `1` compile error, `2` usage error, `3` I/O error,
`4` retired, `5` code generation failed.

## Examples

| Example | What it shows |
| --- | --- |
| [`examples/http-router.um`](examples/http-router.um) | Routed HTTP server with process-per-connection concurrency |
| [`examples/web-app/`](examples/web-app/) | Full web backend: routes, JSON, SQLite, sessions, templates |
| [`examples/v2/sqlite_spike/`](examples/v2/sqlite_spike/) | Real SQLite bound from `umay bindgen` output, no hand wrapper |
| [`examples/v2/gui_app/`](examples/v2/gui_app/) | Real WebKitGTK window with an HTML todo app |
| [`examples/v2/p5_todo_sdl.um`](examples/v2/p5_todo_sdl.um) | SDL3 headless GUI smoke test (`GUI_OK 2`) |
| [`examples/v2/p7_sprite.um`](examples/v2/p7_sprite.um) | 2D sprite demo with input and a fixed-timestep loop |
| [`examples/v2/p7_3d.um`](examples/v2/p7_3d.um) | Integer 3D software renderer |
| [`examples/v2/audio_tone.um`](examples/v2/audio_tone.um) | Deterministic PCM WAV generation |
| [`examples/v2/mobile_core.um`](examples/v2/mobile_core.um) | Shared note core and JSON IPC envelope |
| [`examples/v2/p8_derive.um`](examples/v2/p8_derive.um) | `#[derive(Copy)]` plus a declarative macro |

Run an example from a scratch directory so `dist/` lands there:

```sh
cd /tmp && mkdir umay-example && cd umay-example
umay build /path/to/umay/examples/v2/p5_todo_sdl.um
SDL_VIDEODRIVER=dummy ./dist/main
```

## Documentation

| Document | Contents |
| --- | --- |
| [docs/language-specification.md](docs/language-specification.md) | Normative v1 syntax and semantics |
| [docs/v2/README.md](docs/v2/README.md) | v2 track index and dependency order |
| [docs/v2/specification.md](docs/v2/specification.md) | Versioned v2 grammar |
| [docs/v2/feature-flags.md](docs/v2/feature-flags.md) | Feature names, lifecycle and diagnostics |
| [docs/v2/backward-compatibility.md](docs/v2/backward-compatibility.md) | The v1-freeze contract |
| [docs/type-system.md](docs/type-system.md) | Types, inference, generics, traits |
| [docs/memory-model.md](docs/memory-model.md) | Ownership, borrowing, destruction |
| [docs/compiler-architecture.md](docs/compiler-architecture.md) | Pipeline, modules, interfaces |
| [docs/diagnostics.md](docs/diagnostics.md) | Codes, schema, JSON contract |
| [docs/build-system.md](docs/build-system.md) | Manifest, caching, native dependencies |
| [docs/measurements.md](docs/measurements.md) | Recorded timings, sizes and baselines |
| [ROADMAP.md](ROADMAP.md) | Milestones and explicit exit criteria |
| [CHANGELOG.md](CHANGELOG.md) | Release history |
| [docs/adr/](docs/adr/) | Architecture Decision Records (0001–0022) |

Repository overview:

```text
compiler/   Rust implementation: frontend, MIR, ownership, backends, CLI, LSP
runtime/    UMAY runtime and per-system platform layers
stdlib/     UMAY standard library (fn, future, ipc, audio_wav, ...)
examples/   v1 applications and v2 feature spikes
docs/       specifications, v2 design, ADRs
scripts/    benchmark, packaging and release tooling
tests/      integration and cross-target tests (under compiler/tests/)
out/        release archives
```

## Building and testing

```sh
cargo build --release
cargo test --locked --manifest-path compiler/Cargo.toml
cargo clippy --all-targets --locked --manifest-path compiler/Cargo.toml -- -D warnings
cargo test --locked --test ui                    # compile-fail snapshots; BLESS=1 to update
cargo test --locked --features v2-gates          # opt-in feature-gate checks
cargo build --release --features v2-gates        # enforce gates in builds
```

The suite covers parser recovery, diagnostics and JSON, MIR lowering, ownership,
both backends, FFI ABI conformance, snapshots, reproducibility, and the v2
first slices. Two invariants are tested directly: the parser never panics on
truncated or mutated input, and generated archives are byte-reproducible.

## Not yet

These are real limits, not hidden bugs. Each is tracked in `ROADMAP.md`.

- **Async:** no reactor or waker wake; `Pending` is handled by a bounded
  busy-poll loop for concrete futures. No `Pin`, `spawn`, `select`, `timeout`,
  cancellation, `Send` bounds or async I/O wrappers.
- **GUI:** no `umay-gui` `dyn Backend` facade, fonts, menus, dialogs, asset
  packaging, accessibility or webview JS↔UMAY callbacks.
- **Mobile:** compile-only objects; no iOS codesign/SPM/WKWebView shell, no
  Android NDK/JNI/Gradle AAR/APK/AAB. Store packages need macOS and accounts.
- **Game:** no GPU/Vulkan/Metal renderer, 3D z-buffer, textures, image decoder,
  gamepad or device audio playback (WAV generation works).
- **Macros:** no proc-macro/plugin ABI; library rules (`no dependencies`,
  `unsafe_code = "deny"`) are unchanged. See ADR 0022.
- **Traits:** no associated-type declarations, blanket-impl coherence,
  supertrait slot ordering, `Vec<Box<dyn T>>` or `impl Trait` returns.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). A phase may not claim a feature without
tests, and memory safety is never weakened to simplify an implementation.
Diagnostic codes must have `umay explain` entries; UI snapshots are regenerated
with `BLESS=1` and reviewed in the same diff.

## License

MIT OR Apache-2.0.
