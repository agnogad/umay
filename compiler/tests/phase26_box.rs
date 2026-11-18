//! Phase 26: the generic owned heap box in `stdlib/box.um`.
//!
//! The first group feeds the *real* `box.um` text into the phase2 in-process
//! harness and the `umay check` CLI, so the declarations under test are the ones
//! shipped in the stdlib, not a copy. Box uses `alloc`/`dealloc`/`sizeof` from
//! the prelude, so the in-process harness injects it just like phase20_gui_core
//! does. A later group builds and runs whole programs, asserting exact stdout.

use std::path::PathBuf;
use std::process::Command;

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::target::Target;

const BOX_UM: &str = include_str!("../../stdlib/box.um");

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase26_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
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

/// lex -> parse -> prelude -> `sema::analyze`, mirroring phase2_sema.rs and
/// phase20_gui_core.rs. The prelude is injected because `box.um` uses `alloc`,
/// `dealloc` and `sizeof`, which every spliced program gets for free.
fn analyze(src: &str) -> Diagnostics {
    let mut map = SourceMap::new();
    let file = map.add("<box>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, file);
    assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}\n---\n{src}");
    let parsed = parser::parse(tokens, Vec::new(), file);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}\n---\n{src}");
    let (program, prelude_diags) =
        umayc::prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
    assert!(!prelude_diags.has_errors(), "prelude errors: {prelude_diags:?}");
    let out = sema::analyze(&program, &map);
    if out.diagnostics.has_errors() {
        eprintln!("{}", umayc::render_all(&out.diagnostics, &map, false));
    }
    out.diagnostics
}

#[track_caller]
fn ok(src: &str) {
    assert!(!analyze(src).has_errors(), "expected no type errors\n---\n{src}");
}

/// The real `box.um` declarations followed by a user program.
fn spliced(body: &str) -> String {
    format!("{BOX_UM}\n{body}")
}

/// `umay check` on the spliced source in a temp directory.
fn cli_check(name: &str, src: &str) -> bool {
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, src).expect("write temp source");
    let out = Command::new(compiler())
        .arg("check")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay check");
    let passed = out.status.success();
    if !passed {
        eprintln!(
            "umay check failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    passed
}

/// Builds and runs the spliced source, returning trimmed stdout. `None` means
/// the host has no assembler/linker, in which case a test returns early.
fn run(name: &str, src: &str) -> Option<String> {
    if cfg!(windows) || !toolchain_available() {
        eprintln!("skipping runtime check: no `as` or `cc`");
        return None;
    }
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, src).expect("write temp source");
    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        build.status.success(),
        "build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = dir.join("dist").join(if cfg!(windows) { "main.exe" } else { "main" });
    let out = Command::new(&exe).current_dir(&dir).output().expect("run built program");
    assert!(out.status.success(), "run failed: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    Some(stdout)
}

// ---------------------------------------------------------------------------
// (a) The file type-checks when spliced with a trivial main.
// ---------------------------------------------------------------------------

#[test]
fn box_spliced_typechecks_in_process() {
    ok(&spliced("fn main() {}"));
}

#[test]
fn cli_check_accepts_spliced_box() {
    assert!(cli_check("check", &spliced("fn main() {}")));
}

// Additional programs used by the runtime group are checked here too, so a
// type error is reported as a sema failure rather than a build failure.
#[test]
fn box_programs_typecheck_in_process() {
    ok(&spliced(GET_SRC));
    ok(&spliced(MUT_SRC));
    ok(&spliced(STRUCT_SRC));
    ok(&spliced(FREE_SRC));
}

// ---------------------------------------------------------------------------
// (b) box_new + box_get prints the owned value.
// ---------------------------------------------------------------------------

// `let` bindings initialised by a generic call need an explicit `Box<T>` type:
// the checker leaves the call's result as `Box<T>` until monomorphisation fills
// `T` in, and settling the binding is what lets the later `box_get`/`box_free`
// calls infer their own `T`. See mono.rs `settle_let_types`.
const GET_SRC: &str = r#"
fn main() -> i32 {
    let mut b: Box<i64> = box_new(40i64)
    println_i64(*box_get(&b))
    box_free(&mut b)
    return 0
}
"#;

#[test]
fn box_new_then_get_prints_value() {
    let Some(stdout) = run("get", &spliced(GET_SRC)) else { return };
    assert_eq!(stdout, "40");
}

// ---------------------------------------------------------------------------
// (c) A mutable borrow through box_get_mut reaches the heap value.
// ---------------------------------------------------------------------------

const MUT_SRC: &str = r#"
fn main() -> i32 {
    let mut b: Box<i64> = box_new(40i64)
    let r = box_get_mut(&mut b)
    *r = 99i64
    println_i64(*box_get(&b))
    box_free(&mut b)
    return 0
}
"#;

#[test]
fn mutation_through_box_get_mut_changes_heap_value() {
    let Some(stdout) = run("mut", &spliced(MUT_SRC)) else { return };
    assert_eq!(stdout, "99");
}

// ---------------------------------------------------------------------------
// (d) A struct payload round-trips through the box.
// ---------------------------------------------------------------------------

const STRUCT_SRC: &str = r#"
struct Point { x: i64, y: i64 }
fn main() -> i32 {
    let mut b: Box<Point> = box_new(Point { x: 3i64, y: 4i64 })
    let p: &Point = box_get(&b)
    println_i64(p.x)
    println_i64(p.y)
    box_free(&mut b)
    return 0
}
"#;

#[test]
fn struct_payload_round_trips() {
    let Some(stdout) = run("struct", &spliced(STRUCT_SRC)) else { return };
    assert_eq!(stdout, "3\n4");
}

// ---------------------------------------------------------------------------
// (e) box_free twice does not crash: the second call sees a nulled pointer.
// ---------------------------------------------------------------------------

const FREE_SRC: &str = r#"
fn main() -> i32 {
    let mut b: Box<i64> = box_new(40i64)
    box_free(&mut b)
    box_free(&mut b)
    println_i64(7i64)
    return 0
}
"#;

#[test]
fn double_box_free_is_a_noop() {
    let Some(stdout) = run("free", &spliced(FREE_SRC)) else { return };
    assert_eq!(stdout, "7");
}
