//! `umay lock`, `umay build` inside a project, and the manifest's payoff.
//!
//! These tests run the real binary in a real project directory. The manifest's
//! whole reason to exist is that `umay build` should work from the project's own
//! description of itself, and that is only observable by running it.

use std::path::{Path, PathBuf};
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

struct Project {
    dir: PathBuf,
    cache: PathBuf,
}

impl Project {
    /// A project created by `umay new`, which is what a user gets.
    fn created(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("umay_proj_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the parent directory");
        let status = Command::new(compiler())
            .current_dir(&dir)
            .args(["new", "app"])
            .status()
            .expect("run umay new");
        assert!(status.success(), "`umay new` should succeed");
        let dir = dir.join("app");
        let cache = dir.join("cache");
        Project { dir, cache }
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

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.dir.join(relative))
            .unwrap_or_else(|e| panic!("cannot read {relative}: {e}"))
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create a parent directory");
        }
        std::fs::write(path, text).expect("write a file");
    }

    /// Adds a local library the project depends on.
    fn add_local_dependency(&self, name: &str) {
        self.write(
            &format!("../{name}/src/lib.um"),
            "fn answer() -> i64 {\n    return 42i64\n}\n",
        );
        self.write(
            &format!("../{name}/umay.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n"),
        );
    }

    fn declare_dependency(&self, name: &str, value: &str) {
        let manifest = self.read("umay.toml");
        let updated = format!("{manifest}\n[dependencies]\n{name} = {value}\n");
        self.write("umay.toml", &updated);
    }
}

#[test]
fn a_new_project_builds_and_runs_with_no_arguments() {
    if !have_toolchain() {
        eprintln!("skipping a_new_project_builds_and_runs_with_no_arguments: `cc` is not available");
        return;
    }
    let project = Project::created("hello");
    let (status, stdout, stderr) = project.run(&["build"]);
    assert_eq!(status, 0, "`umay build` should work from the manifest:\n{stderr}");

    let (status, stdout, _) = project.run(&["run"]);
    assert_eq!(status, 0, "a new project should exit 0, got {status}");
    assert!(stdout.contains("Hello, UMAY"), "the greeting should print, got {stdout}");
}

#[test]
fn a_manifest_that_names_target_is_refused() {
    if !have_toolchain() {
        eprintln!("skipping a_manifest_that_names_target_is_refused: `cc` is not available");
        return;
    }
    let project = Project::created("target");
    let manifest = project.read("umay.toml").replace("output-dir = \"dist\"", "output-dir = \"target\"");
    // The generated manifest may not name output-dir at all; write one that does.
    let manifest = if manifest.contains("target") {
        manifest
    } else {
        project.read("umay.toml").replace("[build]", "[build]\noutput-dir = \"target\"")
    };
    project.write("umay.toml", &manifest);
    let (status, _, stderr) = project.run(&["build"]);
    assert_ne!(status, 0, "a `target/` output directory must be refused");
    assert!(stderr.contains("target"), "the error should say why:\n{stderr}");
}

#[test]
fn build_reports_a_manifest_error_rather_than_a_missing_file() {
    if !have_toolchain() {
        eprintln!("skipping build_reports_a_manifest_error_rather_than_a_missing_file: `cc` is not available");
        return;
    }
    let project = Project::created("broken_manifest");
    project.write("umay.toml", "[package]\nname = \"app\"\n");
    let (status, _, stderr) = project.run(&["build"]);
    assert_ne!(status, 0);
    assert!(
        stderr.contains("version") || stderr.contains("edition"),
        "the manifest's problem should be named:\n{stderr}"
    );
    // The error names the file, so the reader knows where to look.
    assert!(stderr.contains("umay.toml"), "got {stderr}");
}

