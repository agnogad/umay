//! The build cache, checked through the real `umay build`.
//!
//! The unit tests in `build.rs` check the key derivation and the store in
//! isolation. These check the thing that actually matters to a user: that a
//! rebuild of unchanged source recompiles nothing, that a change to the source
//! does recompile, and that neither writes anything into the project.
//!
//! Every test points `UMAY_CACHE_DIR` at a scratch directory. A test that shared
//! the user's real cache would be reading state it did not create, and could
//! pass or fail depending on what was built an hour earlier.

use std::path::{Path, PathBuf};
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

/// A scratch project with its own cache.
struct Project {
    dir: PathBuf,
    cache: PathBuf,
}

impl Project {
    fn new(name: &str, source: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("umay_build_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the project directory");
        std::fs::write(dir.join("main.um"), source).expect("write the source");
        let cache = dir.join("cache");
        Project { dir, cache }
    }

    fn build(&self) -> (i32, String) {
        let output = Command::new(compiler())
            .current_dir(&self.dir)
            .env("UMAY_CACHE_DIR", &self.cache)
            .arg("build")
            .arg("main.um")
            .output()
            .expect("run umay build");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn edit(&self, source: &str) {
        std::fs::write(self.dir.join("main.um"), source).expect("rewrite the source");
    }

    /// Every file under the cache, relative to its root.
    fn cache_entries(&self) -> Vec<String> {
        let mut out = Vec::new();
        walk(&self.cache, &self.cache, &mut out);
        out.sort();
        out
    }

    fn project_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        collect_files(&self.dir, &mut out);
        out.sort();
        out
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_string_lossy().into_owned());
        }
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        // The cache is inside the project directory for the test's convenience,
        // but it is not part of the project: it is the cache under a different
        // path. Skipping it keeps this test about project files.
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "cache") {
                continue;
            }
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

const HELLO: &str = "fn main() -> i32 {\n    return 0\n}\n";

#[test]
fn a_first_build_fills_the_cache() {
    if !have_toolchain() {
        eprintln!("skipping a_first_build_fills_the_cache: `cc` is not available");
        return;
    }
    let project = Project::new("first", HELLO);
    let (status, stderr) = project.build();
    assert_eq!(status, 0, "the program is correct:\n{stderr}");
    let entries = project.cache_entries();
    assert!(
        entries.iter().any(|e| e.ends_with("program.s")),
        "the program's assembly should be cached, got {entries:?}"
    );
    assert!(
        entries.iter().any(|e| e.ends_with("runtime.s")),
        "the runtime's assembly should be cached, got {entries:?}"
    );
}

#[test]
fn an_unchanged_rebuild_reuses_the_cache() {
    if !have_toolchain() {
        eprintln!("skipping an_unchanged_rebuild_reuses_the_cache: `cc` is not available");
        return;
    }
    let project = Project::new("unchanged", HELLO);
    assert_eq!(project.build().0, 0);
    let after_first = project.cache_entries();
    assert_eq!(project.build().0, 0);
    let after_second = project.cache_entries();
    assert_eq!(
        after_first, after_second,
        "an unchanged rebuild should not add cache entries"
    );
}

#[test]
fn the_assembly_written_to_disk_is_the_cached_one() {
    if !have_toolchain() {
        eprintln!("skipping the_assembly_written_to_disk_is_the_cached_one: `cc` is not available");
        return;
    }
    let project = Project::new("same_asm", HELLO);
    assert_eq!(project.build().0, 0);
    let first = std::fs::read_to_string(project.dir.join("dist/main.s")).expect("the assembly");
    assert_eq!(project.build().0, 0);
    let second = std::fs::read_to_string(project.dir.join("dist/main.s")).expect("the assembly");
    assert_eq!(first, second, "a cached build must produce the same assembly");
}

#[test]
fn editing_the_source_misses_the_cache() {
    if !have_toolchain() {
        eprintln!("skipping editing_the_source_misses_the_cache: `cc` is not available");
        return;
    }
    let project = Project::new("edited", HELLO);
    assert_eq!(project.build().0, 0);
    let after_first = project.cache_entries();
    project.edit("fn main() -> i32 {\n    return 7\n}\n");
    assert_eq!(project.build().0, 0);
    let after_edit = project.cache_entries();
    assert!(
        after_edit.len() > after_first.len(),
        "a changed source should add an entry: {after_first:?} -> {after_edit:?}"
    );
}

