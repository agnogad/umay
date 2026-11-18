# ADR 0023 — Separate-process macro host

**Status:** proposed
**Phase:** v2 foundation
**Supersedes:** nothing

## Context

ADR 0022 ships declarative token macros in the compiler crate and leaves the
proc-macro/plugin ABI design-only. Its two hard rules still stand: the compiler
crate has **zero dependencies**, and `[lints.rust] unsafe_code = "deny"`
(ADR 0001). A Rust-style proc-macro dylib needs both foreign code and `unsafe`,
so it cannot live in the compiler process. The README "Not yet" list still says
"no proc-macro/plugin ABI."

Exploration found a route that keeps those rules intact: run arbitrary macro
code in a **separate, optional binary** and talk to it over a versioned,
newline-delimited JSON protocol on stdin/stdout. The compiler stays a pure
transformer that speaks a stable wire format; all execution risk, native
dependencies and platform-specific sandbox code stay in the host process. This
record specifies that boundary. It is a **proposed direction**, not an accepted
ABI: nothing here is implemented, and ADR 0022 remains authoritative for what
ships today. Acceptance happens when a host binary exists and passes the
reproducibility and isolation gates in §9 — until then the compiler gains no
dependency, no `unsafe`, and no runtime macro loading.

## Decision

### 1. Process model and hard invariants

- `umay-macro-host` is a **separate binary**, launched by the compiler only when
  a macro package needs it. It is optional: builds using only the builtin
  expander never spawn it and never link its dependencies.
- The **`compiler/` crate stays dependency-free and `unsafe`-free** (ADR 0001).
  It owns only the wire codec, span remapping, cache lookup and package
  verification (verification uses the existing `compiler/src/sha256.rs`, no new
  dependency). No dylib is loaded into the compiler, and no foreign macro code
  shares its address space.
- Native dependencies, FFI shims and OS sandbox primitives live in the host
  crate, behind its own lint policy. If the workspace `unsafe_code = "deny"`
  lint applies there, the small platform sandbox shim is isolated in a dedicated
  helper crate; in no case does that `unsafe` enter the compiler.
- stdout of the host is **protocol-only**. Logs and crash text go to stderr,
  which the compiler captures to a bounded tail for diagnostics.

### 2. Wire protocol and version negotiation

Frames are single-line UTF-8 JSON objects terminated by `\n`; JSON escaping
forbids raw newlines inside strings, so the line is the counted byte length
(ADR 0012). The compiler rejects a frame over a size cap (default 16 MiB)
*before* parsing it. Unknown keys are ignored; required keys are validated.

Handshake, before any request:

```json
{"kind":"hello","protocol":1,"host_version":"0.1.0","caps":["span-v1","sign-v1","sandbox-v1"]}
{"kind":"hello_ok","protocol":1,"compiler_features":["expand","diagnostics-v2"]}
```

On mismatch the host answers `{"kind":"hello_err","reason":"unsupported_protocol","supported":[1]}`
and exits. Rules:

- The **major protocol number must match exactly**; minors are additive and
  carried as capability strings (`caps`). The compiler never guesses a "close
  enough" version and never downgrades silently.
- A request may use only capabilities the host advertised. Missing capability
  plus a request that needs it is a protocol error, not a best-effort run.
- Requests carry a `u64` id; responses echo it. Pipelining is permitted but the
  default client keeps one request in flight so a bad host cannot interleave
  partial replies.

### 3. Request/response envelopes

Request (payload varies by `kind`):

```json
{"v":1,"id":7,"kind":"expand",
 "payload":{"package":"sha256:<checksum>","macro":"derive::my_macro",
            "args":<token-tree>,"definition":<token-tree>,
            "invocation_span":<span>,"crate":"logical-crate-name",
            "features":["gui"]}}
```

Responses, one `kind` per outcome:

- `{"v":1,"id":7,"kind":"expanded","payload":{"tokens":<token-tree>,"spans":<span-table>,"diagnostics":[],"cache_key":"sha256:..."}}`
- `{"v":1,"id":7,"kind":"diagnostics","payload":{"diagnostics":[...],"tokens":null}}` —
  a macro-level error (matcher mismatch, invalid derive target) reported as data.
