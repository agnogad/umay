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
target = "x86_64-linux"      # target triple: x86_64-linux, x86_64-macos
                             # or x86_64-windows (see compiler/src/target.rs)

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
    compiler_binary_fingerprint,
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

The `compiler_version` string is a claim about what the compiler is; the
`compiler_binary_fingerprint` is what it actually is on disk. The version alone
was not enough: editing the backend without bumping the version left the old and
new compiler sharing every key, so the rebuilt compiler served the previous
one's broken assembly from the cache -- the exact failure the `compiler` field
exists to prevent, defeated by a string that only changes when someone remembers
to change it. The fingerprint is the running executable's length and
modification time, computed once per process. A rebuild relinks the binary and
so changes the key; a no-op build relinks nothing and keeps it, so
incrementality survives.

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

### 5.4 Eviction

Content addressing makes every source edit create new keys, so an unpruned cache
grows for as long as the machine is used (1.3 GiB / 19,000 entries on one active
workstation). After a write to a *new* key directory, the cache removes whole
entry directories, oldest modification time first, until the total is at or
below 512 MiB. `UMAY_CACHE_MAX_BYTES` overrides the ceiling in bytes; `0`
disables eviction. `umay doctor` reports both the current size and the limit.
A miss caused by eviction costs a rebuild, never a wrong artifact.

### 5.5 Why content addressing works here

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
| `umay lock` | resolve dependencies and write `umay.lock` |
| `umay clean` | remove `dist/` contents and the cache |
| `umay fmt` | format source (Phase 7) |
| `umay lint` | run lints (Phase 7) |
| `umay lsp` | start the language server (Phase 7) |
| `umay explain <code>` | print the explanation for a diagnostic code |
| `umay ast <file>` | dump the syntax tree as JSON |
| `umay doctor` | toolchain, cache path, phase status |

Exit codes: `0` ok, `1` compile error, `2` usage, `3` I/O, `4` retired (no command
uses it), `5` code generation failed.

`umay build` emits three objects into `dist/` and links them: the program,
`umay_runtime.o`, and nothing else. The runtime is UMAY source
(`runtime/runtime.um`, embedded in the compiler with `include_str!`) compiled by
the same backend, so a program's binary is entirely UMAY apart from the C
functions it binds (`malloc`, `free`, `write`, `exit`). Its assembly and object
are kept next to the executable so a runtime problem is inspectable with the same
tools as a program problem. See [ADR 0010](adr/0010-runtime-in-umay.md).

## 9. Package manager (`upm`)

Partially implemented in Phase 8. What exists:

- The manifest (`compiler/src/manifest.rs`): a reader for the subset of TOML a
  manifest uses, plus the validation rules from section 3. A key or section it
  does not know is an error rather than being ignored, because a typo in a build
  file that is silently skipped builds the wrong thing.
- The lockfile (`compiler/src/lock.rs`): format 1, written in a fixed order so
  the same resolution produces the same bytes, and read back with the format
  version checked.
- `umay lock`: resolves the dependency *graph* -- a dependency's own
  dependencies are locked too -- and records the SHA-256 of each one's source. A
  dependency that changed is visible rather than silently adopted. A chain three
  deep is fully locked; a diamond locks the shared package once; a cycle is
  reported with the chain that closed it instead of expanding forever. Locked
  paths are relative to the project, so a lockfile does not depend on the machine
  that wrote it.
- Dependencies are reachable as modules. Naming `lib` in `[dependencies]` makes
  `mod lib;` resolve to `../lib/src/lib.um`, so the program can call
  `lib::answer()`. The manifest is found by walking up from the source file (a
  project entry point lives in `src/`, not beside `umay.toml`), and its paths are
  relative to the manifest. The cache key covers a dependency's source too, so
  editing one rebuilds the program that uses it.
- Version agreement: the dependency's own manifest says what it is, the parent's
  says what was asked for, and a disagreement is reported with both numbers
  rather than resolved in either direction.