#[test]
fn no_source_and_no_manifest_is_a_usage_error() {
    let dir = std::env::temp_dir().join(format!("umay_bare_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create");
    let output = Command::new(compiler())
        .current_dir(&dir)
        .env("UMAY_CACHE_DIR", dir.join("cache"))
        .args(["build"])
        .output()
        .expect("run umay");
    assert_eq!(output.status.code(), Some(2), "a usage error has exit code 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("umay.toml"), "the message should say what is missing:\n{stderr}");
}

#[test]
fn a_project_with_no_dependencies_locks_to_an_empty_file() {
    let project = Project::created("lock_empty");
    let (status, stdout, stderr) = project.run(&["lock"]);
    assert_eq!(status, 0, "locking an empty project should succeed:\n{stderr}");
    assert!(stdout.contains("0 packages"), "got {stdout}");
    let lock = project.read("umay.lock");
    assert!(lock.contains("version = 1"), "the lockfile records its format:\n{lock}");
    assert!(!lock.contains("[[package]]"), "no packages were resolved:\n{lock}");
}

#[test]
fn a_path_dependency_is_locked_with_its_content_hash() {
    let project = Project::created("lock_path");
    project.add_local_dependency("helper");
    project.declare_dependency("helper", "{ path = \"../helper\", version = \"0.1.0\" }");
    let (status, stdout, stderr) = project.run(&["lock"]);
    assert_eq!(status, 0, "a path dependency should resolve:\n{stderr}");
    assert!(stdout.contains("1 package"), "got {stdout}");
    let lock = project.read("umay.lock");
    assert!(lock.contains("name = \"helper\""), "got {lock}");
    assert!(lock.contains("source = \"../helper\""), "got {lock}");
    // A 64-character SHA-256 digest.
    let hash_line = lock
        .lines()
        .find(|l| l.starts_with("hash = "))
        .unwrap_or_else(|| panic!("the lockfile should record a hash:\n{lock}"));
    let hash = hash_line.trim_start_matches("hash = ").trim_matches('"');
    assert_eq!(hash.len(), 64, "expected a SHA-256 digest, got {hash}");
}

#[test]
fn locking_twice_produces_the_same_file() {
    // A lockfile that changes when nothing changed makes every diff unreadable.
    let project = Project::created("lock_stable");
    project.add_local_dependency("helper");
    project.declare_dependency("helper", "{ path = \"../helper\", version = \"0.1.0\" }");
    assert_eq!(project.run(&["lock"]).0, 0);
    let first = project.read("umay.lock");
    assert_eq!(project.run(&["lock"]).0, 0);
    let second = project.read("umay.lock");
    assert_eq!(first, second, "re-locking the same project should not change the file");
}

#[test]
fn changing_a_dependency_changes_its_locked_hash() {
    let project = Project::created("lock_change");
    project.add_local_dependency("helper");
    project.declare_dependency("helper", "{ path = \"../helper\", version = \"0.1.0\" }");
    assert_eq!(project.run(&["lock"]).0, 0);
    let first = project.read("umay.lock");

    project.write("../helper/src/lib.um", "fn answer() -> i64 {\n    return 43i64\n}\n");
    assert_eq!(project.run(&["lock"]).0, 0);
    let second = project.read("umay.lock");
    assert_ne!(first, second, "a changed dependency must change the lockfile");
}

#[test]
fn a_dependency_with_no_path_is_reported_rather_than_invented() {
    // There is no registry. A lockfile claiming to have pinned something it never
    // fetched would be worse than refusing.
    let project = Project::created("lock_registry");
    project.declare_dependency("lib", "\"1.0.0\"");
    let (status, _, stderr) = project.run(&["lock"]);
    assert_ne!(status, 0, "an unresolvable dependency should fail");
    assert!(
        stderr.contains("no registry"),
        "the message should say why it cannot be fetched:\n{stderr}"
    );
    assert!(
        !project.dir.join("umay.lock").exists(),
        "nothing should be written when resolution is incomplete"
    );
}

#[test]
fn a_missing_path_dependency_is_reported_by_name() {
    let project = Project::created("lock_missing");
    project.declare_dependency("ghost", "{ path = \"../ghost\", version = \"0.1.0\" }");
    let (status, _, stderr) = project.run(&["lock"]);
    assert_ne!(status, 0);
    assert!(stderr.contains("ghost"), "the message should name the dependency:\n{stderr}");
}

#[test]
fn a_manifest_with_a_typo_names_the_line() {
    let project = Project::created("lock_typo");
    project.write(
        "umay.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\nauthros = [\"a\"]\n",
    );
    let (status, _, stderr) = project.run(&["lock"]);
    assert_ne!(status, 0);
    assert!(stderr.contains("authros"), "the typo should be named:\n{stderr}");
    assert!(stderr.contains("umay.toml:5"), "the line should be named:\n{stderr}");
}

#[test]
fn a_lockfile_from_a_future_format_is_refused() {
    let project = Project::created("lock_future");
    project.write("umay.lock", "version = 99\n");
    // The manifest still has to load, so go through a command that reads both.
    let (status, _, stderr) = project.run(&["lock"]);
    // `lock` rewrites the file rather than reading it, so check the reader instead.
    let _ = (status, stderr);
    let lockfile = umayc::lock::Lockfile::parse("version = 99\n").expect_err("a future format is not readable");
    assert!(
        lockfile.iter().any(|e| e.message.contains("format")),
        "got {lockfile:?}"
    );
}

#[test]
fn a_locked_dependency_that_changed_is_visible_to_verify() {
    let project = Project::created("lock_verify");
    project.add_local_dependency("helper");
    project.declare_dependency("helper", "{ path = \"../helper\", version = \"0.1.0\" }");
    assert_eq!(project.run(&["lock"]).0, 0);

    let lockfile = umayc::lock::read_project(&project.dir).expect("the lockfile should parse");
    assert!(
        umayc::lock::verify(&project.dir, &lockfile).is_empty(),
        "an untouched dependency should verify"
    );

    project.write("../helper/src/lib.um", "fn answer() -> i64 {\n    return 0i64\n}\n");
    let problems = umayc::lock::verify(&project.dir, &lockfile);
    assert_eq!(problems.len(), 1, "a changed dependency should be reported: {problems:?}");
    assert!(problems[0].contains("helper"), "got {problems:?}");
}

#[test]
fn the_lockfile_is_never_written_into_a_project_by_a_build() {
    if !have_toolchain() {
        eprintln!("skipping the_lockfile_is_never_written_into_a_project_by_a_build: `cc` is not available");
        return;
    }
    let project = Project::created("build_no_lock");
    assert_eq!(project.run(&["build"]).0, 0);
    assert!(
        !project.dir.join("umay.lock").exists(),
        "a build must not write a lockfile; only `umay lock` does"
    );
    assert!(!project.dir.join("target").exists(), "no `target/` may appear");
}

#[test]
fn dist_is_the_only_thing_a_build_adds_to_a_project() {
    if !have_toolchain() {
        eprintln!("skipping dist_is_the_only_thing_a_build_adds_to_a_project: `cc` is not available");
        return;
    }
    let project = Project::created("build_no_target");
    let before = listing(&project.dir);
    assert_eq!(project.run(&["build"]).0, 0);
    let after = listing(&project.dir);
    for path in after {
        if before.contains(&path) {
            continue;
        }
        assert!(
            Path::new(&path).starts_with("dist"),
            "the build added `{path}`, which is outside dist/"
        );
    }
}

/// Every path under `dir`, relative, excluding the test's own cache.
fn listing(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == "cache") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// A dependency graph, not just a list
// ---------------------------------------------------------------------------

/// A dependency tree: a directory of packages, each with a manifest.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = std::env::temp_dir().join(format!("umay_tree_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the tree");
        Tree { root }
    }

    /// Adds a package with a name, version and dependency list.
    fn package(&self, name: &str, version: &str, deps: &[(&str, &str, &str)]) -> &Self {
        let dir = self.root.join(name);
        std::fs::create_dir_all(dir.join("src")).expect("create the package");
        // Every package gets both shapes of source: a library file for one that
        // is depended on, and an entry point for one that is built and run.
        std::fs::write(
            dir.join("src/lib.um"),
            format!("// {name}\nfn value() -> i64 {{\n    return 1i64\n}}\n"),
        )
        .expect("write the source");
        std::fs::write(
            dir.join("src/main.um"),
            "fn main() -> i32 {\n    return 0\n}\n",
        )
        .expect("write the entry point");
        let mut manifest = format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2026\"\n"
        );
        if !deps.is_empty() {
            manifest.push_str("\n[dependencies]\n");
            for (dep_name, dep_version, dep_path) in deps {
                manifest.push_str(&format!(
                    "{dep_name} = {{ path = \"{dep_path}\", version = \"{dep_version}\" }}\n"
                ));
            }
        }
        std::fs::write(dir.join("umay.toml"), manifest).expect("write the manifest");
        self
    }

    fn lock(&self, package: &str) -> (i32, String, String) {
        let output = Command::new(compiler())
            .current_dir(self.root.join(package))
            .env("UMAY_CACHE_DIR", self.root.join(".cache"))
            .arg("lock")
            .output()
            .expect("run umay lock");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn lockfile(&self, package: &str) -> String {
        std::fs::read_to_string(self.root.join(package).join("umay.lock")).unwrap_or_default()
    }
}

