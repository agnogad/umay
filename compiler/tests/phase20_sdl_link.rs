//! Phase 20: native SDL3 linking through `umay build`.
//!
//! `Target::system_libraries` scans the emitted assembly for external callee
//! names and returns the linker flags for the libraries it recognises. SDL3
//! follows the same symbol-prefix rule as the existing SQLite case: any `SDL_`
//! callee (with or without a leading Mach-O/COFF underscore) adds `-lSDL3`,
//! and a program with no such callee gets no extra flag.
//!
//! The build-and-link test skips (not fails) when a toolchain piece,
//! `pkg-config`, or SDL3 itself is unavailable. The flag assertions are
//! host-independent and always run.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use umayc::target::Target;

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn have_tool(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True when the host has SDL3's development files, i.e. a library to link.
fn have_sdl3() -> bool {
    have_tool("pkg-config")
        && Command::new("pkg-config")
            .args(["--exists", "sdl3"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

fn scratch(tag: &str) -> PathBuf {
    let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "umay_phase20_sdl_{tag}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A tiny program that declares one SDL3 entry point and calls it. `i32` is
/// used for the `Uint32 flags` argument: FFI declarations are trusted, and a
/// 32-bit integer is passed in the same register on every supported ABI.
const SDL_PROGRAM: &str = r#"
#![feature(extern_abi)]

extern "C" {
    fn SDL_Init(flags: i32) -> i32;
}

fn main() -> i32 {
    return unsafe { SDL_Init(0) }
}
"#;

/// (a) A single `SDL_` callee yields exactly one `-lSDL3`, with and without the
/// leading underscore emitted by the Mach-O/COFF symbol prefix.
#[test]
fn sdl_callee_adds_lsdl3_once() {
    assert_eq!(
        Target::system_libraries("    call SDL_Init\n"),
        vec!["-lSDL3"]
    );
    assert_eq!(
        Target::system_libraries("    call _SDL_Init\n"),
        vec!["-lSDL3"]
    );
    // Several SDL calls still collapse to one flag, and SQLite is independent.
    assert_eq!(
        Target::system_libraries("    call SDL_Init\n    call SDL_Quit\n"),
        vec!["-lSDL3"]
    );
    assert_eq!(
        Target::system_libraries("    call SDL_Init\n    call sqlite3_open\n"),
        vec!["-lsqlite3", "-lSDL3"]
    );
}

/// (b) No SDL callee means no `-lSDL3`; SQLite behaviour is unchanged.
#[test]
fn no_sdl_callee_adds_no_lsdl3() {
    assert!(Target::system_libraries("    call umay_println\n").is_empty());
    assert_eq!(
        Target::system_libraries("    call sqlite3_open_v2\n"),
        vec!["-lsqlite3"]
    );
    assert_eq!(
        Target::system_libraries("    call _sqlite3_step\n"),
        vec!["-lsqlite3"]
    );
}

/// (a) The real thing: `umay build` must compile the FFI call and link it with
/// `-lSDL3` without a manual `cc`. Skips when SDL3 or the toolchain is absent.
#[test]
fn a_program_calling_sdl_init_builds_and_links() {
    if !have_tool("as") || !have_tool("cc") || !have_sdl3() {
        eprintln!("skip a_program_calling_sdl_init_builds_and_links: toolchain or SDL3 absent");
        return;
    }

    let dir = scratch("link");
    let file = dir.join("main.um");
    std::fs::write(&file, SDL_PROGRAM).expect("write source");

    // Prove the FFI call survives to assembly, so the link genuinely needs
    // -lSDL3 rather than trivially succeeding with the call optimised away.
    let asm = Command::new(compiler())
        .arg("asm")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay asm");
    assert!(asm.status.success(), "umay asm failed");
    assert!(
        String::from_utf8_lossy(&asm.stdout).contains("call SDL_Init"),
        "emitted assembly has no SDL_Init call"
    );

    // A build failure here is a compile or link failure: without -lSDL3 the
    // linker reports an undefined reference to SDL_Init.
    let built = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        built.status.success(),
        "umay build failed (compile or link):\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );

    let exe = dir.join("dist/main");
    assert!(exe.exists(), "umay build produced no executable");

    let _ = std::fs::remove_dir_all(&dir);
}
