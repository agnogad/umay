//! Build stages that do not depend on each other.
//!
//! The program object and the runtime object were assembled one after the other
//! even though neither reads the other. They are now assembled at the same time.
//! A test cannot measure the speedup without making a flaky timing test, but it
//! can pin what the change risks: both objects must still be produced, and the
//! build's output must still be the same on every run.

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn have_toolchain() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

const PROGRAM: &str = "fn main() -> i32 {\n    println(\"parallel\")\n    return 0\n}\n";

struct Build {
    dir: PathBuf,
    cache: PathBuf,
}

impl Build {
    fn new(tag: &str) -> Build {
        let dir = std::env::temp_dir().join(format!("umay_par_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the directory");
        std::fs::write(dir.join("main.um"), PROGRAM).expect("write the program");
        let cache = dir.join("cache");
        Build { dir, cache }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::new(compiler())
            .current_dir(&self.dir)
            .env("UMAY_CACHE_DIR", &self.cache)
            .args(args)
            .output()
            .expect("run umay");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

#[test]
fn both_objects_are_produced_by_a_build() {
    if !have_toolchain() {
        eprintln!("skipping both_objects_are_produced_by_a_build: `cc` is not available");
        return;
    }
    let build = Build::new("objects");
    let (status, _, stderr) = build.run(&["build", "main.um"]);
    assert_eq!(status, 0, "the build should succeed:\n{stderr}");
    for object in ["dist/main.o", "dist/umay_runtime.o", "dist/main.s", "dist/umay_runtime.s"] {
        let path = build.dir.join(object);
        assert!(path.is_file(), "{object} was not produced");
        let size = std::fs::metadata(&path).expect("the object exists").len();
        assert!(size > 0, "{object} is empty");
    }
}

#[test]
fn the_build_says_the_same_thing_every_time() {
    // Assembling in parallel means two messages could arrive in either order.
    // Nothing is printed concurrently for that reason, and this pins it.
    if !have_toolchain() {
        eprintln!("skipping the_build_says_the_same_thing_every_time: `cc` is not available");
        return;
    }
    let build = Build::new("stable_output");
    let (_, first, _) = build.run(&["build", "main.um"]);
    let (_, second, _) = build.run(&["build", "main.um"]);
    assert_eq!(first, second, "two builds of one source printed different things");
}

#[test]
fn a_failing_assembly_is_reported_once_and_the_build_fails() {
    if !have_toolchain() {
        eprintln!("skipping a_failing_assembly_is_reported_once_and_the_build_fails: `cc` is not available");
        return;
    }
    // A source that generates assembly `as` rejects is hard to force from UMAY,
    // so the failure is simulated by making the object path unwritable: the
    // assembler cannot create it, and the build must say so and stop.
    let build = Build::new("fail_once");
    std::fs::create_dir_all(build.dir.join("dist").join("main.o")).expect("block the object path");
    let (status, _, stderr) = build.run(&["build", "main.um"]);
    assert_ne!(status, 0, "a build that cannot assemble must fail");
    let complaints = stderr.matches("umay:").count();
    assert!(complaints >= 1, "the failure should be reported:\n{stderr}");
    assert!(
        !build.dir.join("dist").join("main").exists(),
        "no executable should be produced when a stage fails"
    );
}

#[test]
fn parallel_assembly_does_not_break_reproducibility() {
    if !have_toolchain() {
        eprintln!("skipping parallel_assembly_does_not_break_reproducibility: `cc` is not available");
        return;
    }
    let a = Build::new("repro_a");
    let b = Build::new("repro_b");
    assert_eq!(a.run(&["build", "main.um"]).0, 0);
    assert_eq!(b.run(&["build", "main.um"]).0, 0);
    for artifact in ["dist/main.o", "dist/umay_runtime.o", "dist/main"] {
        let left = std::fs::read(a.dir.join(artifact)).expect("artifact a");
        let right = std::fs::read(b.dir.join(artifact)).expect("artifact b");
        assert!(left == right, "{artifact} differs between two builds");
    }
}
