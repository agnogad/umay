//! Phase 17: `umay bindgen` — the first C-header slice.
//!
//! These drive the real `umay` CLI binary. They never link against a C library:
//! the generated module is only asked to parse/type-check through `umay check`.
//!
//! The generated module targets v2 `extern "C"` syntax (spec §5), which is gated
//! by `#![feature(extern_abi)]`. While the Phase 2 parser/feature-gate work is
//! still landing, the `check` assertions here may fail for reasons unrelated to
//! bindgen itself; the bindgen-only assertions do not depend on it.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

fn umay() -> Command {
    Command::new(env!("CARGO_BIN_EXE_umay"))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "umay-bindgen-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, text).expect("write temp file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("run umay binary")
}

fn assert_ok(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed (status {:?}):\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bindgen_to_file(header: &PathBuf, out: &PathBuf) -> Output {
    run(umay().args(["bindgen", "--header"]).arg(header).args(["--out"]).arg(out))
}

#[test]
fn small_header_produces_a_module_that_check_accepts() {
    let dir = TempDir::new("small");
    let header = dir.write(
        "small.h",
        "int add(int a, int b);\nvoid *c_alloc(unsigned long n);\n",
    );
    let out = dir.path("small.um");

    assert_ok(&bindgen_to_file(&header, &out), "bindgen");
    let module = std::fs::read_to_string(&out).expect("read generated module");
    assert!(module.contains("extern \"C\" {"), "{module}");
    assert!(module.contains("#![feature(extern_abi)]"), "{module}");
    assert!(module.contains("fn add(a: i32, b: i32) -> i32;"), "{module}");
    assert!(module.contains("fn c_alloc(n: u64) -> *mut u8;"), "{module}");

    assert_ok(&run(umay().arg("check").arg(&out)), "check");
}

#[test]
fn unknown_struct_pointer_becomes_an_opaque_extern_type() {
    let dir = TempDir::new("opaque");
    let header = dir.write(
        "opaque.h",
        "struct Widget;\n\
         struct Widget *widget_create(void);\n\
         void widget_destroy(struct Widget *w);\n",
    );
    let out = dir.path("opaque.um");

    assert_ok(&bindgen_to_file(&header, &out), "bindgen");
    let module = std::fs::read_to_string(&out).expect("read generated module");
    assert!(module.contains("fn widget_create() -> *mut Widget;"), "{module}");
    assert!(module.contains("fn widget_destroy(w: *mut Widget);"), "{module}");
    assert!(module.contains("    type Widget;"), "{module}");

    assert_ok(&run(umay().arg("check").arg(&out)), "check");
}

#[test]
fn malformed_header_does_not_panic() {
    let dir = TempDir::new("malformed");
    // A valid declaration, a bad parameter type, then a truncated tail and an
    // unterminated block comment. None of this may panic the CLI.
    let header = dir.write(
        "malformed.h",
        "int ok(int a);\nint broken(int a, @);\nint truncated(int b\n/* never closed\n",
    );
    let out = dir.path("malformed.um");
    let result = run(
        umay()
            .args(["bindgen", "--header"])
            .arg(&header)
            .arg("--emit-stdout"),
    );
    assert_ok(&result, "bindgen on malformed header");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!stderr.contains("panicked"), "bindgen panicked: {stderr}");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("fn ok(a: i32) -> i32;"), "{stdout}");

    // The truncated tail is ignored; whatever remains is still a valid module.
    std::fs::write(&out, stdout.as_bytes()).expect("write generated module");
    assert_ok(&run(umay().arg("check").arg(&out)), "check");
}

#[test]
fn skipped_declarations_appear_as_comments() {
    let dir = TempDir::new("skipped");
    let header = dir.write(
        "skipped.h",
        "#include <stddef.h>\n\
         #define LIMIT 10\n\
         typedef unsigned long my_ulong;\n\
         long double imprecise(double x);\n\
         int log_msg(const char *fmt, ...);\n",
    );
    let result = run(
        umay()
            .args(["bindgen", "--header"])
            .arg(&header)
            .arg("--emit-stdout"),
    );
    assert_ok(&result, "bindgen");
    let module = String::from_utf8_lossy(&result.stdout);

    assert!(module.contains("umay-bindgen: skipped"), "{module}");
    assert!(module.contains("typedef"), "{module}");
    assert!(module.contains("long double"), "{module}");
    // The v2 grammar accepts a terminal `...` in an extern block (spec §5),
    // so variadic functions are emitted rather than skipped.
    assert!(module.contains("fn log_msg(fmt: *const i8, ...) -> i32;"), "{module}");
    assert!(!module.contains("#include"), "{module}");
    assert!(!module.contains("my_ulong;"), "{module}");
}
