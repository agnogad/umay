//! Phase 18: FFI ABI conformance coverage (v2 Phase 3).
//!
//! Real C callees are compiled with `cc`, linked with the objects emitted by
//! `umay build --emit-object-only`, and run. Only scalar `println_i64` output is
//! asserted, so no test depends on undefined C ABI behaviour.
//!
//! Coverage / target map:
//!   - `sysv_struct_by_value_*` ............ x86_64-unknown-linux-gnu (SysV AMD64)
//!   - `sysv_c_callback_*` ................ x86_64-unknown-linux-gnu (SysV AMD64)
//!   - `sysv_captureless_closure_*` ....... x86_64-unknown-linux-gnu (SysV AMD64)
//!
//! Windows ARM64 aggregate conformance (the v2 exit-criterion-3 gate) runs in
//! `.github/workflows/ci.yml` on a native `windows-11-arm` runner; it cannot run
//! on this Linux host and is deliberately not duplicated here. macOS ABIs are
//! likewise CI-only (see the `macos` jobs in the workflow).
//!
//! Harness follows `phase14_dyn_dispatch.rs` / `phase16_indirect_abi.rs`.

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

/// Probe once per helper invocation; cheap and keeps the "skip" path graceful.
fn cc_and_as_available() -> bool {
    let ok = |name: &str| {
        Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

/// These tests assert the SysV AMD64 ABI and link with the host `cc -no-pie`.
/// They are meaningful only on a Linux x86_64 host with `as` and `cc`.
fn host_supported() -> bool {
    cfg!(all(target_os = "linux", target_arch = "x86_64")) && cc_and_as_available()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase18_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Build a UMAY program to objects, compile a C helper, link them, run, and
/// return raw stdout. Returns `None` when the host cannot run the test.
fn run_linked(name: &str, umay_src: &str, c_src: &str) -> Option<String> {
    if !host_supported() {
        eprintln!("skipping {name}: needs a Linux x86_64 host with `as` and `cc`");
        return None;
    }
    let dir = scratch(name);
    std::fs::write(dir.join("main.um"), umay_src).expect("write UMAY source");
    std::fs::write(dir.join("helper.c"), c_src).expect("write C helper");

    let built = Command::new(compiler())
        .arg("build")
        .arg("--emit-object-only")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        built.status.success(),
        "umay build failed for {name}:\n{}\n{}",
        String::from_utf8_lossy(&built.stderr),
        String::from_utf8_lossy(&built.stdout)
    );

    let compiled = Command::new("cc")
        .args(["-c", "-O2", "helper.c", "-o", "helper.o"])
        .current_dir(&dir)
        .output()
        .expect("compile C helper");
    assert!(
        compiled.status.success(),
        "cc failed for {name}:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let linked = Command::new("cc")
        .args([
            "-o",
            "prog",
            "dist/main.o",
            "dist/umay_runtime.o",
            "helper.o",
            "-no-pie",
        ])
        .current_dir(&dir)
        .output()
        .expect("link UMAY objects with the C helper");
    assert!(
        linked.status.success(),
        "link failed for {name}:\n{}",
        String::from_utf8_lossy(&linked.stderr)
    );

    let ran = Command::new(dir.join("prog")).current_dir(&dir).output().expect("run linked program");
    assert!(
        ran.status.success(),
        "{name} exited with {:?}:\n{}",
        ran.status,
        String::from_utf8_lossy(&ran.stderr)
    );
    Some(String::from_utf8_lossy(&ran.stdout).into_owned())
}

#[track_caller]
fn expect_stdout(name: &str, umay_src: &str, c_src: &str, expected: &str) {
    if let Some(out) = run_linked(name, umay_src, c_src) {
        assert_eq!(out, expected, "{name} stdout mismatch");
    }
}

// ---------------------------------------------------------------------------
// Target: x86_64-unknown-linux-gnu (SysV AMD64).
// Two eightbytes (16 bytes) return in rax:rdx and are passed in registers;
// four eightbytes (32 bytes) use the hidden return pointer and the stack.
// ---------------------------------------------------------------------------

const STRUCT_UMAP: &str = "
struct Small { a: i64, b: i64 }
struct Big { a: i64, b: i64, c: i64, d: i64 }

extern fn \"abi_sum_small\" abi_sum_small(s: Small) -> i64
extern fn \"abi_make_small\" abi_make_small(v: i64) -> Small
extern fn \"abi_sum_big\" abi_sum_big(b: Big) -> i64
extern fn \"abi_make_big\" abi_make_big(v: i64) -> Big

fn main() -> i32 {
    let s = abi_make_small(40i64)
    println_i64(s.a)
    println_i64(s.b)
    println_i64(abi_sum_small(Small { a: 1i64, b: 2i64 }))
    let b = abi_make_big(10i64)
    println_i64(b.a + b.b + b.c + b.d)
    println_i64(abi_sum_big(Big { a: 1i64, b: 2i64, c: 3i64, d: 4i64 }))
    return 0
}
";

const STRUCT_HELPER: &str = "
struct Small { long long a; long long b; };
struct Big { long long a; long long b; long long c; long long d; };
struct Small abi_make_small(long long v) { struct Small s = { v, v + 2 }; return s; }
long long abi_sum_small(struct Small s) { return s.a + s.b; }
struct Big abi_make_big(long long v) { struct Big b = { v, v+1, v+2, v+3 }; return b; }
long long abi_sum_big(struct Big b) { return b.a + b.b + b.c + b.d; }
";

#[test]
fn sysv_struct_by_value_args_and_returns() {
    // Small: make_small(40) -> {40,42}; sum_small({1,2}) -> 3.
    // Big: make_big(10) -> {10,11,12,13} sum 46; sum_big({1,2,3,4}) -> 10.
    expect_stdout("struct_by_value", STRUCT_UMAP, STRUCT_HELPER, "40\n42\n3\n46\n10\n");
}

// ---------------------------------------------------------------------------
// Target: x86_64-unknown-linux-gnu (SysV AMD64).
// A C function receives a UMAY `fn(i64)->i64` pointer and calls it; the callback
// is an indirect call through a plain C function pointer (ADR 0017 §6).
// ---------------------------------------------------------------------------

const CALLBACK_HELPER: &str =
    "long long abi_call_twice(long long (*f)(long long), long long v) { return f(v) + f(v); }\n";

#[test]
fn sysv_c_callback_takes_function_pointer() {
    // A named UMAY function passed as the C callback: triple(7)*2 == 42.
    let umap = "
extern fn \"abi_call_twice\" abi_call_twice(f: fn(i64) -> i64, v: i64) -> i64
fn triple(x: i64) -> i64 { return x * 3i64 }
fn main() -> i32 {
    println_i64(abi_call_twice(triple, 7i64))
    return 0
}
";
    expect_stdout("c_callback_named_fn", umap, CALLBACK_HELPER, "42\n");
}

// ---------------------------------------------------------------------------
// Target: x86_64-unknown-linux-gnu (SysV AMD64).
// A capture-less closure coerces to a plain `fn` pointer, carries no environment
// word, and is callable from C exactly as a function pointer (ADR 0017 §6).
// ---------------------------------------------------------------------------

#[test]
fn sysv_captureless_closure_coerces_to_c_callback() {
    let umap = "
extern fn \"abi_call_twice\" abi_call_twice(f: fn(i64) -> i64, v: i64) -> i64
fn main() -> i32 {
    let f: fn(i64) -> i64 = |x: i64| x + 1i64
    println_i64(abi_call_twice(f, 20i64))
    println_i64(abi_call_twice(|x: i64| x * 2i64, 21i64))
    return 0
}
";
    // (20+1)*2 == 42; (21*2)*2 == 84.
    expect_stdout("captureless_closure_callback", umap, CALLBACK_HELPER, "42\n84\n");
}