- `{"v":1,"id":7,"kind":"error","payload":{"code":"E0136","message":"host exited 101"}}` —
  a host-level failure; see §8.

Diagnostics are the structured payload of ADR 0003, never formatted text:
`code`, `severity`, `message`, and a `spans` array whose entries are
`{file, line, col, byte_start, byte_end, site, expansion_chain}`. The
`E0125`–`E0129` range stays with declarative macros (ADR 0022); host-level
failures reserve `E0135`–`E0139` (`E0130`–`E0134` are already struct parser
diagnostics in `compiler/src/explain.rs`). A diagnostic raised at a generated span is
remapped by the compiler to the original `invocation_span` and labelled
`in expansion of <macro>`, with the expansion chain kept for `umay expand`
traces — the same rule ADR 0022 §3 applies to the builtin engine.

### 4. Token model with spans

A token is `{"kind":"ident"|"keyword"|"literal"|"punct"|"group",
"text":"...","span":<span>}`; a token tree is a nested array of tokens and
groups. Spans are logical, never absolute paths:

```
span = { file: "<crate-relative id>", start: <byte>, end: <byte>,
         site: "def" | "call", ctx: <u32>, expansion: <u32> }
```

- **def-site** tokens come from the macro definition; they carry the
  definition's span and a fresh syntax context (`ctx`), so an identifier the
  macro introduces cannot be captured by a caller identifier of the same text.
  `$crate` becomes a reserved token resolving to the defining crate.
- **call-site** tokens are spliced from the invocation; they keep the caller's
  span and syntax context, so arguments still resolve where the user wrote them.
- Hygiene is resolution by `(text, ctx)`. This is the "def-site vs call-site
  span distinction and fresh names" that ADR 0022 defers to the next increment;
  the host protocol is where it lands without touching the builtin engine.

### 5. Deterministic expansion and content-addressed cache

Expansion must be a pure function of its request: no clock, no randomness, no
environment lookups, no absolute paths, no hash-map iteration order in output.
JSON is emitted canonically (sorted keys, fixed whitespace, integers only).
Two runs with identical inputs produce **byte-identical** token streams.

The cache key is
`sha256(protocol || host_version || package_checksum || canonical_request || compiler_feature_set)`.
A hit returns the stored token tree without launching the host, and is stored
as a build artifact alongside `Artifact::MacroExpansion` from ADR 0022, naming
the file by its hash rather than by mtime or path (ADR 0001 reproducibility,
`docs/build-system.md` §7). The cache is advisory: a miss or a corrupt entry is
recomputed, never trusted. `umay expand --verify` re-expands and compares bytes;
a mismatch quarantines that package/host pair (§8).

### 6. Sandboxing

Each expansion runs in a **fresh, short-lived process** with the least
authority that still lets a macro run:

- **No network** — network namespace/firewall; a package that opens a socket
  fails and is reported as a sandbox violation.
- **No filesystem writes outside one per-invocation temp dir** — package and
  crate sources are mounted read-only; the temp dir is deleted on every exit,
  success or failure. The host returns bytes; it never writes the compiler's
  cache or build directory itself.
- **CPU/memory limits** — CPU-time rlimit plus a wall-clock watchdog, an
  address-space cap, and a scrubbed environment with no inherited descriptors
  except stdio. The compiler kills the whole process group on timeout.
- Where the platform lacks these primitives, external macros are **opt-in and
  must be signed**; if signing is unavailable, the host is disabled rather than
  run sandboxed-in-name-only.

### 7. Signed packages and reproducible builds

A package is an archive plus `manifest.json`:

```
{ name, version, protocol: ">=1,<2", capabilities: [...],
  files: [{ path, sha256 }, ...], key_id, signature }
```

