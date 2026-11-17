//! What each command says about warnings.
//!
//! A warning is worth exactly as much as the chance the user gets to read it.
//! The compiler once reported `W0102` from `check` and silently dropped it from
//! `build`, which meant the same program had a warning or not depending on which
//! command you happened to run. These tests pin the rule: every command that
//! compiles a program the user asked to build reports what it saw.

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
    ["cc"].iter().all(|tool| {
        Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// A program that compiles and warns: the cast narrows a type on purpose.
const NARROWING: &str = "fn main() -> i32 {\n    let big = 1i64\n    let small = big as i32\n    return 0\n}\n";

/// A program with a plain error, so a failing build has something to report.
const BROKEN: &str = "fn main() -> i32 {\n    return \"not an integer\"\n}\n";

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

fn umay_in(dir: &std::path::Path, args: &[&str]) -> Run {
    let output = Command::new(compiler())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run umay");
    Run {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn scratch(name: &str, src: &str) -> PathBuf {
    // The name is in the path, and the tests run in parallel under one pid, so
    // two tests that shared a directory would read each other's source.
    let dir = std::env::temp_dir().join(format!("umay_diag_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("main.um"), src).expect("write source");
    dir
}

#[test]
fn check_reports_a_warning() {
    let dir = scratch("check_warn", NARROWING);
    let run = umay_in(&dir, &["check", "main.um"]);
    assert_eq!(run.status, 0, "a warning is not an error:\n{}", run.stderr);
    assert!(run.stdout.contains("W0102"), "expected W0102 in:\n{}", run.stdout);
}

#[test]
fn build_reports_the_same_warning_check_does() {
    if !have_toolchain() {
        eprintln!("skipping build_reports_the_same_warning_check_does: `cc` is not available");
        return;
    }
    let dir = scratch("build_warn", NARROWING);
    let check = umay_in(&dir, &["check", "main.um"]);
    let build = umay_in(&dir, &["build", "main.um"]);
    assert_eq!(build.status, 0, "the program is correct:\n{}", build.stderr);
    // Both commands render diagnostics the same way and to the same stream, so a
    // wrapper that captures one captures the other.
    assert!(
        build.stdout.contains("W0102"),
        "build dropped a warning that check reported:\n{}",
        build.stdout
    );
    // `build` follows its report with the line naming what it produced, so the
    // report is what comes before it.
    let build_report = build.stdout.split("\nbuilt ").next().unwrap_or_default().trim_end();
    assert_eq!(
        build_report, check.stdout.trim_end(),
        "the two commands should describe the warning identically"
    );
}

#[test]
fn run_reports_the_warning_before_the_output() {
    if !have_toolchain() {
        eprintln!("skipping run_reports_the_warning_before_the_output: `cc` is not available");
        return;
    }
    let dir = scratch("run_warn", NARROWING);
    let run = umay_in(&dir, &["run", "main.um"]);
    assert_eq!(run.status, 0, "the program runs:\n{}", run.stderr);
    assert!(run.stdout.contains("W0102"), "expected W0102 in:\n{}", run.stdout);
}

#[test]
fn a_successful_build_without_warnings_says_nothing() {
    if !have_toolchain() {
        eprintln!("skipping a_successful_build_without_warnings_says_nothing: `cc` is not available");
        return;
    }
    let dir = scratch("build_quiet", "fn main() -> i32 {\n    return 0\n}\n");
    let build = umay_in(&dir, &["build", "main.um"]);
    assert_eq!(build.status, 0);
    assert!(
        build.stderr.trim().is_empty(),
        "a clean build should not print diagnostics:\n{}",
        build.stderr
    );
}

#[test]
fn a_failing_build_can_report_json() {
    if !have_toolchain() {
        eprintln!("skipping a_failing_build_can_report_json: `cc` is not available");
        return;
    }
    let dir = scratch("build_json", BROKEN);
    let run = umay_in(&dir, &["build", "--format", "json", "main.um"]);
    assert_ne!(run.status, 0, "the program does not type check");
    // A driver reads one shape from every command, so a failure has to arrive as
    // JSON rather than as the terminal rendering.
    let parsed: serde_like_json::Value =
        serde_like_json::parse(&run.stdout).expect("stdout should be JSON");
    let version = parsed.get("version").and_then(|v| v.as_text());
    assert_eq!(version.as_deref(), Some("1"));
    let codes = parsed.strings_under("code");
    assert!(
        codes.iter().any(|c| c.starts_with('E') || c.starts_with('W')),
        "expected a diagnostic code in {codes:?}"
    );
    assert!(run.stderr.trim().is_empty(), "stderr should stay empty:\n{}", run.stderr);
}

#[test]
fn json_from_a_warning_build_is_the_same_document_check_produces() {
    if !have_toolchain() {
        eprintln!("skipping json_from_a_warning_build_is_the_same_document_check_produces: `cc` is not available");
        return;
    }
    let dir = scratch("build_json_warn", NARROWING);
    let check = umay_in(&dir, &["check", "--format", "json", "main.um"]);
    let build = umay_in(&dir, &["build", "--format", "json", "main.um"]);
    assert_eq!(build.status, 0);
    // `build` prints its own "built dist/main" line after the report, so only the
    // first line is the document under comparison.
    let build_json = build.stdout.lines().next().unwrap_or_default();
    assert_eq!(
        build_json.trim(),
        check.stdout.trim(),
        "a driver should not be able to tell the two apart"
    );
}

/// Just enough JSON to read this compiler's diagnostic documents.
///
/// The compiler emits one fixed shape, so a full parser would be more code than
/// it is worth. This reads objects, arrays and strings, and finds values by key.
mod serde_like_json {
    #[derive(Debug, Clone)]
    pub enum Value {
        Null,
        Bool,
        Number(f64),
        Str(String),
        Arr(Vec<Value>),
        Obj(Vec<(String, Value)>),
    }

    impl Value {
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }

        /// Every string value filed under `key`, anywhere in the document.
        pub fn strings_under(&self, key: &str) -> Vec<String> {
            let mut out = Vec::new();
            self.collect(key, &mut out);
            out
        }

        fn collect(&self, key: &str, out: &mut Vec<String>) {
            match self {
                Value::Obj(fields) => {
                    for (k, v) in fields {
                        if k == key {
                            if let Value::Str(s) = v {
                                out.push(s.clone());
                            }
                        }
                        v.collect(key, out);
                    }
                }
                Value::Arr(items) => {
                    for item in items {
                        item.collect(key, out);
                    }
                }
                _ => {}
            }
        }
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl<'a> Parser<'a> {
        fn skip_space(&mut self) {
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
        }

        fn value(&mut self) -> Option<Value> {
            self.skip_space();
            let c = *self.bytes.get(self.pos)?;
            match c {
                b'{' => {
                    self.pos += 1;
                    let mut fields = Vec::new();
                    loop {
                        self.skip_space();
                        if *self.bytes.get(self.pos)? == b'}' {
                            self.pos += 1;
                            return Some(Value::Obj(fields));
                        }
                        let key = match self.value()? {
                            Value::Str(s) => s,
                            _ => return None,
                        };
                        self.skip_space();
                        if *self.bytes.get(self.pos)? != b':' {
                            return None;
                        }
                        self.pos += 1;
                        let value = self.value()?;
                        fields.push((key, value));
                        self.skip_space();
                        match *self.bytes.get(self.pos)? {
                            b',' => self.pos += 1,
                            b'}' => {
                                self.pos += 1;
                                return Some(Value::Obj(fields));
                            }
                            _ => return None,
                        }
                    }
                }
                b'[' => {
                    self.pos += 1;
                    let mut items = Vec::new();
                    loop {
                        self.skip_space();
                        if *self.bytes.get(self.pos)? == b']' {
                            self.pos += 1;
                            return Some(Value::Arr(items));
                        }
                        items.push(self.value()?);
                        self.skip_space();
                        match *self.bytes.get(self.pos)? {
                            b',' => self.pos += 1,
                            b']' => {
                                self.pos += 1;
                                return Some(Value::Arr(items));
                            }
                            _ => return None,
                        }
                    }
                }
                b'"' => {
                    self.pos += 1;
                    let mut out = String::new();
                    while let Some(&b) = self.bytes.get(self.pos) {
                        self.pos += 1;
                        match b {
                            b'"' => return Some(Value::Str(out)),
                            b'\\' => {
                                let esc = *self.bytes.get(self.pos)?;
                                self.pos += 1;
                                match esc {
                                    b'n' => out.push('\n'),
                                    b't' => out.push('\t'),
                                    b'r' => out.push('\r'),
                                    b'u' => {
                                        let hex = std::str::from_utf8(
                                            self.bytes.get(self.pos..self.pos + 4)?,
                                        )
                                        .ok()?;
                                        self.pos += 4;
                                        let code = u32::from_str_radix(hex, 16).ok()?;
                                        out.push(char::from_u32(code)?);
                                    }
                                    other => out.push(other as char),
                                }
                            }
                            other if other < 0x80 => out.push(other as char),
                            _ => return None,
                        }
                    }
                    None
                }
                b't' if self.bytes[self.pos..].starts_with(b"true") => {
                    self.pos += 4;
                    Some(Value::Bool)
                }
                b'f' if self.bytes[self.pos..].starts_with(b"false") => {
                    self.pos += 5;
                    Some(Value::Bool)
                }
                b'n' if self.bytes[self.pos..].starts_with(b"null") => {
                    self.pos += 4;
                    Some(Value::Null)
                }
                _ => {
                    let start = self.pos;
                    while let Some(&b) = self.bytes.get(self.pos) {
                        if b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E') {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
                    text.parse().ok().map(Value::Number)
                }
            }
        }
    }

    pub fn parse(text: &str) -> Option<Value> {
        let mut parser = Parser { bytes: text.as_bytes(), pos: 0 };
        let value = parser.value()?;
        parser.skip_space();
        if parser.pos == parser.bytes.len() {
            Some(value)
        } else {
            None
        }
    }
}

impl serde_like_json::Value {
    /// Reads a scalar as text, so an assertion can name it.
    pub fn as_text(&self) -> Option<String> {
        match self {
            serde_like_json::Value::Str(s) => Some(s.clone()),
            serde_like_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }
}

#[test]
fn a_warning_is_reported_on_every_build_not_just_the_first() {
    // The cache holds the assembly and nothing else, so a cache hit has no
    // diagnostics with it. Caching a build that warned made the warning appear
    // once and then vanish -- which is worse than never showing it, because the
    // user learns they cannot trust a quiet build.
    if !have_toolchain() {
        eprintln!("skipping a_warning_is_reported_on_every_build_not_just_the_first: `cc` is not available");
        return;
    }
    let dir = scratch("warn_every_time", NARROWING);
    // A cache of this test's own, so a warm entry from another test cannot decide
    // the result.
    let cache = dir.join("cache");
    let build = |dir: &std::path::Path| {
        let output = Command::new(compiler())
            .current_dir(dir)
            .env("UMAY_CACHE_DIR", &cache)
            .args(["build", "main.um"])
            .output()
            .expect("run umay");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    };

    for attempt in 1..=3 {
        let (status, stdout) = build(&dir);
        assert_eq!(status, 0, "the program is correct");
        assert!(
            stdout.contains("W0102"),
            "build {attempt} dropped the warning it reported before:\n{stdout}"
        );
    }
}

#[test]
fn every_emitted_error_code_has_an_explanation() {
    // `umay explain` is a promise that every code the compiler can emit has a
    // stable explanation. Four module/trait codes once escaped it because
    // nothing checked the promise; this walks the source instead of keeping a
    // hand-maintained list that would go stale the same way.
    fn collect(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read the src directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "explain.rs")
            {
                let text = std::fs::read_to_string(&path).expect("read source");
                let bytes = text.as_bytes();
                let mut i = 0;
                while i + 6 < bytes.len() {
                    if bytes[i] == b'"'
                        && bytes[i + 1] == b'E'
                        && bytes[i + 2..i + 6].iter().all(u8::is_ascii_digit)
                        && bytes[i + 6] == b'"'
                    {
                        let code = text[i + 1..i + 6].to_string();
                        if !out.contains(&code) {
                            out.push(code);
                        }
                        i += 7;
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }

    let mut codes = Vec::new();
    collect(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"), &mut codes);
    codes.sort();
    assert!(!codes.is_empty(), "the scan must find at least one code");
    for code in codes {
        assert!(
            umayc::explain::lookup(&code).is_some(),
            "no `umay explain` entry for {code}"
        );
    }
}