#[test]
fn a_dependency_of_a_dependency_is_locked_too() {
    // Before this walked the graph, only the direct dependency was recorded and a
    // lockfile claimed to pin a project's dependencies while leaving most of them
    // out.
    let tree = Tree::new("transitive");
    tree.package("base", "0.2.0", &[]);
    tree.package("lib", "1.0.0", &[("base", "0.2.0", "../base")]);
    tree.package("app", "0.1.0", &[("lib", "1.0.0", "../lib")]);

    let (status, stdout, stderr) = tree.lock("app");
    assert_eq!(status, 0, "locking should succeed:\n{stderr}");
    assert!(stdout.contains("2 packages"), "got {stdout}");
    let lock = tree.lockfile("app");
    assert!(lock.contains("name = \"lib\""), "got:\n{lock}");
    assert!(lock.contains("name = \"base\""), "the transitive dependency is missing:\n{lock}");
}

#[test]
fn a_chain_three_deep_is_fully_locked() {
    let tree = Tree::new("chain");
    tree.package("c", "1.0.0", &[]);
    tree.package("b", "1.0.0", &[("c", "1.0.0", "../c")]);
    tree.package("a", "1.0.0", &[("b", "1.0.0", "../b")]);
    tree.package("app", "0.1.0", &[("a", "1.0.0", "../a")]);

    assert_eq!(tree.lock("app").0, 0);
    let lock = tree.lockfile("app");
    for name in ["a", "b", "c"] {
        assert!(lock.contains(&format!("name = \"{name}\"")), "`{name}` is missing:\n{lock}");
    }
}

