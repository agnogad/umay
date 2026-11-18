//! The `umay expand` command's contract.
//!
//! Expansion is a developer tool whose whole value is that its output is exact:
//! tests and agents diff it, so it must be deterministic, it must actually strip
//! the macro layer, and a broken macro must fail loudly rather than emit a half
//! expansion. These tests hold the command against the real binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

fn expand_in(dir: &Path, file: &str) -> Run {
    let output = Command::new(compiler())
        .current_dir(dir)
        .args(["expand", file])
        .output()
        .expect("run umay expand");
    Run {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A private scratch directory per test: tests run in parallel under one pid, so
/// a shared path would let them read each other's source.
fn scratch(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_expand_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("main.um"), src).expect("write source");
    dir
}

const MACRO_SRC: &str = "macro_rules! answer { () => { 42 } }\nfn main() -> i32 { return answer!() }\n";
const PLAIN_SRC: &str = "fn main() -> i32 {\n    return 0\n}\n";
const MALFORMED_SRC: &str = "macro_rules! broken { () => { 42 \n";

#[test]
fn expand_replaces_the_macro_with_its_body() {
    let dir = scratch("macro", MACRO_SRC);
    let run = expand_in(&dir, "main.um");
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains("42"),
        "expanded source should contain the macro body: {}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("macro_rules"),
        "the macro definition should not remain in expanded source: {}",
        run.stdout
    );
}

#[test]
fn a_file_without_macros_round_trips_unchanged() {
    let dir = scratch("plain", PLAIN_SRC);
    let run = expand_in(&dir, "main.um");
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout, PLAIN_SRC);
}

#[test]
fn a_malformed_macro_fails_with_a_diagnostic() {
    let dir = scratch("malformed", MALFORMED_SRC);
    let run = expand_in(&dir, "main.um");
    assert_ne!(run.status, 0, "a malformed macro must not exit successfully");
    let combined = format!("{}{}", run.stdout, run.stderr).to_lowercase();
    assert!(
        combined.contains("error"),
        "a diagnostic was expected, got:\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
}

#[test]
fn two_expansion_runs_are_byte_identical() {
    // Reproducibility is the phase-8 exit criterion: no paths, no timestamps,
    // no hash-map ordering may leak into the expanded text.
    let dir = scratch("repro", MACRO_SRC);
    let a = expand_in(&dir, "main.um");
    let b = expand_in(&dir, "main.um");
    assert_eq!(a.status, 0, "stderr: {}", a.stderr);
    assert_eq!(b.status, 0, "stderr: {}", b.stderr);
    assert_eq!(
        a.stdout.as_bytes(),
        b.stdout.as_bytes(),
        "two runs on the same file must produce byte-identical stdout"
    );
}
