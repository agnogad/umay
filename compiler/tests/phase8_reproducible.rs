//! Reproducible builds.
//!
//! The claim is that the same source compiled twice produces the same bytes, no
//! matter where it lives or whether the cache was warm. That is worth a test
//! because it is easy to break without noticing: an absolute path written into
//! the assembly, a timestamp in the object, or a cache entry that omits an input
//! would all pass every other test in this suite.
//!
//! Two directories, not two runs in one, because a path that leaks into the
//! output only shows up when the path differs.

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn have_toolchain() -> bool {
    // The host execution harness uses the Unix assembler/linker and executable
    // names. Windows coverage is the native smoke step in CI.
    if cfg!(windows) {
        return false;
    }
    Command::new("cc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

const PROGRAM: &str = "fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> i32 {\n    let mut total = 0i64\n    let mut i = 0i64\n    while i < 10i64 {\n        total = add(total, i)\n        i = i + 1i64\n    }\n    println(\"done\")\n    return 0\n}\n";

/// A directory with the program in it and its own cache.
struct Build {
    dir: PathBuf,
}

impl Build {
    fn new(tag: &str) -> Build {
        let dir = std::env::temp_dir().join(format!("umay_repro_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the directory");
        std::fs::write(dir.join("p.um"), PROGRAM).expect("write the program");
        Build { dir }
    }

    fn run(&self, args: &[&str], cache: &str) -> (i32, String, String) {
        let output = Command::new(compiler())
            .current_dir(&self.dir)
            .env("UMAY_CACHE_DIR", self.dir.join(cache))
            .args(args)
            .output()
            .expect("run umay");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// `umay asm` writes the assembly to stdout, so it is returned, not read.
    fn assembly(&self) -> String {
        let (status, stdout, stderr) = self.run(&["asm", "p.um"], "cache");
        assert_eq!(status, 0, "asm should succeed:\n{stderr}");
        assert!(!stdout.is_empty(), "the assembly should not be empty");
        stdout
    }

    fn read(&self, relative: &str) -> Vec<u8> {
        std::fs::read(self.dir.join(relative))
            .unwrap_or_else(|e| panic!("cannot read {relative} in {}: {e}", self.dir.display()))
    }

}

#[test]
fn assembly_is_the_same_in_two_different_directories() {
    let left = Build::new("asm_a").assembly();
    let right = Build::new("asm_b").assembly();
    assert!(
        left == right,
        "assembly differs between two directories ({} vs {} bytes)",
        left.len(),
        right.len()
    );
}

#[test]
fn a_warm_cache_does_not_change_the_output() {
    // A cache that returns something subtly different from what it stored is the
    // worst kind: correct until the day it is not.
    let build = Build::new("warm");
    let cold = build.assembly();
    let warm = build.assembly();
    assert!(cold == warm, "a cache hit produced different bytes than the miss");
}

#[test]
fn the_object_and_executable_are_reproducible_too() {
    if !have_toolchain() {
        eprintln!("skipping the_object_and_executable_are_reproducible_too: `cc` is not available");
        return;
    }
    let a = Build::new("obj_a");
    let b = Build::new("obj_b");
    assert_eq!(a.run(&["build", "p.um"], "cache").0, 0);
    assert_eq!(b.run(&["build", "p.um"], "cache").0, 0);

    // `read` is used here; the assembly cases above use captured stdout.
    for artifact in ["dist/p.o", "dist/umay_runtime.o", "dist/p"] {
        let left = a.read(artifact);
        let right = b.read(artifact);
        assert!(!left.is_empty(), "{artifact} should not be empty");
        assert!(left == right, "{artifact} differs between two directories");
    }
}

#[test]
fn the_assembly_carries_no_absolute_path() {
    // An absolute path in the output would make the build reproducible only on
    // the machine that produced it.
    let build = Build::new("paths");
    let text = build.assembly();
    let dir = build.dir.to_string_lossy().into_owned();
    assert!(
        !text.contains(&dir),
        "the assembly mentions the build directory `{dir}`"
    );
}
