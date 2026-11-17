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
        "pub fn banner() -> *const u8 {\n    return \"from util\"\n}\n",
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
        "pub fn banner() -> *const u8 {\n    return \"from a directory\"\n}\n",
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
    project.write("a/b.um", "pub fn name() -> *const u8 {\n    return \"deep\"\n}\n");
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "`a::b::name` should resolve:\n{stdout}{stderr}");
}

#[test]
fn an_inline_module_still_works_and_its_items_are_named_by_its_path() {
    let project = Project::new("inline");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println(inner::greet())\n    return 0\n}\n\nmod inner {\n    pub fn greet() -> *const u8 {\n        return \"hi\"\n    }\n}\n",
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
        "pub fn message() -> *const u8 {\n    return \"hello from a module\"\n}\n",
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
        "pub fn message() -> *const u8 {\n    return \"first\"\n}\n",
    );
    let (status, first, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the first run should succeed:\n{stderr}");
    assert!(first.contains("first"), "got:\n{first}");

    project.write(
        "greet.um",
        "pub fn message() -> *const u8 {\n    return \"second\"\n}\n",
    );
    let (status, second, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the second run should succeed:\n{stderr}");
    assert!(
        second.contains("second"),
        "a rebuild after a module edit reused the old assembly, got:\n{second}"
    );
    assert!(!second.contains("first"), "the old text must not survive:\n{second}");
}

// ---------------------------------------------------------------------------
// The other tools see modules too
// ---------------------------------------------------------------------------

#[test]
fn metadata_reports_the_items_in_a_module_file() {
    // A tool asking what a project declares should hear about its modules, not
    // only its entry point.
    let project = Project::new("metadata");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    return 0\n}\n\nmod util;\n",
    );
    project.write("util.um", "fn helper() -> i64 {\n    return 1i64\n}\n");
    let (status, stdout, stderr) = project.run(&["metadata", "main.um"]);
    assert_eq!(status, 0, "metadata should succeed:\n{stderr}");
    assert!(stdout.contains("fn main()"), "got:\n{stdout}");
    assert!(
        stdout.contains("util::helper"),
        "the module's function should be listed under its path:\n{stdout}"
    );
}

#[test]
fn metadata_fails_the_same_way_check_does_on_a_missing_module() {
    let project = Project::new("metadata_missing");
    project.write("main.um", "fn main() -> i32 {\n    return 0\n}\n\nmod ghost;\n");
    let (status, stdout, stderr) = project.run(&["metadata", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a missing module is an error here too");
    assert!(report.contains("E0193"), "got:\n{report}");
}

#[test]
fn the_language_server_resolves_modules_from_the_document_path() {
    // The checker resolves `mod name;` against the file's directory, so a buffer
    // has to tell the server where it lives or every module is a missing file.
    use std::io::Write;
    use std::process::{Command, Stdio};

    let project = Project::new("lsp_path");
    let uri = format!(
        "file://{}/main.um",
        project.dir.to_string_lossy().trim_end_matches('/')
    );
    // The module file exists, so a server that knows where the buffer lives
    // reports nothing.
    project.write("util.um", "fn helper() -> i64 {\n    return 1i64\n}\n");

    let source = "fn main() -> i32 {\n    println(util::helper())\n    return 0\n}\n\nmod util;\n";
    let open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"umay","version":1,"text":{}}}}}}}"#,
        json_string(source)
    );
    let messages = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#.to_string(),
        open,
    ];

    let mut child = Command::new(compiler())
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the server");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for message in &messages {
            stdin
                .write_all(&frame(message))
                .expect("write a message");
        }
    }
    let output = child.wait_with_output().expect("the server should finish");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !stdout.contains("E0193"),
        "the server reported a missing module it should have found:\n{stdout}"
    );
    assert!(
        !stdout.contains("E0101"),
        "the module's function should resolve:\n{stdout}"
    );
}

fn frame(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[test]
fn two_modules_may_declare_the_same_function_name()
{
    // Two dependencies that both define `run` is the ordinary case, not a
    // mistake. Registering a module's items under their bare names made the
    // second one a duplicate definition, and made whichever was collected first
    // answer for the other: `b::run()` called `a::run()`.
    if !have_toolchain() {
        eprintln!("skipping two_modules_may_declare_the_same_function_name: `cc` is not available");
        return;
    }
    let project = Project::new("same_fn_name");
    project.write("a.um", "pub fn run() -> i64 {\n    return 1i64\n}\n");
    project.write("b.um", "pub fn run() -> i64 {\n    return 2i64\n}\n");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(a::run())\n    println_i64(b::run())\n    return 0\n}\n\nmod a;\nmod b;\n",
    );

    let (status, stdout, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the program should run:\n{stderr}");
    let numbers: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.starts_with("built "))
        .collect();
    assert_eq!(
        numbers,
        ["1", "2"],
        "each module's function should answer for itself; full output was {stdout:?}"
    );
}

