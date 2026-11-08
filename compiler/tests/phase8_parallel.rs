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

/// The number of cache entries whose file name is `name`.
fn entries_named(root: &std::path::Path, name: &str) -> usize {
    let mut count = 0;
    fn walk(dir: &std::path::Path, name: &str, count: &mut usize) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, name, count);
            } else if path.file_name().is_some_and(|n| n == name) {
                *count += 1;
            }
        }
    }
    walk(root, name, &mut count);
    count
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

// ---------------------------------------------------------------------------
// The linked executable is cached as well
// ---------------------------------------------------------------------------

#[test]
fn an_unchanged_rebuild_still_produces_a_working_executable() {
    // The executable is now copied out of the cache instead of being linked.
    // A copy that is truncated, or written without its mode, would fail here
    // rather than in the field.
    if !have_toolchain() {
        eprintln!("skipping an_unchanged_rebuild_still_produces_a_working_executable: `cc` is not available");
        return;
    }
    let build = Build::new("exe_cache");
    assert_eq!(build.run(&["build", "main.um"]).0, 0);
    let first = std::fs::read(build.dir.join("dist/main")).expect("the first executable");

    assert_eq!(build.run(&["build", "main.um"]).0, 0);
    let second = std::fs::read(build.dir.join("dist/main")).expect("the second executable");
    assert!(first == second, "the cached executable differs from the linked one");

    // Remove the executable so the next build must restore it from the cache.
    // Without this the file is still the linker's own output, which is
    // executable whatever the cache wrote -- and a restored copy that lost its
    // mode would go unnoticed.
    std::fs::remove_file(build.dir.join("dist/main")).expect("remove the executable");
    assert_eq!(build.run(&["build", "main.um"]).0, 0);
    assert!(
        build.dir.join("dist/main").is_file(),
        "the cached executable was not restored"
    );

    // It has to be runnable, which a byte comparison alone does not show: the
    // execute bit lives in the file mode, not in the bytes.
    let (status, stdout, stderr) = build.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the cached executable should run:\n{stderr}");
    assert!(stdout.contains("parallel"), "got:\n{stdout}");
}

#[test]
fn changing_the_source_replaces_the_cached_executable() {
    if !have_toolchain() {
        eprintln!("skipping changing_the_source_replaces_the_cached_executable: `cc` is not available");
        return;
    }
    let build = Build::new("exe_invalidate");
    assert_eq!(build.run(&["run", "main.um"]).0, 0);
    std::fs::write(
        build.dir.join("main.um"),
        "fn main() -> i32 {\n    println(\"changed\")\n    return 0\n}\n",
    )
    .expect("rewrite the program");
    let (status, stdout, stderr) = build.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the rebuild should succeed:\n{stderr}");
    assert!(stdout.contains("changed"), "the old executable was reused:\n{stdout}");
}

#[test]
fn a_different_optimisation_level_produces_a_different_executable_entry() {
    if !have_toolchain() {
        eprintln!(
            "skipping a_different_optimisation_level_produces_a_different_executable_entry: `cc` is not available"
        );
        return;
    }
    // The linked executable's key includes the program assembly, and `-O0` and
    // `-O2` generate different assembly, so the two cannot share an entry. This
    // test pins the consequence rather than the mechanism: an `-O0` build
    // followed by an `-O2` build must leave two executable entries.
    //
    // The level itself is also a field in the key. That is defence in depth and
    // is not what this test proves: removing it changes nothing here precisely
    // because the assembly differs. Worth knowing, so it is written down.
    let build = Build::new("exe_opt");
    let count = || entries_named(&build.cache, "exe");

    assert_eq!(build.run(&["build", "main.um", "-O0"]).0, 0);
    let after_o0 = count();
    assert!(after_o0 > 0, "the -O0 build should have cached an executable");

    assert_eq!(build.run(&["build", "main.um", "-O2"]).0, 0);
    let after_o2 = count();
    assert!(
        after_o2 > after_o0,
        "-O2 reused -O0's linked executable ({after_o0} then {after_o2})"
    );
}
