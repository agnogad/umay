//! Phase 10: closure semantics.
//!
//! Two halves, matching the two established harnesses in this suite:
//!   * parse + semantic checks through `umayc` in-process (`analyze`/`ok`/`err`),
//!     copied from `phase2_sema.rs`;
//!   * end-to-end build + run through `umay build` with stdout assertions,
//!     copied from `phase5_codegen.rs` / `phase6_runtime.rs`.
//!
//! These tests are written against the intended v2 closure model. While the
//! AST/parser and sema work lands concurrently, they may not compile or pass;
//! that is expected and is not fixed here.

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;

// ---------------------------------------------------------------------------
// In-process parse + sema helpers (phase2_sema.rs)
// ---------------------------------------------------------------------------

struct Checked {
    diagnostics: Diagnostics,
}

fn analyze(src: &str) -> Checked {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(
        lex_errors.is_empty(),
        "test source failed to lex: {:?}\n---\n{src}",
        lex_errors
    );
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

/// Asserts the program produces no errors.
#[track_caller]
fn ok(src: &str) {
    let codes = error_codes(src);
    assert!(codes.is_empty(), "expected no errors, got {codes:?}\n---\n{src}");
}

/// Asserts the program produces this error code (and no parse errors).
#[track_caller]
fn err(src: &str, code: &str) {
    let codes = error_codes(src);
    assert!(codes.contains(&code), "expected error {code}, got {codes:?}\n---\n{src}");
}

// ---------------------------------------------------------------------------
// Build + run helpers (phase5_codegen.rs / phase6_runtime.rs)
// ---------------------------------------------------------------------------

use std::path::PathBuf;
use std::process::Command;

/// The compiler binary under test (Cargo sets this for integration tests).
fn compiler() -> &'static str {
    env!("CARGO_BIN_EXE_umay")
}