#[test]
fn a_diamond_locks_the_shared_dependency_once() {
    // Two routes to the same package is one package, not two entries and not a
    // conflict.
    let tree = Tree::new("diamond");
    tree.package("shared", "1.0.0", &[]);
    tree.package("left", "1.0.0", &[("shared", "1.0.0", "../shared")]);
    tree.package("right", "1.0.0", &[("shared", "1.0.0", "../shared")]);
    tree.package(
        "app",
        "0.1.0",
        &[("left", "1.0.0", "../left"), ("right", "1.0.0", "../right")],
    );

    let (status, stdout, stderr) = tree.lock("app");
    assert_eq!(status, 0, "a diamond should resolve:\n{stderr}");
    assert!(stdout.contains("3 packages"), "expected three packages, got {stdout}");
    let lock = tree.lockfile("app");
    assert_eq!(
        lock.matches("name = \"shared\"").count(),
        1,
        "the shared dependency should appear once:\n{lock}"
    );
}

#[test]
fn a_dependency_cycle_is_reported_with_its_chain() {
    let tree = Tree::new("cycle");
    tree.package("a", "1.0.0", &[("b", "1.0.0", "../b")]);
    tree.package("b", "1.0.0", &[("a", "1.0.0", "../a")]);
    tree.package("app", "0.1.0", &[("a", "1.0.0", "../a")]);

    let (status, stdout, stderr) = tree.lock("app");
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a cycle should not resolve silently:\n{report}");
    assert!(report.contains("cycle"), "the message should say what it found:\n{report}");
    assert!(
        report.contains("a -> b -> a"),
        "the chain should be named so it can be found:\n{report}"
    );
    assert!(
        !tree.root.join("app").join("umay.lock").exists(),
        "nothing should be written when the graph is not resolvable"
    );
}

#[test]
fn a_dependency_that_reaches_the_project_itself_is_reported() {
    let tree = Tree::new("self_cycle");
    tree.package("app", "0.1.0", &[]);
    // Rewrite the manifest so `app` depends on itself.
    std::fs::write(
        tree.root.join("app").join("umay.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nself_ref = { path = \".\", version = \"0.1.0\" }\n",
    )
    .expect("write the manifest");

    let (status, stdout, stderr) = tree.lock("app");
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a self-dependency should not resolve:\n{report}");
    assert!(report.contains("cycle"), "got:\n{report}");
}

#[test]
fn a_dependency_whose_version_disagrees_with_the_request_is_reported() {
    // The dependency's own manifest says what it is; the parent's manifest says
    // what was asked for. Quietly choosing either one gives the author a program
    // they did not ask for.
    let tree = Tree::new("version_mismatch");
    tree.package("lib", "0.9.0", &[]);
    tree.package("app", "0.1.0", &[("lib", "0.1.0", "../lib")]);

    let (status, stdout, stderr) = tree.lock("app");
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a version mismatch should not resolve:\n{report}");
    assert!(report.contains("0.9.0"), "the real version should be named:\n{report}");
    assert!(report.contains("0.1.0"), "the requested version should be named:\n{report}");
}

#[test]
fn the_same_name_at_two_versions_through_different_paths_is_reported() {
    let tree = Tree::new("two_versions");
    tree.package("lib_v1", "1.0.0", &[]);
    tree.package("lib_v2", "2.0.0", &[]);
    // Both packages declare a dependency called `lib`, at different versions. A
    // program cannot have two `lib`s, so this is a conflict rather than a choice.
    tree.package("left", "1.0.0", &[("lib", "1.0.0", "../lib_v1")]);
    tree.package("right", "1.0.0", &[("lib", "2.0.0", "../lib_v2")]);
    tree.package(
        "app",
        "0.1.0",
        &[("left", "1.0.0", "../left"), ("right", "1.0.0", "../right")],
    );

    let (status, stdout, stderr) = tree.lock("app");
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "two versions of one name should be reported:\n{report}");
    assert!(report.contains("lib"), "the name should be named:\n{report}");
}

