# UMAY

UMAY is a modern, native, statically typed systems programming language.

It combines Rust-inspired memory safety with simpler syntax, exceptionally clear
compiler diagnostics, and workflows designed for AI-assisted development.

> **Safety without unnecessary complexity.
> Performance without unnecessary overhead.
> Diagnostics that explain, not merely complain.**

```um
fn main() {
    let name = "Umay"
    let mut version = 1

    print("Hello, {name}")

    version += 1

    print("Version: {version}")
}
```

## Status

UMAY is under active development. This repository currently contains **Phases 0
through 5** of the roadmap and Phase 6 as far as the file layer: a
complete lexer, parser and semantic analyser with the AI-first diagnostics
infrastructure, a full HIR → MIR lowering with explicit control flow and drop
elaboration, an ownership and borrow checker running on that MIR, and a custom
x86_64 backend.
`umay build` produces a working executable, `umay run` builds and runs it,
and `umay asm` prints the generated assembly. The MIR can be inspected with
`umay mir <file>`.

## Targets

One frontend, three systems. `--target` selects one; without it, the compiler
builds for the machine it is running on.

| Triple | Object format | ABI | Runtime layer |
| --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | ELF | System V | `runtime/platform/linux.um` |
| `x86_64-apple-darwin` | Mach-O | System V | `runtime/platform/macos.um` |
| `x86_64-pc-windows-gnu` | COFF | Win64 | `runtime/platform/windows.um` |

```sh
umay build main.um                                   # the host's own system
umay build --target x86_64-apple-darwin main.um
umay build --target x86_64-pc-windows-gnu main.um
umay asm --target x86_64-apple-darwin main.um        # print, don't link
```

What differs between them is confined to a few places, so a fourth system means
adding one file rather than finding every assumption:

- **Symbol spelling.** Mach-O and COFF prefix external names with `_`, which
  `compiler/src/target.rs` (`ObjectFormat::symbol_prefix`) applies once.
- **Object-file directives.** Mach-O wants `.section __TEXT,__const` and rejects
  the ELF `.type`/`.size`/`.note.GNU-stack` metadata; COFF rejects the stack
  note. `Target::rodata_directive` and `Target::object_trailer` decide.
- **Calling convention.** Win64 passes arguments in `%rcx, %rdx, %r8, %r9`
  (four, not six), reserves 32 bytes of shadow space, returns *every* aggregate
  through a hidden pointer, and preserves `%rsi`/`%rdi`. `Abi` carries this, and
  `isel`, `regalloc` and `asm` ask it.
- **The runtime's system layer.** The C symbols, their numeric constants
  (`O_CREAT` is `0x40` on Linux, `0x200` on macOS, `0x0100` on Windows), the
  `errno` accessor (`__errno_location`, `__error`, `_errno`), and whether `fork`
  exists at all.

`runtime/runtime.um` is the portable body: allocation, byte strings, integer
formatting, output. `runtime/platform/*.um` is what a new system has to supply.
On Windows there is no `fork`, so the checked fan-out uses `CreateThread`
instead; the language's `Send` guarantee is unchanged, because it was settled by
the front end before either call.

Cross builds need an assembler that can emit the target's object format. Both
non-Linux targets use `clang`'s integrated assembler, which is why a `--target`
build assembles on a Linux host. **Linking** a non-host target additionally
needs that system's SDK and CRT: a Linux machine produces a correct Mach-O or
COFF object but cannot link it without an Apple SDK or a MinGW-w64 runtime.
Only x86_64 has a backend, so an ARM triple is refused with an explanation
rather than emitting a binary nothing can load.

Every program is linked against a runtime written in UMAY itself, and the prelude
and the first library types are UMAY source too, so a program can print, allocate
build strings and read and write files without importing anything:

```umay
fn main() -> i32 {
    println("merhaba")
    println_i64(42)

    let mut s = String::new()
    let mut i = 0
    while i < 5 {
        s = s.push(65 + i as u8)
        i = i + 1
    }
    println(s.as_ptr())

    let text = "merhaba, dosya\n"
    if write_file_bytes("not.txt" as *const u8, text as *const u8, byte_len(text)) {
        println_i64(file_size("not.txt" as *const u8))
    }
    return 0
}
```

A command-line tool is an ordinary program: arguments arrive through `main`'s
`argc`/`argv` and `Args` wraps them, so a flag has no global state behind it.

```umay
fn main(argc: i32, argv: *mut *mut u8) -> i32 {
    let args = Args::new(argc, argv)
    let path = args.value_after("--file" as *const u8)
    if read_byte(path) == 0u8 {
        eprintln("no --file given")
        return 2
    }
    let bytes = read_file_bytes(path)
    println_i64(bytes.len() as i64)
    bytes.free()
    return 0
}
```