#[test]
fn a_public_method_inside_an_impl_parses()
{
    // `pub fn` inside an `impl` was a parse error, so the ordinary way of writing
    // a method -- especially in a module, where it is meant to be reachable from
    // outside -- could not be written at all.
    if !have_toolchain() {
        eprintln!("skipping a_public_method_inside_an_impl_parses: `cc` is not available");
        return;
    }
    let project = Project::new("pub_method");
    project.write(
        "main.um",
        "struct Counter {\n    value: i64,\n}\n\nimpl Counter {\n    pub fn add(self, n: i64) -> i64 {\n        return self.value + n\n    }\n}\n\nfn main() -> i32 {\n    let c: Counter = Counter { value: 3i64 }\n    println_i64(c.add(4i64))\n    return 0\n}\n",
    );

    let (status, stdout, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "`pub fn` in an impl should parse and run:\n{stderr}");
    assert!(stdout.contains('7'), "the method should run, got {stdout:?}");
}

#[test]
fn a_struct_and_its_method_work_from_inside_a_module()
{
    if !have_toolchain() {
        eprintln!("skipping a_struct_and_its_method_work_from_inside_a_module: `cc` is not available");
        return;
    }
    let project = Project::new("mod_struct");
    project.write(
        "geo.um",
        "pub struct Point {\n    x: i64,\n    y: i64,\n}\n\nimpl Point {\n    pub fn sum(self) -> i64 {\n        return self.x + self.y\n    }\n}\n",
    );
    project.write(
        "main.um",
        "fn main() -> i32 {\n    let p: geo::Point = geo::Point { x: 3i64, y: 4i64 }\n    println_i64(p.sum())\n    return 0\n}\n\nmod geo;\n",
    );

    let (status, stdout, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "a module's type and method should be usable:\n{stderr}");
    assert!(stdout.contains('7'), "expected 3 + 4, got {stdout:?}");
}

#[test]
fn an_attribute_does_not_turn_the_next_token_into_an_error()
{
    // `parse_attribute` consumes a `#` before it looks at what follows, so
    // calling it unconditionally made every token after a method an "expected
    // `[` after `#`" error.
    let project = Project::new("no_attribute");
    project.write(
        "main.um",
        "struct S {\n    v: i64,\n}\n\nimpl S {\n    fn get(self) -> i64 {\n        return self.v\n    }\n}\n\nfn main() -> i32 {\n    return 0\n}\n",
    );

    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_eq!(status, 0, "a method with no attribute should parse:\n{report}");
    assert!(
        !report.contains("after `#`"),
        "the parser should not complain about a `#` that is not there:\n{report}"
    );
}

#[test]
fn a_method_may_carry_an_attribute()
{
    let project = Project::new("method_attribute");
    project.write(
        "main.um",
        "struct S {\n    v: i64,\n}\n\nimpl S {\n    #[inline]\n    pub fn get(self) -> i64 {\n        return self.v\n    }\n}\n\nfn main() -> i32 {\n    return 0\n}\n",
    );

    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "an attributed method should parse:\n{stdout}{stderr}");
}

#[test]
fn a_module_exposes_constants_enums_and_their_variants()
{
    // Constants, enum variants and types were each looked up separately, and each
    // looked up by its bare name. A module registers `module::Name`, so all three
    // were "unknown name" from outside -- and the enum's own variants were never
    // attached to it at all, because the definition was found by bare name too.
    if !have_toolchain() {
        eprintln!("skipping a_module_exposes_constants_enums_and_their_variants: `cc` is not available");
        return;
    }
    let project = Project::new("module_surface");
    project.write(
        "kinds.um",
        "pub enum Kind {\n    Small,\n    Large,\n}\n\npub const LIMIT: i64 = 3i64;\n\npub fn weigh(k: Kind) -> i64 {\n    match k {\n        Kind::Small => return 1i64,\n        Kind::Large => return 2i64,\n    }\n}\n",
    );
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(kinds::LIMIT)\n    println_i64(kinds::weigh(kinds::Kind::Large))\n    return 0\n}\n\nmod kinds;\n",
    );

    let (status, stdout, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "the module's constants and enum should be usable:\n{stderr}");
    let numbers: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.starts_with("built "))
        .collect();
    assert_eq!(numbers, ["3", "2"], "got {stdout:?} (stderr {stderr:?})");
}

#[test]
fn a_module_may_declare_a_constant_with_the_same_name_as_another()
{
    // Two modules each declaring `LIMIT` is the ordinary case. A constant is a
    // name like any other, and the module is what tells them apart.
    let project = Project::new("two_constants");
    project.write("a.um", "pub const LIMIT: i64 = 1i64;\n");
    project.write("b.um", "pub const LIMIT: i64 = 2i64;\n");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(a::LIMIT)\n    println_i64(b::LIMIT)\n    return 0\n}\n\nmod a;\nmod b;\n",
    );

    if !have_toolchain() {
        eprintln!("skipping a_module_may_declare_a_constant_with_the_same_name_as_another: `cc` is not available");
        return;
    }
    let (status, stdout, stderr) = project.run(&["run", "main.um"]);
    assert_eq!(status, 0, "two modules may each declare `LIMIT`:\n{stderr}");
    let numbers: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.starts_with("built "))
        .collect();
    assert_eq!(numbers, ["1", "2"], "got {stdout:?}");
}