#[test]
fn a_locked_graph_names_paths_relative_to_the_project() {
    // An absolute path would make the lockfile depend on the machine that wrote
    // it, which breaks both reproducibility and sharing the file.
    let tree = Tree::new("relative_paths");
    tree.package("base", "1.0.0", &[]);
    tree.package("lib", "1.0.0", &[("base", "1.0.0", "../base")]);
    tree.package("app", "0.1.0", &[("lib", "1.0.0", "../lib")]);

    assert_eq!(tree.lock("app").0, 0);
    let lock = tree.lockfile("app");
    // `source` is the line a path lives on; the header comment has a slash in it
    // and is none of this test's business.
    for line in lock.lines().filter(|l| l.starts_with("source = ")) {
        assert!(
            line.contains("\"../"),
            "a source path should be written relative to the project: {line}"
        );
    }
    let absolute_root = tree.root.to_string_lossy().into_owned();
    assert!(
        !lock.contains(&absolute_root),
        "the lockfile names the machine's paths:\n{lock}"
    );
    // The transitive dependency sits one level further out than the project.
    assert!(
        lock.contains("source = \"../base\""),
        "the transitive path should be written from the project:\n{lock}"
    );
}

#[test]
fn locking_a_graph_twice_produces_the_same_bytes() {
    let tree = Tree::new("graph_stable");
    tree.package("base", "1.0.0", &[]);
    tree.package("lib", "1.0.0", &[("base", "1.0.0", "../base")]);
    tree.package("app", "0.1.0", &[("lib", "1.0.0", "../lib")]);

    assert_eq!(tree.lock("app").0, 0);
    let first = tree.lockfile("app");
    assert_eq!(tree.lock("app").0, 0);
    assert_eq!(first, tree.lockfile("app"), "re-locking the same graph changed the file");
}

#[test]
fn a_build_warns_when_the_lockfile_no_longer_matches()
{
    // `verify` existed and nothing called it, so a lockfile was documentation
    // rather than a guarantee. The check has to happen somewhere the author
    // actually runs, and `build` is where they run.
    if !have_toolchain() {
        eprintln!("skipping a_build_warns_when_the_lockfile_no_longer_matches: `cc` is not available");
        return;
    }
    let tree = Tree::new("stale_lock");
    tree.package("base", "1.0.0", &[]);
    tree.package("lib", "1.0.0", &[("base", "1.0.0", "../base")]);
    tree.package("app", "0.1.0", &[("lib", "1.0.0", "../lib")]);
    let build = |tree: &Tree| {
        let output = Command::new(compiler())
            .current_dir(tree.root.join("app"))
            .env("UMAY_CACHE_DIR", tree.root.join(".cache"))
            .arg("build")
            .output()
            .expect("run umay build");
        (
            output.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    };

    assert_eq!(tree.lock("app").0, 0);
    let (status, report) = build(&tree);
    assert_eq!(status, 0, "a matching lockfile should not stop a build:\n{report}");
    assert!(
        !report.contains("no longer matches"),
        "a fresh lockfile should be quiet:\n{report}"
    );

    // Change the transitive dependency, which is the case the first version of
    // the check would have missed: it only looked at direct dependencies.
    std::fs::write(
        tree.root.join("base").join("src/lib.um"),
        "fn value() -> i64 {\n    return 99i64\n}\n",
    )
    .expect("change the dependency");

    let (status, report) = build(&tree);
    assert_eq!(status, 0, "a stale lockfile is a warning, not an error:\n{report}");
    assert!(
        report.contains("base"),
        "the changed dependency should be named:\n{report}"
    );
    assert!(
        report.contains("umay lock"),
        "the message should say what to do:\n{report}"
    );
}