- `umay build` and `umay run` with no file argument use `[build] entry`.
- `output-dir` naming `target`, in any spelling, is refused.
- **Fetching from a registry** (`compiler/src/upm.rs`). A dependency with no
  `path` is fetched from a registry named by `UMAY_REGISTRY` -- one or more
  directories separated by the platform path separator. Each registry holds
  packages as `<name>/<version>/` with a `umay.toml` and a `src/`; a fetch copies
  the chosen version into the project's `deps/<name>/` and records its SHA-256
  in the lockfile. `umay lock` walks the whole graph, so a fetched dependency's
  own dependencies are fetched too. An empty version (`lib = ""`) asks for the
  newest the registry holds, compared by number so `1.10.0` beats `1.9.0`. A
  name that could escape `deps/` (`../x`, `a/b`) is refused, and a package with
  no manifest, a missing version, or no registry named is an error that says
  which, never a silent no-op.

The registry is a directory of packages, not a network service. That is a
choice, not a limitation to hide: the fetch, the version selection and the
content hash are the parts that must be right, and they are exercised in full
against a directory on disk (`compiler/tests/phase8_upm.rs` builds a throwaway
registry and drives the fetcher against it). A hosted registry is the same code
behind one more layer of transport, and is deliberately not built yet.

Design position for the rest:

- `upm` is a separate executable built on the same manifest format.
- Fetched sources live in the project's `deps/`, pinned by content hash in
  `umay.lock`; a re-fetch replaces old content so a registry update is picked up
  rather than shadowed by a stale copy.
- A lockfile `umay.lock` records exact resolved versions and content hashes so
  builds are reproducible.
- No scripts run on install; dependency resolution is declarative.

### The linked executable is cached too

A warm build used to skip codegen but still ran two `as` processes and one `cc`.
The executable is now stored under a key made of the program assembly and the
runtime assembly, so an unchanged rebuild links nothing: on
`examples/http-router.um`, 0.30s down to 0.18s.

The restored file is written with mode 0755. `std::fs::write` creates a file with
no execute bit, so the first version produced a build that reported success and a
`umay run` that failed with "Permission denied" -- the failure pointing at the
shell rather than at the cache. The regression test removes the executable
between builds so that the copy really does come from the cache; without that the
file is still the linker's own output and the bug is invisible.

### What runs in parallel

The program object and the runtime object are assembled at the same time. They do
not read each other, and `as` prints nothing unless it fails, so the two runs
cannot interleave their output; the caller prints the failure after both finish.
That ordering matters more than the speedup: without it the same build could say
different things on different runs.

Compiling the two programs in parallel was measured and rejected: a cold build is
around 0.18s, dominated by the link through `cc`, so it would buy milliseconds in
exchange for shared mutable state.

### Modules in the key

The key covers every module file that went into the assembly, not only the entry
point. Understanding why: the first version hashed the entry file alone, so
editing a module was a cache hit — the program kept its old behaviour while the
source said otherwise, which is the worst kind of stale. The module files are
named by their path relative to the entry point, so the key does not depend on
where the project sits and reproducible builds still hold.

### What is not cached, deliberately

A build that produced a warning is not cached. The cache holds the assembly text
and nothing else, so a hit has no diagnostics to go with it; caching a warning
build made the warning appear on the first build and vanish on the second. A
warning the user sees once is one they cannot trust, so the build runs again and
says the same thing every time.

## 10. What is implemented

Phase 8 (done):

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
- `upm` fetching (`compiler/src/upm.rs`): a dependency with no `path` is fetched
  from the registry named by `UMAY_REGISTRY` into `deps/<name>/` and pinned by
  content hash, so a build verifies what it links.

Known coarseness, deliberately not addressed: per-module reuse (the key covers
the whole module tree, so an edit to any module recompiles the program -- correct,
but coarse), and a parallel frontend (measured and rejected: the link dominates a
cold build, so threads would add shared-state risk for a few milliseconds).

## 11. What Phase 1 implemented

- The canonical project layout produced by `umay new`.
- The external cache path (`cache::cache_dir`), with `$UMAY_CACHE_DIR` override.
- `umay clean` removing `dist/` contents and cache entries.
- `umay doctor` reporting the cache path and phase status.
- The `.gitignore` rule that keeps `dist/` empty but present.