#[test]
fn a_private_function_is_not_reachable_from_another_module() {
    let project = Project::new("private_fn");
    project.write("util.um", "fn helper() -> i64 {\n    return 7i64\n}\n");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(util::helper())\n    return 0\n}\n\nmod util;\n",
    );
    let (status, _stdout, stderr) = project.run(&["check", "main.um"]);
    assert_ne!(status, 0, "a private function must not be callable from outside");
    let report = format!("{_stdout}{stderr}");
    assert!(report.contains("E1018"), "expected E1018:\n{report}");
    assert!(report.contains("util::helper"), "the private name should be named:\n{report}");
}

#[test]
fn a_private_constant_is_not_reachable_from_another_module() {
    let project = Project::new("private_const");
    project.write("cfg.um", "const LIMIT: i64 = 7i64;\n");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(cfg::LIMIT)\n    return 0\n}\n\nmod cfg;\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a private constant must not be readable from outside");
    assert!(report.contains("E1018"), "expected E1018:\n{report}");
}

#[test]
fn a_private_struct_is_not_reachable_from_another_module() {
    let project = Project::new("private_struct");
    project.write("geo.um", "struct Point {\n    x: i64,\n}\n");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    let p: geo::Point = geo::Point { x: 1i64 }\n    return 0\n}\n\nmod geo;\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a private struct must not be constructible from outside");
    assert!(report.contains("E1018"), "expected E1018:\n{report}");
}

#[test]
fn a_private_enum_is_not_reachable_from_another_module() {
    let project = Project::new("private_enum");
    project.write("kinds.um", "enum Kind {\n    Small,\n    Large,\n}\n");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    let k: kinds::Kind = kinds::Kind::Large\n    return 0\n}\n\nmod kinds;\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a private enum must not be nameable from outside");
    assert!(report.contains("E1018"), "expected E1018:\n{report}");
}

#[test]
fn a_module_may_use_its_own_private_item_by_its_qualified_name() {
    let project = Project::new("private_self");
    project.write(
        "util.um",
        "fn helper() -> i64 {\n    return 7i64\n}\n\npub fn run() -> i64 {\n    return util::helper()\n}\n",
    );
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(util::run())\n    return 0\n}\n\nmod util;\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "a module's own private item is visible to it:\n{stdout}{stderr}");
}

#[test]
fn a_private_function_is_not_reachable_by_bare_name() {
    // A private module item used to be registered in the global bare-name
    // table, so `util::helper()` could be called as just `helper()` from
    // anywhere. That made the `pub` check trivial to bypass.
    let project = Project::new("private_bare");
    project.write("util.um", "fn helper() -> i64 {\n    return 7i64\n}\n");
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(helper())\n    return 0\n}\n\nmod util;\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "--explain", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a private function must not resolve by bare name");
    assert!(report.contains("E0101"), "expected an unknown-name error:\n{report}");
    assert!(report.contains("private to module `util`"), "the cause should be named:\n{report}");
}

#[test]
fn a_private_method_is_not_reachable_from_another_module() {
    let project = Project::new("private_method");
    project.write(
        "geo.um",
        "pub struct Point {\n    x: i64,\n}\n\nimpl Point {\n    fn hidden(self) -> i64 {\n        return self.x\n    }\n}\n\npub fn make() -> Point {\n    return Point { x: 7i64 }\n}\n",
    );
    project.write(
        "main.um",
        "fn main() -> i32 {\n    let p = geo::make()\n    println_i64(p.hidden())\n    return 0\n}\n\nmod geo;\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    let report = format!("{stdout}{stderr}");
    assert_ne!(status, 0, "a private method must not be callable from outside");
    assert!(report.contains("E1018"), "expected E1018:\n{report}");
}

#[test]
fn a_module_can_use_its_own_private_method() {
    let project = Project::new("private_method_self");
    project.write(
        "geo.um",
        "pub struct Point {\n    x: i64,\n}\n\nimpl Point {\n    fn hidden(self) -> i64 {\n        return self.x\n    }\n}\n\npub fn value() -> i64 {\n    let p = Point { x: 9i64 }\n    return p.hidden()\n}\n",
    );
    project.write(
        "main.um",
        "fn main() -> i32 {\n    println_i64(geo::value())\n    return 0\n}\n\nmod geo;\n",
    );
    let (status, stdout, stderr) = project.run(&["check", "main.um"]);
    assert_eq!(status, 0, "a module's own private method is visible to it:\n{stdout}{stderr}");
}