The runtime is UMAY source compiled with each program, not a binary blob, and
assumes only the POSIX calls it declares itself — `malloc`, `free`, `write`,
`exit`, `open`, `read`, `close`, `lseek`, `unlink`, `rename`, `mkdir` and
`getenv`. See
[ADR 0010](docs/adr/0010-runtime-in-umay.md). `String` lives in
`stdlib/string.um` and is compiled into every program by the same path, which is
why it is checked, borrowed and code-generated exactly like user code.

The backend is UMAY's own — no LLVM, no Cranelift. It covers integer
arithmetic, control flow, structs, function calls and recursion, and is
optimised at four levels. Constructs it cannot yet generate code for are
reported as `E3001` rather than miscompiled; see
[ADR 0009](docs/adr/0009-x86_64-backend.md).

| Phase | Area | Status |
|-------|------|--------|
| 0 | Architecture and specification | **complete** |
| 1 | Lexer and parser | **complete** |
| 2 | Semantic analysis and type checking | **complete** |
| 3 | HIR and MIR | **complete** |
| 4 | Ownership and borrow checking | **complete** |
| 5 | Custom x86_64 backend | **complete** |
| 6 | Runtime and standard library | **complete** |
| 7 | AI developer experience | **complete** |
| 8 | Build system and packages | **complete** |
| 9 | Stabilization | **complete** (Linux verified; see below) |

macOS and Windows emit the right objects and are asserted against a real
assembler, but a linked, running binary on those systems has not been verified
on this host. See `ROADMAP.md` Phase 10 for the exact status.

See [ROADMAP.md](ROADMAP.md) for the detailed milestone plan and
[docs/](docs/) for the specifications this implementation is held to.

## What works today

```bash
cargo build --release          # build the compiler (produces target/release/umay)

umay new my-project            # scaffold a project (umay.toml, src/main.um, dist/)
umay check src/main.um         # lex, parse, type-check, report diagnostics
umay check src/*.um --format json
umay check src/main.um --explain   # include full explanations
umay ast src/main.um           # dump the syntax tree as JSON
umay explain E0101             # explain any diagnostic code
umay doctor                    # toolchain and cache information
umay clean                     # remove dist/ and the external cache
```

Exit codes are stable: `0` success, `1` compile error, `2` usage error,
`3` I/O error, `4` retired (no command uses it), `5` code generation failed.

## Project layout

UMAY projects never contain a `target/` directory. Compiler temporaries and
caches live in the operating system's user cache directory; only final artifacts
appear in `dist/`.

```text
project/
├── umay.toml
├── src/
│   └── main.um
└── dist/
    └── application
```

## Repository layout

```text
.
├── compiler/            # the `umay` compiler (written in Rust)
│   └── src/
│       ├── source.rs       # source text, spans, line/column mapping
│       ├── token.rs        # token definitions
│       ├── lexer.rs        # source -> tokens
│       ├── ast.rs          # abstract syntax tree
│       ├── parser/         # tokens -> AST
│       ├── diagnostics.rs  # human and JSON diagnostic rendering
│       ├── explain.rs      # the `umay explain` database
│       ├── cache.rs        # external cache location, AST JSON dump
│       └── cli.rs          # the `umay` command line surface
├── docs/                 # language and compiler specifications
├── dist/                 # build output directory (empty by default)
└── ROADMAP.md
```

Rust is only the implementation language of the *compiler*. The production code
generation backend is written by this project; UMAY does not use LLVM or
Cranelift.

## Documentation

| Document | Contents |
|----------|----------|
| [docs/language-specification.md](docs/language-specification.md) | Normative syntax and semantics, v1.0 |
| [docs/type-system.md](docs/type-system.md) | Types, inference, generics, traits |
| [docs/memory-model.md](docs/memory-model.md) | Ownership, borrowing, lifetimes, destruction |
| [docs/compiler-architecture.md](docs/compiler-architecture.md) | Pipeline, modules, interfaces |
| [docs/diagnostics.md](docs/diagnostics.md) | Codes, schema, JSON contract |
| [docs/build-system.md](docs/build-system.md) | Manifest, caching, incremental builds |
| [docs/measurements.md](docs/measurements.md) | Recorded timings, sizes, and what they say |
| [CHANGELOG.md](CHANGELOG.md) | What changed in each release |
| [RELEASING.md](RELEASING.md) | How a release is cut and verified |
| [docs/adr/](docs/adr/) | Architecture Decision Records |

## Building and testing

```bash
cargo build            # build the compiler
cargo test             # run the full test suite
cargo clippy --all-targets
```

The test suite covers the lexer, the parser, diagnostic rendering, the JSON
schema, error recovery, and two robustness invariants: the parser never panics
on truncated input, and never panics on mutated input.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The short version: no phase may claim a
feature is implemented unless tests verify it, and memory safety is never
weakened to simplify an implementation.

## Releasing

See [RELEASING.md](RELEASING.md). `scripts/release.sh` refuses to build an
artifact from a dirty tree, a failing test suite or a draft specification, and
produces a stripped, checksummed tarball under `out/`.

## License

MIT OR Apache-2.0.
