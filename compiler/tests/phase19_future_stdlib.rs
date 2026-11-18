//! Phase 19: the async core types in `stdlib/future.um` (ADR 0019).
//!
//! The first group uses the phase2 in-process harness (lex -> parse ->
//! `sema::analyze`). It feeds the *real* `stdlib/future.um` text into each test
//! program so the declarations under test are the ones shipped in the stdlib,
//! not a copy. A later group goes through `analyze_source` and the `umay check`
//! CLI to prove the file is actually wired into `PORTABLE_PRELUDE`.
//!
//! The ADR spells `trait Future { type Output; ... }`. UMAY traits cannot
//! declare associated types and `Self` in type position resolves to Error, so
//! the output type is a generic parameter here, matching `Iterator<T>`.

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;

const FUTURE_UM: &str = include_str!("../../stdlib/future.um");

struct Checked {
    diagnostics: Diagnostics,
}

fn analyze(src: &str) -> Checked {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(lex_errors.is_empty(), "test source failed to lex: {:?}\n---\n{src}", lex_errors);
    let parsed = parser::parse(tokens, Vec::new(), id);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "test source failed to parse: {parse_errors:?}\n---\n{src}");
    let out = sema::analyze(&parsed.program, &map);
    Checked { diagnostics: out.diagnostics }
}

fn error_codes(src: &str) -> Vec<&'static str> {
    analyze(src)
        .diagnostics
        .items()
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| d.code)
        .collect()
}

#[track_caller]
fn ok(src: &str) {
    let codes = error_codes(src);
    assert!(codes.is_empty(), "expected no errors, got {codes:?}\n---\n{src}");
}

/// The real `future.um` declarations followed by a user program.
fn with_future(body: &str) -> String {
    format!("{FUTURE_UM}\n{body}")
}

// ---------------------------------------------------------------------------
// (a) Poll variants parse and type-check.
// ---------------------------------------------------------------------------

#[test]
fn poll_ready_and_pending_typecheck() {
    // `None` needs an annotation to name its `T`; the same holds for `Pending`.
    ok(&with_future(
        "fn main() {
            let ready: Poll<i64> = Poll::Ready(1i64)
            let pending: Poll<i64> = Poll::Pending
            let _ = ready
            let _ = pending
        }
        ",
    ));
}

// ---------------------------------------------------------------------------
// (b) A user future implements `Future<i64>`; a direct `poll` call returns Ready.
// ---------------------------------------------------------------------------

#[test]
fn ready_future_impl_and_direct_poll_call_typecheck() {
    ok(&with_future(
        "struct ReadyFuture { value: i64 }
        impl Future<i64> for ReadyFuture {
            fn poll(&mut self, cx: &mut Context) -> Poll<i64> {
                let _ = cx
                return Poll::Ready(self.value)
            }
        }
        fn main() {
            let mut f: ReadyFuture = ReadyFuture { value: 1i64 }
            let mut cx: Context = make_context()
            match f.poll(&mut cx) {
                Poll::Ready(v) => { let _ = v },
                Poll::Pending => { },
            }
        }
        ",
    ));
}

// ---------------------------------------------------------------------------
// (c) make_context() type-checks.
// ---------------------------------------------------------------------------

#[test]
fn make_context_typechecks() {
    ok(&with_future(
        "fn main() {
            let cx: Context = make_context()
            let _ = cx
        }
        ",
    ));
}

// ---------------------------------------------------------------------------
// The declarations are compiled into every program via `PORTABLE_PRELUDE`.
// ---------------------------------------------------------------------------

#[test]
fn future_types_resolve_from_the_prelude() {
    // No declarations here: `Poll`, `Context` and `make_context` must come from
    // the prelude. A missing `include_str!` or a parse error in `future.um`
    // would surface as an unknown-name / unknown-type error.
    let (diagnostics, map) = umayc::analyze_source(
        "t.um",
        "fn main() {
            let ready: Poll<i64> = Poll::Ready(1i64)
            let pending: Poll<i64> = Poll::Pending
            let cx: Context = make_context()
            let _ = ready
            let _ = pending
            let _ = cx
        }
        ",
    );
    if diagnostics.has_errors() {
        eprintln!("{}", umayc::render_all_full(&diagnostics, &map, false, false));
    }
    let codes: Vec<_> = diagnostics
        .items()
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| d.code)
        .collect();
    assert!(codes.is_empty(), "future types did not resolve from the prelude: {codes:?}");
}

#[test]
fn cli_check_accepts_a_program_using_the_future_types() {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("umay-phase19-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path: PathBuf = dir.join("main.um");
    std::fs::write(
        &path,
        "struct ReadyFuture { value: i64 }
         impl Future<i64> for ReadyFuture {
             fn poll(&mut self, cx: &mut Context) -> Poll<i64> {
                 let _ = cx
                 return Poll::Ready(self.value)
             }
         }
         fn main() {
             let mut f: ReadyFuture = ReadyFuture { value: 1i64 }
             let mut cx: Context = make_context()
             match f.poll(&mut cx) {
                 Poll::Ready(v) => { let _ = v },
                 Poll::Pending => { },
             }
         }
         ",
    )
    .expect("write temp source");

    let output = Command::new(env!("CARGO_BIN_EXE_umay"))
        .arg("check")
        .arg(&path)
        .output()
        .expect("run umay check");
    let passed = output.status.success();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        passed,
        "umay check failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
