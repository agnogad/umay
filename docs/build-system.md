# UMAY Build System

Status: design specification. `umay new`, `clean`, `doctor` and the project
layout rules are implemented in Phase 1; incremental compilation and caching
land in Phase 8.

## 1. Hard constraints

1. **No project-local `target/` directory, ever.** Not for caches, not for
   intermediates, not for lockfiles.
2. Only final build artifacts appear in `dist/`.
3. Compiler caches live in the operating system's user cache directory.
4. Identical inputs produce identical outputs (reproducible builds) where the
   target toolchain allows.

## 2. Project layout

```text
project/
├── umay.toml          # manifest
├── src/
│   └── main.um        # entry module
├── tests/             # integration tests (future)
└── dist/              # final artifacts only
```

Optional:

```text
project/
├── src/lib.um         # library entry module
└── modules/           # additional module roots
```

## 3. Manifest (`umay.toml`)

```toml
[package]
name = "my-project"          # required, unique, [a-z0-9_-]
version = "0.1.0"            # required, semver
edition = "2026"             # required, language edition
authors = ["A <a@example.com>"]
description = "..."

[build]
entry = "src/main.um"        # default: src/main.um
output-dir = "dist"          # default: dist, MUST NOT be named "target"
opt-level = 0                # 0 | 1 | 2 | 3
debug = true                 # emit debug info for opt-level 0/1
link = "dynamic"             # dynamic | static
target = "x86_64-linux"      # target triple

[dependencies]
# name = "1.0.0" or { path = "../lib", version = "1.0.0" }
```

Validation rules (implemented gradually, all of them errors):

- `output-dir` may not be `target` or contain `target` as a path component.
- `name` must be a valid identifier package name.
- `version` must parse as semver.
- `edition` must be a known edition.

## 4. Cache location

Determined in this order:

1. `$UMAY_CACHE_DIR`
2. Linux: `$XDG_CACHE_HOME/umay` or `~/.cache/umay`
3. macOS: `~/Library/Caches/umay`
4. Windows: `%LOCALAPPDATA%\umay\cache`

Implemented in `cache::cache_dir()`. `umay doctor` reports the resolved path and
whether it is writable. `umay clean` clears it along with `dist/`.

## 5. Content-addressed cache

### 5.1 Cache key

```text
key = H(
    compiler_version,
    compiler_backend,
    target_triple,
    opt_level,
    debug_flag,
    link_mode,
    edition,
    source_bytes,               // exact content of the module
    dependency_keys[],          // recursively, sorted by module path
    relevant_flags[],           // sorted
)
```

`H` is SHA-256, implemented in `compiler/src/sha256.rs` because the compiler has
no dependencies. The key is hex-encoded and used as the directory name under a
two-character prefix directory, so one directory never holds tens of thousands
of entries. The field list is also printed by `umay doctor`, so the inputs can be
read rather than inferred.

Each input is hashed with its name and its length before its bytes, so two
different sets of inputs cannot produce the same hash by concatenating to the
same bytes: `("a", "bc")` and `("a", "b"), ("c", "")` hash differently.

### 5.2 Cache entries

Each entry stores exactly one artifact plus its metadata:

```text
<cache>/v1/<key-prefix>/<key>/
├── meta.json     # inputs, timings, artifact list, diagnostics
└── <artifact>    # object file, or parsed HIR, depending on level
```

Cache levels are separate so a change in one does not invalidate the other:

- **parse level** — token stream + AST + syntax diagnostics,
- **analysis level** — HIR + types + ownership results (Phase 4+),
- **codegen level** — object file (Phase 5+).

### 5.3 Invalidation

Module-level. Changing module `m` invalidates `m` and every module that depends
on `m` transitively. Independent modules keep their cache entries.

### 5.4 Why content addressing works here

UMAY has no macros and no include mechanism, so a module's token stream is a pure
function of its bytes. That is the property that makes correct, fast, whole-file
content hashing possible — and it is why macro-like compile-time execution is
deliberately absent from v0.1.

## 6. Incremental build algorithm

