//! Phase 16: x86-64 indirect-call ABI parity.
//!
//! An indirect call through a `fn` pointer (or a `dyn Trait` vtable slot) used to
//! skip the ABI planning a direct call gets: no sret classification, no Win64
//! shadow space, no aggregate-argument placement. These tests build and run real
//! programs so an indirect call with aggregate arguments and returns behaves
//! exactly like a direct one.
//!
//! Harness copied from `phase10_closures.rs` / `phase14_dyn_dispatch.rs`.

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;

// ---------------------------------------------------------------------------
// In-process parse + sema helpers (phase2_sema.rs / phase10 / phase14)
// ---------------------------------------------------------------------------

struct Checked {
    diagnostics: Diagnostics,
}

fn analyze(src: &str) -> Checked {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}\n---\n{src}");
    let parsed = parser::parse(tokens, Vec::new(), id);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}\n---\n{src}");
    Checked { diagnostics: sema::analyze(&parsed.program, &map).diagnostics }
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

// ---------------------------------------------------------------------------
// Build + run helpers (phase10_closures.rs / phase14_dyn_dispatch.rs)
// ---------------------------------------------------------------------------

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
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
    let dir = std::env::temp_dir().join(format!("umay_phase16_{name}_{}", std::process::id()));
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
// (c) Regression: the existing scalar indirect callback still works.
// ---------------------------------------------------------------------------

#[test]
fn scalar_indirect_callback_regression() {
    // Same program as `captureless_closure_as_callback_and_typed_binding` in
    // phase10_closures.rs. `apply` calls `f` through a fn-pointer parameter.
    expect_stdout(
        "scalar_callback",
        "fn apply(f: fn(i64) -> i64, v: i64) -> i64 {\n    return f(v)\n}\n\
         fn main() -> i32 {\n    let f: fn(i64) -> i64 = |x: i64| x * 2\n    println_i64(f(21))\n    println_i64(apply(|x: i64| x * 2, 4))\n    return 0\n}\n",
        "42\n8\n",
    );
}

// ---------------------------------------------------------------------------
// (a) Indirect call through a fn pointer with a struct argument/return.
// ---------------------------------------------------------------------------

#[test]
fn indirect_fn_ptr_struct_arguments_are_checked() {
    ok("
struct Pair { a: i64, b: i64 }
fn apply_pair(f: fn(Pair) -> Pair, p: Pair) -> Pair { return f(p) }
fn scale(p: Pair) -> Pair { return Pair { a: p.a, b: p.b } }
fn main() -> i32 {
    let p = Pair { a: 1, b: 2 }
    let r = apply_pair(scale, p)
    let _ = r
    return 0
}
");
}

#[test]
fn indirect_fn_ptr_struct_arg_and_register_return() {
    // A 16-byte aggregate returns in rax/rdx (no sret) and is passed in two
    // registers. The old indirect path stored only rax and produced a wrong
    // upper half here. `scale` is called through `f` in `apply_pair`.
    expect_stdout(
        "fn_ptr_struct",
        "struct Pair { a: i64, b: i64 }\n\
         fn apply_pair(f: fn(Pair) -> Pair, p: Pair) -> Pair { return f(p) }\n\
         fn scale(p: Pair) -> Pair { return Pair { a: p.a * 3, b: p.b + 5 } }\n\
         fn main() -> i32 {\n    let p = Pair { a: 7, b: 9 }\n    let r = apply_pair(scale, p)\n    println_i64(r.a)\n    println_i64(r.b)\n    return 0\n}\n",
        "21\n14\n",
    );
}

#[test]
fn indirect_fn_ptr_sret_aggregate_return() {
    // A 24-byte aggregate goes through the hidden return pointer, and its
    // argument is copied into the outgoing stack area. The old indirect path
    // rejected (or mishandled) both.
    expect_stdout(
        "fn_ptr_sret",
        "struct Triple { a: i64, b: i64, c: i64 }\n\
         fn apply(f: fn(Triple) -> Triple, t: Triple) -> Triple { return f(t) }\n\
         fn bump(t: Triple) -> Triple { return Triple { a: t.a + 1, b: t.b + 2, c: t.c + 3 } }\n\
         fn main() -> i32 {\n    let t = Triple { a: 10, b: 20, c: 30 }\n    let r = apply(bump, t)\n    println_i64(r.a)\n    println_i64(r.b)\n    println_i64(r.c)\n    return 0\n}\n",
        "11\n22\n33\n",
    );
}

// ---------------------------------------------------------------------------
// (b) `dyn Trait` method with a struct parameter and aggregate return.
// ---------------------------------------------------------------------------

#[test]
fn dyn_trait_method_with_struct_arg_and_aggregate_return() {
    // `d.mix(q)` lowers to an indirect call through a vtable slot whose shim
    // signature is `(*mut u8, Pair) -> Pair`. Caller and callee must agree on the
    // aggregate argument placement and the 16-byte return pair.
    expect_stdout(
        "dyn_struct",
        "struct Pair { a: i64, b: i64 }\n\
         trait Mix { fn mix(&self, other: Pair) -> Pair }\n\
         impl Mix for Pair {\n    fn mix(&self, other: Pair) -> Pair {\n        return Pair { a: self.a + other.a, b: self.b + other.b }\n    }\n}\n\
         fn main() -> i32 {\n    let p = Pair { a: 1, b: 2 }\n    let q = Pair { a: 10, b: 20 }\n    let d: dyn Mix = &p\n    let r = d.mix(q)\n    println_i64(r.a)\n    println_i64(r.b)\n    return 0\n}\n",
        "11\n22\n",
    );
}