The compiler verifies the signature over the canonical manifest (minus
`signature`) against its key set, then checks every file checksum, **before**
launching the host. Checksums cover the files the host actually reads. Unsigned
packages are accepted only in the trusted-local rollout stage with an explicit
opt-in flag and never in release or CI. Registry downloads use the ADR 0016
archive flow; the manifest is the integrity and provenance boundary.

Reproducibility follows from §5 plus a fixed host version and package checksum
in the key: a given source tree and toolchain produce the same expanded bytes
on every machine and every run. Path-like inputs are crate-relative logical ids,
never build-machine paths. The existing byte-for-byte `umay expand` check
(`docs/v2/benchmarks.md`, Phase 8) is the gate; the host is accepted only when
it passes on all CI targets.

### 8. Failure modes

| Failure | Detection | Behavior |
|---------|-----------|----------|
| Host missing / protocol mismatch | handshake | fail closed: builtin fallback if policy allows, else `E0135` |
| Malformed frame / unknown required key / cap violation | codec | session dropped, `E0135` |
| Per-request timeout | watchdog | kill process group, delete temp dir, `E0136` |
| Crash or nonzero exit | exit status | `E0136` with code and bounded stderr tail |
| OOM / address-space cap | limit | kill host, `E0136` |
| Network or out-of-temp-dir write | sandbox | kill host, `E0137`, package quarantined |
| Signature or checksum mismatch | pre-launch verify | reject before any code runs, `E0138` |
| Non-deterministic output (`--verify` mismatch) | byte compare | `E0139`, host disabled for that package |

A host failure never corrupts compiler memory or the build cache: partial stdout
is discarded, the temp dir is removed, and the compiler either falls back to the
builtin expander (trusted local macros only) or reports a diagnostic. No failure
path silently returns unexpanded source.

### 9. Rollout path

1. **Builtin expander only** (today, ADR 0022): declarative token macros in the
   compiler crate, no host, both hard rules intact.
2. **External host for trusted local macros**: opt-in `--macro-host <path>`;
   unsigned packages allowed only with an explicit local-only flag; the
   protocol, sandbox and determinism gates in §§2–6 must pass first. No registry.
3. **Signed packages**: manifest verification becomes the default for
   third-party macros served by the ADR 0016 registry; stage 2's unsigned path
   is removed from release builds.

This ADR becomes accepted when stage 2 ships with the §5/§7 gates green. Nothing
in it changes what ADR 0022 implements in the meantime.

## Consequences

- Arbitrary macro code becomes possible without breaking the compiler's
  zero-dependency / no-`unsafe` rules; a host crash is a diagnostic, not a
  compiler crash.
- A stable versioned protocol is a long-term compatibility commitment: every
  server change must respect `protocol` and `caps`.
- Hosts add a process launch and a JSON codec per uncached expansion; the
  content-addressed cache and one-in-flight default keep that off the common
  path.
- Sandbox quality varies by OS; unsupported platforms lose external macros
  rather than getting a weaker guarantee.
- Signed packages introduce a key-management and revocation duty for the
  project (ADR 0016).
- Def-site/call-site spans and fresh names finally give macros real hygiene,
  but only for host-driven macros until the builtin engine is upgraded.

## Alternatives considered

**In-process dylib ABI now.** Rejected: foreign loading and `unsafe` break both
hard rules (ADR 0001), a macro panic or memory bug takes down the compiler, and
there is no sandbox boundary to hang versioning or signing on. This was the
deferred option in ADR 0022; the process boundary supersedes it as the preferred
route without relaxing the rules.

**No macros beyond the declarative subset.** Rejected: Phase 8 is an approved
v2 deliverable, derives are already partially in place (ADR 0022), and real
codegen macros (derive `impl`s, procedural attributes) have no other path.

**WebAssembly sandbox inside the compiler process.** Deferred, not rejected:
it removes OS-specific sandbox code but adds a runtime dependency and a new
ABI to the compiler story; it can be offered later as a second `kind` of host
behind the same protocol without changing the envelope format.

**`build.rs` / compile-time codegen hooks.** Rejected: arbitrary code runs with
the full privileges of the build, reproducibility is accidental rather than
enforced, and there is no per-package versioning or isolation.
