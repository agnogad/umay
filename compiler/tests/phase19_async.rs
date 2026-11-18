//! ADR 0019: first async lowering slice.
//!
//! The outward shape is checked in two ways: in-process sema output proves the
//! state struct and the generated functions are really emitted into HIR, and a
//! build+run proves the generated code prints the right value.
//!
//! Ceilings (see `compiler/src/sema/async_lower.rs`): parameters only, no
//! Pending/resumption, no waker wake, no spawn/select/timeout, no cancellation,
//! no Send, no Pin.

use std::path::PathBuf;
use std::process::Command;

use umayc::hir::Hir;
use umayc::source::SourceMap;
use umayc::target::Target;
use umayc::types::{TyCtxt, TyKind};

const ONE: &str = "
async fn one() -> i64 { return 42 }
fn main() -> i32 {
    let v = block_on(one())
    println_i64(v)
    return 0
}
";

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase19_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Parse + prelude + sema, mirroring the in-process assertion style of
/// `phase15_dyn_drop.rs`.
fn analyze(source: &str) -> (Hir, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add("<phase19>", source);
    let (tokens, lex_errors) = umayc::lexer::tokenize(source, file);
    assert!(lex_errors.is_empty(), "{lex_errors:?}");
    let parsed = umayc::parser::parse(tokens, Vec::new(), file);
    let (program, prelude_diags) =
        umayc::prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
    assert!(!prelude_diags.has_errors(), "{prelude_diags:?}");
    let out = umayc::sema::analyze(&program, &map);
    assert!(
        !out.diagnostics.has_errors(),
        "{}",
        umayc::render_all(&out.diagnostics, &map, false)
    );
    (out.hir, out.types)
}

fn run(source: &str, label: &str) -> String {
    if cfg!(windows) || !toolchain_available() {
        eprintln!("skipping runtime check: no `as` or `cc`");
        return String::new();
    }
    let dir = scratch(label);
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write source");
    let build = Command::new(compiler()).arg("build").arg(&file).current_dir(&dir).output().expect("build");
    assert!(
        build.status.success(),
        "build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let out = Command::new(dir.join("dist/main")).output().expect("run");
    assert!(out.status.success(), "run failed: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    stdout
}

fn toolchain_available() -> bool {
    let ok = |name: &str| {
        Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    };
    ok("as") && ok("cc")
}

#[test]
fn async_fn_emits_state_struct_and_poll_fn() {
    let (hir, types) = analyze(ONE);
    let state = hir
        .structs
        .iter()
        .find(|s| s.name.starts_with("__umay_async_one_"))
        .expect("a generated async state struct");
    assert!(!state.is_copy, "the state must be non-Copy");
    assert!(state.fields.is_empty(), "one() has no parameters");
    assert!(hir.fns.iter().any(|f| f.name == "__umay_thunk_one"), "a thunk");
    assert!(hir.fns.iter().any(|f| f.name == "__umay_poll_one"), "a poll fn");
    assert!(hir.fns.iter().any(|f| f.name == "__umay_block_on_one"), "a block_on fn");
    // The async function itself now returns the state type.
    let one = hir.fns.iter().find(|f| f.name == "one").expect("one()");
    assert!(one.is_async);
    assert!(
        matches!(types.kind(one.ret), TyKind::Adt { def, .. } if *def == state.def_id),
        "return rewritten to the generated state ADT"
    );
}

#[test]
fn block_on_runs_the_original_body() {
    assert_eq!(run(ONE, "one"), "42");
}

#[test]
fn await_inside_an_async_fn_works() {
    let src = "
async fn inner() -> i64 { return 7 }
async fn outer() -> i64 { let v = inner().await return v }
fn main() -> i32 {
    println_i64(block_on(outer()))
    return 0
}
";
    assert_eq!(run(src, "await"), "7");
}

#[test]
fn parameters_pass_through_the_state_and_thunk() {
    let src = "
async fn add(a: i64, b: i64) -> i64 { return a + b }
fn main() -> i32 {
    println_i64(block_on(add(2, 3)))
    return 0
}
";
    assert_eq!(run(src, "params"), "5");
}
