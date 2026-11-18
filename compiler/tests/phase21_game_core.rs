//! Phase 7: the pure-UMAY game/mathematics core in `examples/v2/game_core.um`.
//! `game_core.um` is spliced at crate root above a generated SDL binding, so
//! the first group feeds the real file text into the phase2 in-process harness
//! and the `umay check` CLI. The second group builds and runs whole programs,
//! asserting exact scalar stdout.

use std::path::PathBuf;
use std::process::Command;

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::prelude;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::target::Target;

const GAME_CORE_UM: &str = include_str!("../../examples/v2/game_core.um");

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase21_{name}_{}", std::process::id()));
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
/// prelude is injected because the core uses `read_byte`/`read_u32`/
/// `write_u32`/`alloc`/`println_i64`, which every spliced program gets free.
fn analyze(src: &str) -> Diagnostics {
    let mut map = SourceMap::new();
    let file = map.add("<game_core>", src);
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

/// The real `game_core.um` text followed by the program that uses it.
fn spliced(body: &str) -> String {
    format!("{GAME_CORE_UM}\n{body}")
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
fn game_core_spliced_typechecks_in_process() {
    ok(&spliced("fn main() {}"));
}

#[test]
fn cli_check_accepts_spliced_game_core() {
    assert!(cli_check("check", &spliced("fn main() {}")));
}

// ---------------------------------------------------------------------------
// (2) make_sprite_pixels -> two alternating opaque ARGB colours.
// ---------------------------------------------------------------------------

const PIXELS_SRC: &str = r#"
fn probe_u32(buf: *const u8, i: i64) -> u32 {
    let at = unsafe { (buf as usize + (i * 4) as usize) as *const u8 }
    return read_u32(at)
}

fn main() -> i32 {
    let buf = alloc((8 * 8 * 4) as usize)
    make_sprite_pixels(unsafe { (buf as usize) as *mut u32 })
    println_i64(probe_u32(buf, 0) as i64)
    println_i64(probe_u32(buf, 1) as i64)
    println_i64(probe_u32(buf, 2) as i64)
    println_i64(probe_u32(buf, 63) as i64)
    dealloc(buf)
    return 0
}
"#;

#[test]
fn make_sprite_pixels_produces_two_colour_pattern() {
    let Some(stdout) = run("pixels", &spliced(PIXELS_SRC)) else { return };
    let c0 = 0xffc85028u32 as i64;
    let c1 = 0xff284fc8u32 as i64;
    assert_eq!(stdout, format!("{c0}\n{c1}\n{c0}\n{c1}"));
}

// ---------------------------------------------------------------------------
// (3) synthetic key state (RIGHT + UP) -> deterministic move + clamp.
// ---------------------------------------------------------------------------

const INPUT_SRC: &str = r#"
fn zero(buf: *mut u8, n: i64) {
    let mut i = 0
    while i < n {
        write_byte(unsafe { (buf as usize + i as usize) as *mut u8 }, 0u8)
        i = i + 1
    }
}

fn main() -> i32 {
    let keys = alloc(128usize)
    zero(keys, 128)
    write_byte(unsafe { (keys as usize + 80) as *mut u8 }, 1u8) // RIGHT
    write_byte(unsafe { (keys as usize + 82) as *mut u8 }, 1u8) // UP

    let pix = alloc((8 * 8 * 4) as usize)
    make_sprite_pixels(unsafe { (pix as usize) as *mut u32 })
    let p = unsafe { (pix as usize) as *const u32 }

    let mut s = sprite_new(0, 8, 8, 8, p)
    sprite_input(&mut s, keys)
    sprite_step(&mut s)
    println_i64(sprite_x(&s))
    println_i64(sprite_y(&s))

    let mut t = sprite_new(56, 0, 8, 8, p)
    sprite_input(&mut t, keys)
    sprite_step(&mut t)
    println_i64(sprite_x(&t))
    println_i64(sprite_y(&t))

    dealloc(pix)
    dealloc(keys)
    return 0
}
"#;

#[test]
fn input_and_step_are_deterministic_and_clamped() {
    let Some(stdout) = run("input", &spliced(INPUT_SRC)) else { return };
    assert_eq!(stdout, "1\n7\n56\n0");
}

// ---------------------------------------------------------------------------
// (4) fixed_step accumulator.
// ---------------------------------------------------------------------------

const FIXED_SRC: &str = r#"
fn main() -> i32 {
    println_i64(fixed_step(0, 10, 16))
    println_i64(fixed_step(10, 10, 16))
    println_i64(fixed_step(5, 3, 0))
    return 0
}
"#;

#[test]
fn fixed_step_wraps_and_guards_nonpositive_step() {
    let Some(stdout) = run("fixed", &spliced(FIXED_SRC)) else { return };
    assert_eq!(stdout, "10\n4\n8");
}

// ---------------------------------------------------------------------------
// (5) sprite_draw clipping keeps sentinel bytes intact.
// ---------------------------------------------------------------------------

const DRAW_SRC: &str = r#"
fn probe_u8(buf: *const u8, i: i64) -> u8 {
    let at = unsafe { (buf as usize + i as usize) as *const u8 }
    return read_byte(at)
}

fn probe_u32(buf: *const u8, i: i64) -> u32 {
    let at = unsafe { (buf as usize + (i * 4) as usize) as *const u8 }
    return read_u32(at)
}

fn fill(buf: *mut u8, n: i64, v: u8) {
    let mut i = 0
    while i < n {
        write_byte(unsafe { (buf as usize + i as usize) as *mut u8 }, v)
        i = i + 1
    }
}

fn main() -> i32 {
    let pix = alloc((8 * 8 * 4) as usize)
    make_sprite_pixels(unsafe { (pix as usize) as *mut u32 })
    let p = unsafe { (pix as usize) as *const u32 }

    // 4 sentinel bytes + 4x4 ARGB framebuffer (64 bytes) + 4 sentinel bytes.
    let raw = alloc(72usize)
    fill(raw, 72, 0x7eu8)
    let fb = unsafe { (raw as usize + 4) as *mut u8 }

    // Origin (-2, -1) clips to the whole 4x4 target.
    let s = sprite_new(-2, -1, 8, 8, p)
    sprite_draw(&s, fb, 4, 4)

    println_i64(probe_u8(raw, 0) as i64)
    println_i64(probe_u8(raw, 3) as i64)
    println_i64(probe_u8(raw, 68) as i64)
    println_i64(probe_u8(raw, 71) as i64)
    println_i64(probe_u32(fb, 0) as i64)
    println_i64(probe_u32(fb, 1) as i64)
    println_i64(probe_u32(fb, 4) as i64)
    println_i64(probe_u32(fb, 15) as i64)

    dealloc(raw)
    dealloc(pix)
    return 0
}
"#;

#[test]
fn sprite_draw_clips_and_preserves_sentinel_bytes() {
    let Some(stdout) = run("draw", &spliced(DRAW_SRC)) else { return };
    let c0 = 0xffc85028u32 as i64;
    let c1 = 0xff284fc8u32 as i64;
    assert_eq!(stdout, format!("126\n126\n126\n126\n{c0}\n{c1}\n{c0}\n{c1}"));
}