```text
1. Load manifest, validate.
2. Discover modules from the entry point.
3. For each module: hash content, compute key, look up cache.
4. For each cache miss: compile to the required level (in parallel, bounded
   parallelism = min(cpu count, modules)).
5. Link: if all object keys are unchanged and the link inputs are unchanged,
   reuse the linked binary.
6. Copy the final artifact into dist/ atomically (write to a temp file in the
   cache, then rename).
7. Report a summary: modules compiled, modules cached, wall time.
```

Parallelism constraints: modules are independent for parsing and analysis; the
link step is serial. Any phase that needs global information (entry-point
resolution, cross-module type checking) runs after the parallel phase.

## 7. Reproducibility

- No timestamps, hostnames or local paths in object files or diagnostics.
- Cache keys never include the project's absolute path.
- Object file section ordering is deterministic.
- Diagnostics are sorted deterministically.

If a target toolchain (the system linker, for example) introduces
non-determinism, that is documented rather than hidden.

## 8. Commands and exit codes

| Command | Behaviour |
|---------|-----------|
| `umay new <name>` | create project layout (manifest, `src/main.um`, `dist/`) |
| `umay check [files]` | parse and analyse; `--format json`, `--explain`, `--color` |
| `umay build` | compile to an executable in `dist/`; `-O0`..`-O3` |
| `umay run` | build then execute, passing the program's exit status through |
| `umay asm` | print the generated assembly without building |
| `umay clean` | remove `dist/` contents and the cache |
| `umay fmt` | format source (Phase 7) |
| `umay lint` | run lints (Phase 7) |
| `umay lsp` | start the language server (Phase 7) |
| `umay explain <code>` | print the explanation for a diagnostic code |
| `umay ast <file>` | dump the syntax tree as JSON |
| `umay doctor` | toolchain, cache path, phase status |

Exit codes: `0` ok, `1` compile error, `2` usage, `3` I/O, `4` not implemented,
`5` code generation failed.

`umay build` emits three objects into `dist/` and links them: the program,
`umay_runtime.o`, and nothing else. The runtime is UMAY source
(`runtime/runtime.um`, embedded in the compiler with `include_str!`) compiled by
the same backend, so a program's binary is entirely UMAY apart from the C
functions it binds (`malloc`, `free`, `write`, `exit`). Its assembly and object
are kept next to the executable so a runtime problem is inspectable with the same
tools as a program problem. See [ADR 0010](adr/0010-runtime-in-umay.md).

## 9. Package manager (`upm`)

Deferred to Phase 8. Design position:

- `upm` is a separate executable built on the same manifest format.
- Vendored `deps/` remains inside the project; downloaded sources are stored in
  the external cache and linked by content hash, never copied into the project
  silently.
- A lockfile `umay.lock` records exact resolved versions and content hashes so
  builds are reproducible.
- No scripts run on install; dependency resolution is declarative.

## 10. What is implemented

Phase 8 (in progress):

- SHA-256 (`compiler/src/sha256.rs`), checked against the NIST vectors including
  the 55/56/64-byte padding boundary and the one-million-`a` stress case.
- The content-addressed cache (`compiler/src/build.rs`): key derivation, the
  `<root>/v1/<2 hex>/<hash>/` layout, atomic writes, and stats for `umay doctor`.
- `umay build` consults the cache for both the program's assembly and the
  runtime's, so an unchanged rebuild recompiles neither.
- An empty entry is treated as a miss rather than as an artifact. A build killed
  mid-write leaves a zero-length file, and serving it links an assembly with no
  text in it -- the program then fails to find `main`, which is a confusing place
  to discover a cache problem.
- `umay doctor` reports the entry count, the size, and the key's inputs.

Still to come: module-level invalidation across a multi-file program (the
compiler is single-file today), parallel compilation, and `upm`.

## 11. What Phase 1 implemented

- The canonical project layout produced by `umay new`.
- The external cache path (`cache::cache_dir`), with `$UMAY_CACHE_DIR` override.
- `umay clean` removing `dist/` contents and cache entries.
- `umay doctor` reporting the cache path and phase status.
- The `.gitignore` rule that keeps `dist/` empty but present.
