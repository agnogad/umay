//! Phase 6: the platform-independent mobile note core in
//! `examples/v2/mobile_core.um` (ADR 0021, docs/v2/webview-decision.md).
//!
//! The core is spliced at crate root between `stdlib/ipc.um` and the app body,
//! in that order. The first group feeds the real spliced text into the phase2
//! in-process harness and the `umay check` CLI; the second group builds and runs
//! whole programs, asserting exact scalar stdout.
//!
//! `stdlib/ipc.um` is read at run time rather than with `include_str!` on
//! purpose: it is landed by a separate agent, so this test file still compiles
//! before it exists and then fails with a precise message instead of a build
//! error. The frozen IPC contract is never stubbed here -- every test uses the
//! real file, so a missing or divergent `ipc.um` cannot be faked away.

use std::path::PathBuf;
use std::process::Command;

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::prelude;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::target::Target;

const MOBILE_CORE_UM: &str = include_str!("../../examples/v2/mobile_core.um");

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase22_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The frozen IPC stdlib file owned by the parallel Phase 6 agent.
fn ipc_source() -> String {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../stdlib/ipc.um"));
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot load the frozen IPC contract at {}: {e}. stdlib/ipc.um is landed by a \
             separate Phase 6 agent; build this test once that file exists (do not stub it).",
            path.display()
        )
    });
    assert!(
        src.contains("struct IpcReply"),
        "stdlib/ipc.um diverged from the frozen Phase 6 interface: missing \
         `struct IpcReply {{ id: i64, ok: bool, result: i64 }}`. The mobile core returns that \
         type from note_dispatch; do not redefine it here -- fix the dependency."
    );
    src
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

/// lex -> parse -> prelude -> `sema::analyze`, mirroring phase2_sema.rs. The
/// prelude is injected because the core uses `Vec<T>`, `sizeof`, `grow` and
/// `println_i64`, which every spliced program gets for free.
fn analyze(src: &str) -> Diagnostics {
    let mut map = SourceMap::new();
    let file = map.add("<mobile_core>", src);
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
        prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
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

/// The real splice order: stdlib IPC, mobile core, then the app body.
fn spliced(body: &str) -> String {
    format!("{}\n{}\n{}", ipc_source(), MOBILE_CORE_UM, body)
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
// (1) The spliced file type-checks with a trivial main.
// ---------------------------------------------------------------------------

#[test]
fn mobile_core_spliced_typechecks_in_process() {
    ok(&spliced("fn main() {}"));
}

#[test]
fn cli_check_accepts_spliced_mobile_core() {
    assert!(cli_check("check", &spliced("fn main() {}")));
}

// ---------------------------------------------------------------------------
// (2) mobile_selftest -> final count is 1.
// ---------------------------------------------------------------------------

const SELFTEST_SRC: &str = r#"
fn main() -> i32 {
    let mut notes: Vec<Note> = Vec::new()
    println_i64(mobile_selftest(&mut notes))
    return 0
}
"#;

#[test]
fn mobile_selftest_returns_one() {
    let Some(stdout) = run("selftest", &spliced(SELFTEST_SRC)) else { return };
    assert_eq!(stdout, "1");
}

// ---------------------------------------------------------------------------
// (3) dispatch methods 1/2/3/4 mutate state and reply ok with the live count.
// ---------------------------------------------------------------------------

/// Prints `id`, `ok` (as 1/0), `result`, then the live count after the call.
const EMIT_FN: &str = r#"
fn emit(r: IpcReply, notes: &Vec<Note>) {
    println_i64(r.id)
    println_i64(if r.ok { 1i64 } else { 0i64 })
    println_i64(r.result)
    println_i64(note_count(notes))
}
"#;

const DISPATCH_SRC: &str = r#"
fn main() -> i32 {
    let mut m: Vec<Note> = Vec::new()
    emit(note_dispatch(&mut m, IpcRequest { id: 10, method: 1, arg: 10 }), &m)
    emit(note_dispatch(&mut m, IpcRequest { id: 20, method: 1, arg: 20 }), &m)
    emit(note_dispatch(&mut m, IpcRequest { id: 30, method: 2, arg: 10 }), &m)
    emit(note_dispatch(&mut m, IpcRequest { id: 40, method: 3, arg: 20 }), &m)
    emit(note_dispatch(&mut m, IpcRequest { id: 50, method: 4, arg: 0 }), &m)
    m.free()
    return 0
}
"#;

#[test]
fn note_dispatch_methods_mutate_and_reply_ok() {
    let src = format!("{EMIT_FN}\n{DISPATCH_SRC}");
    let Some(stdout) = run("dispatch", &spliced(&src)) else { return };
    // id, ok, result, count for add/add/toggle/delete/count.
    assert_eq!(
        stdout,
        "10\n1\n1\n1\n20\n1\n2\n2\n30\n1\n2\n2\n40\n1\n1\n1\n50\n1\n1\n1"
    );
}

// ---------------------------------------------------------------------------
// (4) unknown method -> ok:false, result:-1, state unchanged.
// ---------------------------------------------------------------------------

const UNKNOWN_SRC: &str = r#"
fn main() -> i32 {
    let mut u: Vec<Note> = Vec::new()
    emit(note_dispatch(&mut u, IpcRequest { id: 5, method: 99, arg: 7 }), &u)
    note_add(&mut u, 1i64)
    emit(note_dispatch(&mut u, IpcRequest { id: 6, method: -3, arg: 2 }), &u)
    u.free()
    return 0
}
"#;

#[test]
fn note_dispatch_unknown_method_is_error_noop() {
    let src = format!("{EMIT_FN}\n{UNKNOWN_SRC}");
    let Some(stdout) = run("unknown", &spliced(&src)) else { return };
    assert_eq!(stdout, "5\n0\n-1\n0\n6\n0\n-1\n1");
}