/// True when the assembler and linker the backend shells out to are present.
fn toolchain_available() -> bool {
    // Host execution uses Unix tool/executable names; Windows is covered by the
    // native smoke step. Matches phase5_codegen.rs.
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

/// A per-test scratch directory, cleaned first; the pid keeps parallel tests apart.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase10_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Builds `source`, runs it, and returns its stdout. `None` when no toolchain.
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
    let out = Command::new(&exe)
        .current_dir(&dir)
        .output()
        .expect("run built program");
    assert!(
        out.status.success(),
        "{name} exited with {:?}:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Asserts that a built program prints exactly `expected` on stdout.
#[track_caller]
fn expect_stdout(name: &str, source: &str, expected: &str) {
    if let Some(out) = run_stdout(name, source) {
        assert_eq!(out, expected, "{name} stdout mismatch");
    }
}

// ---------------------------------------------------------------------------
// 1. Parse + check: syntax forms
// ---------------------------------------------------------------------------

#[test]
fn closure_one_annotated_param_checks() {
    ok("fn main() { let f = |x: i64| x + 1\n let _ = f }");
}

#[test]
fn closure_zero_params_checks() {
    ok("fn main() { let f = || 42i64\n let _ = f }");
}

#[test]
fn closure_two_params_checks() {
    ok("fn main() { let f = |x: i64, y: i64| x + y\n let _ = f }");
}

#[test]
fn closure_block_body_checks() {
    ok("fn main() { let f = |x: i64| { x + 1 }\n let _ = f }");
}

#[test]
fn closure_move_prefix_checks() {
    ok("fn main() { let f = move |x: i64| x + 1\n let _ = f }");
}

// ---------------------------------------------------------------------------
// 2. Parameter type inference: explicit annotation vs typed binding
// ---------------------------------------------------------------------------

#[test]
fn closure_unannotated_param_without_context_is_e1019() {
    err("fn main() { let f = |x| x + 1\n let _ = f }", "E1019");
}

#[test]
fn closure_typed_binding_supplies_param_type() {
    ok("fn main() { let f: fn(i64) -> i64 = |x| x + 1\n let _ = f }");
}

// ---------------------------------------------------------------------------
// 6. Nested closures; closure inside a generic function
// ---------------------------------------------------------------------------

#[test]
fn nested_closures_check() {
    // A capture-less outer closure whose block body defines and calls a
    // capture-less inner closure.
    ok("
fn main() {
    let f = |x: i64| {
        let g = |y: i64| y * 2
        g(x)
    }
    let _ = f
}
");
}

#[test]
fn closure_inside_generic_function_check() {
    // Generic bodies are checked once as written (see `a_generic_body_is_accepted`
    // in phase2_sema.rs), so a capture-less closure in such a body type-checks
    // without needing a monomorphisation instance.
    ok("
fn identity<T>(x: T) -> T {
    let f = |v: i64| v + 1
    let _ = f
    return x
}
fn main() -> i32 { return 0 }
");
}

// ---------------------------------------------------------------------------
// 3. Build + run: capture-less closures
// ---------------------------------------------------------------------------

#[test]
fn closure_inc_prints_42() {
    expect_stdout(
        "closure_inc",
        "fn main() -> i32 {\n    let f = |x: i64| x + 1\n    println_i64(f(41))\n    return 0\n}\n",
        "42\n",
    );
}

#[test]
fn closure_zero_params_prints_42() {
    // `42i64` keeps the return type fixed without relying on contextual
    // inference from `println_i64`; the point under test is the `||` form.
    expect_stdout(
        "closure_zero_params",
        "fn main() -> i32 {\n    let f = || 42i64\n    println_i64(f())\n    return 0\n}\n",
        "42\n",
    );
}

#[test]
fn closure_two_param_sum_prints_42() {
    expect_stdout(
        "closure_two_param_sum",
        "fn main() -> i32 {\n    let f = |x: i64, y: i64| x + y\n    println_i64(f(17, 25))\n    return 0\n}\n",
        "42\n",
    );
}

// ---------------------------------------------------------------------------
// 4. Build + run: captured Copy local
// ---------------------------------------------------------------------------

#[test]
fn captured_copy_local_prints_and_original_usable() {
    // `n` is Copy, so creating the closure copies it into the environment and
    // the original binding can still be printed afterwards.
    expect_stdout(
        "closure_capture_copy",
        "fn main() -> i32 {\n    let n = 40i64\n    let f = |x: i64| x + n\n    println_i64(f(2))\n    println_i64(n)\n    return 0\n}\n",
        "42\n40\n",
    );
}

// ---------------------------------------------------------------------------
// 5. Build + run: capture-less closure as a callback / fn-typed binding
// ---------------------------------------------------------------------------

#[test]
fn captureless_closure_as_callback_and_typed_binding() {
    // Stored in a `fn(i64) -> i64` binding (42) and passed to a function that
    // calls it through its pointer (4 * 2 = 8).
    expect_stdout(
        "closure_callback",
        "fn apply(f: fn(i64) -> i64, v: i64) -> i64 {\n    return f(v)\n}\n\
         fn main() -> i32 {\n    let f: fn(i64) -> i64 = |x: i64| x * 2\n    println_i64(f(21))\n    println_i64(apply(|x: i64| x * 2, 4))\n    return 0\n}\n",
        "42\n8\n",
    );
}

// ---------------------------------------------------------------------------
// 7. Regression: `|` bitwise or and `||` logical or stay unambiguous
// ---------------------------------------------------------------------------

#[test]
fn bitwise_or_regression() {
    // 6 | 3 == 7; must not be read as the start of a closure.
    expect_stdout(
        "closure_or_bitwise",
        "fn main() -> i32 {\n    let a = 6i64\n    let b = 3i64\n    println_i64(a | b)\n    return 0\n}\n",
        "7\n",
    );
}

#[test]
fn logical_or_regression() {
    // `a || b` must still parse as logical or, not as a zero-param closure.
    expect_stdout(
        "closure_or_logical",
        "fn main() -> i32 {\n    let a = true\n    let b = false\n    println_i64((a || b) as i64)\n    println_i64((b || b) as i64)\n    return 0\n}\n",
        "1\n0\n",
    );
}
