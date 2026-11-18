//! Phase 5: the pure-UMAY immediate-mode core in `examples/v2/gui_core.um`
//! (ADR 0020). `gui_core.um` is spliced at crate root above a generated SDL
//! binding, so the first group feeds the *real* file text into the phase2
//! in-process harness and the `umay check` CLI. The second group builds and
//! runs whole programs, asserting exact scalar stdout.

use std::path::PathBuf;
use std::process::Command;

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::target::Target;

const GUI_CORE_UM: &str = include_str!("../../examples/v2/gui_core.um");

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase20_{name}_{}", std::process::id()));
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

/// lex -> parse -> prelude -> `sema::analyze`, mirroring phase2_sema.rs. The
/// prelude is injected because `gui_core.um` uses `Vec<T>`, `sizeof` and
/// `write_byte`, which every spliced program gets for free.
fn analyze(src: &str) -> Diagnostics {
    let mut map = SourceMap::new();
    let file = map.add("<gui_core>", src);
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

/// The real `gui_core.um` text followed by the program that uses it.
fn spliced(body: &str) -> String {
    format!("{GUI_CORE_UM}\n{body}")
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
// (1) The file type-checks when spliced with a trivial main.
// ---------------------------------------------------------------------------

#[test]
fn gui_core_spliced_typechecks_in_process() {
    ok(&spliced("fn main() {}"));
}

#[test]
fn cli_check_accepts_spliced_gui_core() {
    assert!(cli_check("check", &spliced("fn main() {}")));
}

// ---------------------------------------------------------------------------
// (2) add + toggle + delete -> count is 2.
// ---------------------------------------------------------------------------

const COUNT_SRC: &str = r#"
fn main() -> i32 {
    let mut items: Vec<TodoItem> = Vec::new()
    todo_add(&mut items, 10i64)
    todo_add(&mut items, 20i64)
    todo_add(&mut items, 30i64)
    todo_toggle(&mut items, 20i64)
    todo_delete(&mut items, 30i64)
    println_i64(todo_count(&items))
    return 0
}
"#;

#[test]
fn add_toggle_delete_count_is_two() {
    let Some(stdout) = run("count", &spliced(COUNT_SRC)) else { return };
    assert_eq!(stdout, "2");
}

// ---------------------------------------------------------------------------
// (3) fill_rect clipping and todo_draw colours, read back with read_byte.
// ---------------------------------------------------------------------------

const PIXELS_SRC: &str = r#"
fn gui_probe(buf: *const u8, width: i64, x: i64, y: i64) -> u8 {
    let at = unsafe { (buf as usize + (y * width + x) as usize) as *const u8 }
    return read_byte(at)
}

fn main() -> i32 {
    let mut items: Vec<TodoItem> = Vec::new()
    todo_add(&mut items, 1i64)
    todo_add(&mut items, 2i64)
    todo_add(&mut items, 3i64)
    todo_toggle(&mut items, 2i64)

    let width = 4i64
    let buf = alloc((width * 4) as usize)
    let mut k = 0i64
    while k < width * 4 {
        let at = unsafe { (buf as usize + k as usize) as *mut u8 }
        write_byte(at, 0u8)
        k = k + 1
    }
    // x=-2, w=8 clips to [0,4); y=-1, h=4 clips to rows [0,3).
    gui_fill_rect(buf, width, -2i64, -1i64, 8i64, 4i64, 9u8)
    println_i64(gui_probe(buf, width, 0, 0) as i64)
    println_i64(gui_probe(buf, width, 3, 2) as i64)
    println_i64(gui_probe(buf, width, 0, 3) as i64)
    // Three items, but height=2: only rows 0 and 1 are drawn. Row 0 is the
    // undone colour 1, row 1 the toggled/done colour 2, row 2 keeps the fill.
    todo_draw(&items, buf, width, 2i64)
    println_i64(gui_probe(buf, width, 0, 0) as i64)
    println_i64(gui_probe(buf, width, 0, 1) as i64)
    println_i64(gui_probe(buf, width, 3, 2) as i64)
    println_i64(gui_probe(buf, width, 0, 3) as i64)
    dealloc(buf)
    return 0
}
"#;

#[test]
fn fill_rect_and_draw_produce_deterministic_pixels() {
    let Some(stdout) = run("pixels", &spliced(PIXELS_SRC)) else { return };
    assert_eq!(stdout, "9\n9\n0\n1\n2\n9\n0");
}

// ---------------------------------------------------------------------------
// (4) a missing id is a no-op for toggle and delete.
// ---------------------------------------------------------------------------

const MISSING_SRC: &str = r#"
fn main() -> i32 {
    let mut items: Vec<TodoItem> = Vec::new()
    todo_add(&mut items, 1i64)
    todo_add(&mut items, 2i64)
    println_i64(todo_count(&items))
    todo_toggle(&mut items, 99i64)
    todo_delete(&mut items, 99i64)
    println_i64(todo_count(&items))
    return 0
}
"#;

#[test]
fn missing_id_toggle_delete_is_noop() {
    let Some(stdout) = run("missing", &spliced(MISSING_SRC)) else { return };
    assert_eq!(stdout, "2\n2");
}
