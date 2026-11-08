# UMAY Diagnostics

Status: **implemented** in Phase 1 for lexical and syntax diagnostics. Later
phases emit new codes against the same contract.

Diagnostics are a defining feature of UMAY, not an afterthought. They are
designed for two audiences with equal weight: humans reading a terminal, and
programs (editors, CI, AI agents) consuming structured data.

## 1. Principles

1. **Stable codes.** Every diagnostic has a code that never changes meaning.
2. **Always a cause.** A diagnostic says what went wrong, not just where.
3. **Explanations on demand.** `umay explain CODE` documents every code.
4. **Machine-readable by default.** `--format json` is a first-class output.
5. **No speculative fixes.** A suggestion is emitted only when the compiler has
   verified the replacement is type-correct and memory-safe.
6. **No duplicate reports for one byte range.**
7. **Deterministic order.** Sorted by file, then start offset, then severity.

## 2. Code ranges

| Range | Area |
|-------|------|
| `E0001`–`E0099` | lexical errors |
| `E0100`–`E0399` | syntax/structural errors |
| `E1001`–`E1999` | name resolution and type checking (Phase 2) |
| `E2001`–`E2999` | ownership and borrow checking (Phase 4) |
| `E3001`–`E3999` | code generation and linking (Phase 5) |
| `W0001`–`W0999` | warnings |

Codes are reserved in the specification documents before they are emitted, so
documentation and tooling can reference them stably.

## 3. Diagnostic structure

```rust
Diagnostic {
    code: &'static str,        // e.g. "E0101"
    severity: Severity,        // error | warning | note | help
    message: String,           // one line, imperative or descriptive
    span: Option<Span>,        // primary location
    labels: Vec<Label>,        // secondary spans with messages
    cause: Option<String>,     // root cause in plain language
    explanation: Option<String>, // why the rule exists
    suggestions: Vec<Suggestion>,// verified, machine-applicable fixes
}

Suggestion {
    description: String,       // "did you mean `print`?"
    span: Span,                // exact range to replace
    replacement: String,       // replacement text
    detail: Option<String>,    // optional extra context
}
```

`span` is optional only for whole-program diagnostics (e.g. a missing entry
point). Everything else must point at a location.

## 4. Human output

Canonical layout:

```text
error[E0101]: unknown name `prnt`
    --> src/main.um:2:5
    |
  2 |     prnt("hi")
    |     ^^^^ not found in this scope
    |
    = cause: no function named `prnt` is in scope
    = help: did you mean `print`?
       src/main.um:2:5   prnt -> print
```

Rules:
- the gutter width adapts to the largest line number shown,
- carets underline the exact byte range, using character counts (not bytes) so
  non-ASCII source aligns correctly,
- at most one caret row is drawn per source line: the primary label wins,
  otherwise the first secondary label on that line,
- context lines are limited; elisions are marked with `...`,
- colour is applied only when stdout is a terminal and `NO_COLOR` is unset
  (or when `--color always` is passed),
- `--explain` adds the `= why:` explanation paragraph.

## 5. JSON schema (version 1)

`umay check --format json` prints exactly one JSON object to stdout and nothing
else. Machine consumers must ignore unknown fields; additive fields do not bump
the version. Removing or renaming a field requires a version bump.

```json
{
  "version": 1,
  "diagnostics": [
    {
      "code": "E0101",
      "severity": "error",
      "message": "unknown name `prnt`",
      "file": "src/main.um",
      "line": 2,
      "column": 5,
      "end_line": 2,
      "end_column": 9,
      "span": { "start": 16, "end": 20 },
      "cause": "no function named `prnt` is in scope",
      "explanation": "Name resolution failed: ...",
      "labels": [
        {
          "file": "src/main.um",
          "line": 2,
          "column": 5,
          "message": "not found in this scope"
        }
      ],
      "suggestions": [
        {
          "description": "did you mean `print`?",
          "replacement": "print",
          "file": "src/main.um",
          "span": { "start": 16, "end": 20, "file": "src/main.um" },
          "detail": null
        }
      ]
    }
  ]
}
```

Field contract:

| Field | Type | Meaning |
|-------|------|---------|
| `version` | integer | schema version, currently `1` |
| `code` | string | stable diagnostic code |
| `severity` | string | `error`, `warning`, `note`, `help` |
| `message` | string | one-line description |
| `file` | string \| null | display path |
| `line` / `column` | integer \| null | 1-based start position |
| `end_line` / `end_column` | integer \| null | 1-based end position (exclusive) |
| `span.start` / `span.end` | integer | byte offsets into the file |
| `cause` | string \| null | root cause |
| `explanation` | string \| null | longer rationale |
| `labels[]` | array | secondary locations |
| `suggestions[]` | array | verified fixes |

Notes:
- `line` and `column` count characters, not bytes; `span` counts bytes.
- An empty result is exactly `{"version":1,"diagnostics":[]}`.
- The JSON emitter has no external dependency, so the byte-level shape is stable
  and reviewable in diffs.

## 6. Suggestion policy

A suggestion is emitted only when all hold:

1. the replacement parses,
2. the replacement is type-correct in context,
3. the replacement does not introduce a memory-safety violation.

Concretely this forbids:
- inventing a name for an unknown identifier (only renames to a name the
  compiler actually resolved are suggested),
- adding `unsafe` to silence an error,
- inserting `.unwrap()` or `.clone()` to silence an ownership error,
- replacing a borrow error with a raw-pointer cast.

The current implementation emits suggestions only for keyword-as-identifier
(`E0110`, offering `name_`). More suggestions arrive with Phase 2, where the
compiler has the information needed to verify them.

## 7. Explain database

`umay explain CODE` prints the summary, a wrapped explanation and, where useful,
a code example. The database is a static table with a unit test asserting:

- codes are unique,
- every entry has a non-empty summary and explanation,
- every code the frontend can emit has an entry.

`umay explain` with no argument lists every known code.

## 8. Exit codes

| Code | Meaning |
|------|---------|
| `0` | success |
| `1` | at least one error diagnostic |
| `2` | usage error (bad flags, missing arguments, unknown command) |
| `3` | I/O error (unreadable file, unwritable cache) |
| `4` | retired (no command exits with it any more) |
| `5` | code generation failed (backend error, assembler rejected the output) |

Warnings never change the exit code.

## 9. Not yet implemented

- `--output-format github` / `sarif` emitters,
- rustc-style `--error-format=short`.

These are additive and do not affect the version 1 schema. Multi-file
compilation with entry-point resolution (modules) and LSP diagnostic publication
are implemented and covered by tests.