#[test]
fn the_optimisation_level_is_part_of_the_key() {
    if !have_toolchain() {
        eprintln!("skipping the_optimisation_level_is_part_of_the_key: `cc` is not available");
        return;
    }
    // A program with foldable arithmetic: at `-O0` and `-O2` the optimiser
    // produces different assembly, so a reused entry would be visible.
    let project = Project::new("opt", "fn main() -> i32 {\n    let x = 1 + 2\n    return x\n}\n");
    let build_at = |level: &str| {
        let output = Command::new(compiler())
            .current_dir(&project.dir)
            .env("UMAY_CACHE_DIR", &project.cache)
            .args(["build", level, "main.um"])
            .output()
            .expect("run umay build");
        assert!(output.status.success(), "{level} should build");
    };
    build_at("-O2");
    let after_o2 = project.cache_entries();
    build_at("-O0");
    let after_o0 = project.cache_entries();
    assert!(
        after_o0.len() > after_o2.len(),
        "-O0 must not reuse the -O2 entry: {after_o2:?} -> {after_o0:?}"
    );
}

#[test]
fn a_build_writes_nothing_into_the_project_but_dist() {
    if !have_toolchain() {
        eprintln!("skipping a_build_writes_nothing_into_the_project_but_dist: `cc` is not available");
        return;
    }
    let project = Project::new("no_target", HELLO);
    assert_eq!(project.build().0, 0);
    for path in project.project_files() {
        let relative = path.strip_prefix(&project.dir).expect("inside the project");
        // `main.um` is the test's own input, not something the build wrote.
        if relative == Path::new("main.um") {
            continue;
        }
        assert!(
            relative.starts_with("dist"),
            "the build wrote `{}`, which is outside dist/",
            relative.display()
        );
    }
    assert!(
        !project.dir.join("target").exists(),
        "no `target/` directory may appear in a project"
    );
}

#[test]
fn two_clean_builds_produce_identical_assemblies() {
    if !have_toolchain() {
        eprintln!("skipping two_clean_builds_produce_identical_assemblies: `cc` is not available");
        return;
    }
    // The same source built twice from cold caches must give the same bytes.
    // A timestamp or a host path in the output would break this.
    let first = Project::new("repro_a", HELLO);
    assert_eq!(first.build().0, 0);
    let one = std::fs::read(first.dir.join("dist/main.s")).expect("assembly");

    let second = Project::new("repro_b", HELLO);
    assert_eq!(second.build().0, 0);
    let two = std::fs::read(second.dir.join("dist/main.s")).expect("assembly");

    assert_eq!(one, two, "the same source must produce the same assembly");
}

#[test]
fn an_unreadable_cache_entry_costs_a_rebuild_not_a_failure() {
    if !have_toolchain() {
        eprintln!("skipping an_unreadable_cache_entry_costs_a_rebuild_not_a_failure: `cc` is not available");
        return;
    }
    let project = Project::new("corrupt", HELLO);
    assert_eq!(project.build().0, 0);
    // Truncate every cached artifact: the next build must notice and recompile
    // rather than link half a file.
    for entry in project.cache_entries() {
        let path = project.cache.join(&entry);
        std::fs::write(&path, b"").expect("truncate");
    }
    let (status, stderr) = project.build();
    assert_eq!(status, 0, "a truncated cache should not break the build:\n{stderr}");
    let assembly = std::fs::read_to_string(project.dir.join("dist/main.s")).expect("assembly");
    assert!(!assembly.is_empty(), "the assembly should have been regenerated");
}

#[test]
fn clean_reports_the_entries_it_removed() {
    // The two numbers have to agree. `clean` counted the top-level directory and
    // said "cleared 1 cache entry" while removing two hundred, next to a `doctor`
    // that counted files -- a pair of numbers that disagree teaches the reader to
    // stop trusting both.
    if !have_toolchain() {
        eprintln!("skipping clean_reports_the_entries_it_removed: `cc` is not available");
        return;
    }
    let project = Project::new("clean_count", HELLO);
    assert_eq!(project.build().0, 0);
    let cached = project.cache_entries().len();
    assert!(cached > 0, "the build should have cached something");

    let output = Command::new(compiler())
        .current_dir(&project.dir)
        .env("UMAY_CACHE_DIR", &project.cache)
        .arg("clean")
        .output()
        .expect("run umay clean");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("cleared {cached}")),
        "clean removed {cached} files but said:\n{stdout}"
    );
    assert!(
        project.cache_entries().is_empty(),
        "clean should have emptied the cache"
    );
}
