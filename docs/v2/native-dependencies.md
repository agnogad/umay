# Native dependencies and build scripts — v2 design draft

**Status:** design draft for v2. **Not implemented.** This is the model that the
FFI spike ([ffi-spike.md](ffi-spike.md)) and the webview native shells
([webview-decision.md](webview-decision.md)) assume.

## 1. Why

Today the only native link is a hardcoded prefix scan: `Target::system_libraries`
looks for `sqlite3_` in the assembly and adds `-lsqlite3`
(`compiler/src/target.rs:391-406`). There is no way to name a system library,
give include paths, or generate bindings. The manifest reader treats unknown
sections and keys as **errors** (`compiler/src/manifest.rs:112-120, 202-205,
249-252`), so `[native]` must be added deliberately, not silently ignored.

## 2. Manifest section

```toml
[native.sdl3]
pkg-config = "sdl3"                # preferred discovery
libs = ["SDL3"]                    # fallback / explicit -l names
include-paths = ["vendor/include"]
lib-paths = ["vendor/lib"]
headers = ["SDL3/SDL.h"]           # inputs to umay-bindgen
link = "dynamic"                   # dynamic | static

[native.sdl3.target.aarch64-w64-mingw32]   # override, key = target triple
pkg-config = ""                    # no pkg-config on Windows
libs = ["SDL3"]
lib-paths = ["vendor/win-arm64"]
```

- Discovery: run `pkg-config --cflags --libs [--static]`; merge its flags with
  the explicit `libs`/`include-paths`/`lib-paths` (deduped, explicit wins on
  conflict). `link = "static"` uses `pkg-config --static` and must agree with
  `[build] link` (`manifest.rs:45`, `read_build` at 237-244); a mismatch is an
  error. Explicit flags alone are valid when `pkg-config` is absent.
- Per-platform overrides are keyed by the exact triples in `Target::ALL`
  (`compiler/src/target.rs:354-362`). Rule: base keys are defaults; the single
  entry matching the current triple overrides them. No implicit chaining
  between overrides.
- The hand-written TOML reader (`manifest.rs:99-153`, `read_*` helpers at
  174-292) and the writer (`to_toml`, 492-529) must be extended for `[native]`
  and the nested `target` tables. Unknown keys remain errors — a typo must not
  silently drop a library.

## 3. Build scripts

A root project may declare `build-script = "scripts/build.sh"` under `[build]`.
Rules:

- Runs after manifest validation, before compilation, cwd = project root.
- Environment: `UMAY_TARGET`, `UMAY_CACHE_DIR`, `UMAY_MANIFEST_DIR`; nothing else
  is forwarded (no ambient secrets).
- Communicates by stdout directives only, `umay:` prefixed:
  `umay:link-lib=…`, `umay:link-search=…`, `umay:include=…`,
  `umay:bindgen=<header>:<out.um>`, `umay:rerun-if-changed=<path>`.
- May emit generated `.um` sources and flag files **into the OS user cache**,
  never into the project (`docs/build-system.md:9-12`, hard constraint 1). It
  cannot modify project sources. Output paths must be under the cache root;
  `..` and absolute paths outside it are rejected.
- Reruns when the script, any declared `rerun-if-changed` input, or the target
  changes. A build script is not run on install — consistent with `upm`'s
  current "no script runs on install" rule (`docs/build-system.md:274, 284`).

## 4. Caching and reproducibility

- Resolved native flags and generated bindings are inputs to the build cache:
  fold them into the `extra` inputs of `CacheKey::derive_for`
  (`compiler/src/build.rs:96-137`), alongside the existing runtime-source extra
  input. A library version or header change then rebuilds.
- Reproducibility: no absolute paths in keys (`docs/build-system.md:177-178`).
  Hash header and script *bytes*, and normalize pkg-config output relative to a
  root before hashing. Include the pkg-config version string.
- Limitation to document, not hide: a system library upgraded in place with
  identical metadata is invisible to the key; pin header content where possible
  and let `umay doctor` print the resolved versions. No remote checksum trust
  in v2 (compare ADR 0016: the lockfile pins content but first fetch trusts the
  transport).

## 5. Interaction with UPM and the build system

- `upm` fetches UMAY packages into `deps/<name>/`, pinned by content hash
  (`compiler/src/upm.rs:119-196, 259-262`; `docs/build-system.md:243-260`).
  `[native]` entries are **not** UPM packages: they name locally installed
  system libraries, discovered at build time.
- A UPM package may declare its own `[native]` requirements; these are merged
  into the root project's resolution (transitively, like `umay lock` walks the
  graph — `docs/build-system.md:225-231`) and are pinned by the package's
  existing content hash. A dependency's build script is **not** executed
  automatically; only the root project's script runs.
- `Target::system_libraries` (`target.rs:391-406`) stays as a built-in default
  so existing SQLite builds do not change. `[native]` generalizes it; a
  `[native.sqlite3]` entry overrides the heuristic.
- `umay doctor` prints resolved native deps, pkg-config commands, generated
  bindings and the build script path.

## 6. Missing-library errors

Dedicated diagnostic in a reserved build-resolution code range, exit status 3
(I/O — `docs/build-system.md:203-204`). The message must state: library name,
current target triple, every lookup attempted (`pkg-config <name>`, each
`lib-paths`/`include-paths` and per-target override), and the exact manifest key
to set or package to install. Never degrade to a silent "library not found" at
link time. Generation of a binding for a header that does not exist is the same
class of error and names the header path and the target.

## 7. Security boundary

Build scripts execute arbitrary code. This is a trust boundary:

- Only the root project's declared script runs. Scripts in `upm` dependencies
  are data and are never executed.
- No privilege escalation, no network sandbox in v2 (documented as a known
  limit). Environment is the minimal set in §3.
- Script stdout is parsed as **data**: flags and paths are validated, paths
  confined to the cache root, no shell re-evaluation of emitted strings.
- The script's bytes are hashed into the lockfile/build key, so a review can
  see when the executable code of a build changed.
- FFI misuse in the resulting program is not a compiler vulnerability
  (`SECURITY.md:35-37`); a build script escaping its declared outputs is.
