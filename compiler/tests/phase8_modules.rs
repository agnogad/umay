//! Modules loaded from files.
//!
//! `mod name;` without a body used to parse and do nothing: no file was read, so
//! a program that used the module reported `unknown name` whether the file was
//! there or not. That made the syntax impossible to debug, which is why these
//! tests care mostly about the errors -- a missing file, a cycle, the same file
//! named twice -- as much as the happy path.

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

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(tag: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("umay_mod_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the directory");
        Project { dir }
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create a parent directory");
        }
        std::fs::write(path, text).expect("write a file");
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::new(compiler())
            .current_dir(&self.dir)
            .env("UMAY_CACHE_DIR", self.dir.join(".cache"))
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
fn a_module_in_its_own_file_is_loaded() {
    let project = Project::new("flat");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println(util::banner())\n    return 0\n}\n\nmod util;\n",
    );
    project.write(
        "util.um",
        "fn banner() -> *const u8 {\n    return \"from util\"\n}\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "a loaded module should type-check:\n{stdout}{stderr}");
}

#[test]
fn a_module_in_its_own_directory_is_loaded() {
    // `name/mod.um` is the other spelling the language looks for.
    let project = Project::new("dir");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println(util::banner())\n    return 0\n}\n\nmod util;\n",
    );
    project.write(
        "util/mod.um",
        "fn banner() -> *const u8 {\n    return \"from a directory\"\n}\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "`name/mod.um` should be found:\n{stdout}{stderr}");
}

#[test]
fn a_missing_module_file_is_an_error_that_names_both_paths() {
    // The old behaviour was silence, which left the author guessing whether the
    // file was in the wrong place or the name was wrong.
    let project = Project::new("missing");
    project.write("main.um", "fn main() -> i32 {\n    return 0\n}\n\nmod ghost;\n");
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a missing module file is an error");
    assert!(report.contains("E0193"), "expected E0193:\n{report}");
    assert!(report.contains("ghost.um"), "the path looked for should be named:\n{report}");
    assert!(report.contains("mod.um"), "both spellings should be named:\n{report}");
}

#[test]
fn a_module_that_includes_itself_is_refused() {
    let project = Project::new("self");
    project.write("main.um", "fn main() -> i32 {\n    return 0\n}\n\nmod a;\n");
    project.write("a.um", "mod a;\n");
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a cycle must not be expanded");
    assert!(report.contains("E0192"), "expected E0192:\n{report}");
    assert!(report.contains("itself"), "the message should say what happened:\n{report}");
}

#[test]
fn two_modules_that_include_each_other_are_refused() {
    let project = Project::new("mutual");
    project.write("main.um", "fn main() -> i32 {\n    return 0\n}\n\nmod a;\n");
    project.write("a.um", "mod b;\n");
    project.write("b.um", "mod a;\n");
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a mutual cycle must not be expanded");
    assert!(report.contains("E0192"), "expected E0192:\n{report}");
}

#[test]
fn a_module_named_twice_is_loaded_once() {
    // Loading it twice would duplicate every item in it, which would be reported
    // as a duplicate definition in the module rather than in the file that named
    // it twice.
    let project = Project::new("twice");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    return 0\n}\n\nmod a;\nmod a;\n",
    );
    project.write("a.um", "fn f() -> i64 {\n    return 1i64\n}\n");
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "the same module twice is one module:\n{stdout}{stderr}");
}

#[test]
fn nested_modules_resolve_relative_to_their_own_file() {
    let project = Project::new("nested");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println(a::b::name())\n    return 0\n}\n\nmod a;\n",
    );
    project.write("a/mod.um", "mod b;\n");
    project.write("a/b.um", "fn name() -> *const u8 {\n    return \"deep\"\n}\n");
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "`a::b::name` should resolve:\n{stdout}{stderr}");
}

#[test]
fn an_inline_module_still_works_and_its_items_are_named_by_its_path() {
    let project = Project::new("inline");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println(inner::greet())\n    return 0\n}\n\nmod inner {\n    fn greet() -> *const u8 {\n        return \"hi\"\n    }\n}\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "an inline module should keep its items reachable:\n{stdout}{stderr}");
}

#[test]
fn an_error_inside_a_module_points_at_the_module_file() {
    // Reporting the error against the file that declared the module would send
    // the reader to a line they cannot fix.
    let project = Project::new("error_place");
    project.write("main.um", "fn main() -> i32 {\n    return 0\n}\n\nmod util;\n");
    project.write(
        "util.um",
        "fn banner() -> *const u8 {\n    return 42\n}\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "the module has an error");
    assert!(
        report.contains("util.um"),
        "the error should be reported against the module's file:\n{report}"
    );
}

#[test]
fn a_module_file_is_compiled_into_the_program() {
    if !have_toolchain() {
        eprintln!("skipping a_module_file_is_compiled_into_the_program: `cc` is not available");
        return;
    }
    let project = Project::new("codegen");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println(greet::message())\n    return 0\n}\n\nmod greet;\n",
    );
    project.write(
        "greet.um",
        "fn message() -> *const u8 {\n    return \"hello from a module\"\n}\n",
    );
    let (status, stdout, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "running should succeed:\n{stderr}");
    assert!(
        stdout.contains("hello from a module"),
        "the module's code should run, got:\n{stdout}"
    );
}

#[test]
fn editing_a_module_changes_the_output() {
    // Module-level invalidation, the thing the cache could not do while the
    // compiler only read one file: a rebuild after a module edit must not reuse
    // the assembly from before it.
    if !have_toolchain() {
        eprintln!("skipping editing_a_module_changes_the_output: `cc` is not available");
        return;
    }
    let project = Project::new("rebuild");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println(greet::message())\n    return 0\n}\n\nmod greet;\n",
    );
    project.write(
        "greet.um",
        "fn message() -> *const u8 {\n    return \"first\"\n}\n",
    );
    let (status, first, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the first run should succeed:\n{stderr}");
    assert!(first.contains("first"), "got:\n{first}");

    project.write(
        "greet.um",
        "fn message() -> *const u8 {\n    return \"second\"\n}\n",
    );
    let (status, second, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the second run should succeed:\n{stderr}");
    assert!(
        second.contains("second"),
        "a rebuild after a module edit reused the old assembly, got:\n{second}"
    );
    assert!(!second.contains("first"), "the old text must not survive:\n{second}");
}
