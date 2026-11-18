//! Phase 12: least-privilege closure capture modes (ADR 0017 §2).
//!
//! A Copy capture is borrowed rather than copied into the environment: shared
//! when the body only reads it, mutable when the body writes it. Non-Copy
//! captures keep the by-value behaviour, and writing one is E1020.
//!
//! Harness copied from `phase10_closures.rs`.

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;

// ---------------------------------------------------------------------------
// In-process parse + sema helpers
// ---------------------------------------------------------------------------

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

#[track_caller]
fn err(src: &str, code: &str) {
    let codes = error_codes(src);
    assert!(codes.contains(&code), "expected error {code}, got {codes:?}\n---\n{src}");
}

// ---------------------------------------------------------------------------
// Build + run helpers
// ---------------------------------------------------------------------------

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> &'static str {
    env!("CARGO_BIN_EXE_umay")
}

fn toolchain_available() -> bool {
    if cfg!(windows) {
        return false;
    }
    let ok = |name: &str| {
        Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase12_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn run_stdout(name: &str, source: &str) -> Option<String> {
    if !toolchain_available() {
        eprintln!("skipping {name}: `as` or `cc` is not available");
        return None;
    }
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        build.status.success(),
        "build failed for {name}:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    let exe = dir.join("dist").join(if cfg!(windows) { "main.exe" } else { "main" });
    let out = Command::new(&exe).current_dir(&dir).output().expect("run built program");
    assert!(
        out.status.success(),
        "{name} exited with {:?}:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[track_caller]
fn expect_stdout(name: &str, source: &str, expected: &str) {
    if let Some(out) = run_stdout(name, source) {
        assert_eq!(out, expected, "{name} stdout mismatch");
    }
}

// ---------------------------------------------------------------------------
// 1. Read-only Copy capture -> &T, original stays usable
// ---------------------------------------------------------------------------

#[test]
fn shared_copy_capture_leaves_original_usable() {
    ok("fn main() { let n = 40i64\n let f = |x: i64| x + n\n let _ = (f, n) }");
    // Same expectation as phase10's captured_copy_local test: correctness by
    // reference must not change the observable result, so that test needs no
    // edit.
    expect_stdout(
        "shared_capture",
        "fn main() -> i32 {\n    let n = 40i64\n    let f = |x: i64| x + n\n    println_i64(f(2))\n    println_i64(n)\n    return 0\n}\n",
        "42\n40\n",
    );
}

// ---------------------------------------------------------------------------
// 2. Mutated Copy capture -> &mut T, mutation reaches the original
// ---------------------------------------------------------------------------

#[test]
fn mutable_copy_capture_mutates_original() {
    ok("fn main() { let mut n = 1i64\n let mut f = |x: i64| { n += x }\n let _ = (f, n) }");
    expect_stdout(
        "mutable_capture",
        "fn main() -> i32 {\n    let mut n = 1i64\n    let mut f = |x: i64| { n += x }\n    f(2)\n    println_i64(n)\n    return 0\n}\n",
        "3\n",
    );
}

// ---------------------------------------------------------------------------
// 3. Non-Copy capture still works by value
// ---------------------------------------------------------------------------

#[test]
fn non_copy_capture_by_value_still_works() {
    ok("struct Box { v: i64 }\nfn main() { let b = Box { v: 40 }\n let f = |x: i64| x + b.v\n let _ = f }");
    expect_stdout(
        "non_copy_capture",
        "struct Box { v: i64 }\nfn main() -> i32 {\n    let b = Box { v: 40 }\n    let f = |x: i64| x + b.v\n    println_i64(f(2))\n    return 0\n}\n",
        "42\n",
    );
}

// ---------------------------------------------------------------------------
// 4. Mutating a non-Copy capture is a documented limitation (E1020)
// ---------------------------------------------------------------------------

#[test]
fn mutating_non_copy_capture_is_e1020() {
    err(
        "struct Box { v: i64 }\nfn main() {\n    let mut b = Box { v: 1 }\n    let f = |x: i64| { b = Box { v: x }\n        x }\n    let _ = f\n}\n",
        "E1020",
    );
}
