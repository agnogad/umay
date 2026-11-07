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
through 2** of the roadmap: a complete lexer, parser and semantic analyser with
the AI-first diagnostics infrastructure. There is no code generator yet, so
`umay build` and `umay run` exist as commands but report that they are not
implemented.

| Phase | Area | Status |
|-------|------|--------|
| 0 | Architecture and specification | **complete** |
| 1 | Lexer and parser | **complete** |
| 2 | Semantic analysis and type checking | **complete** |
| 3 | HIR and MIR | not started |
| 4 | Ownership and borrow checking | not started |
| 5 | Custom x86_64 backend | not started |
| 6 | Runtime and standard library | not started |
| 7 | AI developer experience | not started |
| 8 | Build system and packages | not started |
| 9 | Stabilization | not started |

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
`3` I/O error, `4` not implemented (used by `build`, `run`, `fmt`, `lint`, `lsp`).

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
| [docs/language-specification.md](docs/language-specification.md) | Normative syntax and semantics, v0.1 |
| [docs/type-system.md](docs/type-system.md) | Types, inference, generics, traits |
| [docs/memory-model.md](docs/memory-model.md) | Ownership, borrowing, lifetimes, destruction |
| [docs/compiler-architecture.md](docs/compiler-architecture.md) | Pipeline, modules, interfaces |
| [docs/diagnostics.md](docs/diagnostics.md) | Codes, schema, JSON contract |
| [docs/build-system.md](docs/build-system.md) | Manifest, caching, incremental builds |
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

## License

MIT OR Apache-2.0.
