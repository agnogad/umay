//! Pure-UMAY JSON IPC codec for the mobile webview bridge (docs/v2/webview-decision.md).
//!
//! `stdlib/ipc.um` is spliced at crate root above a generated shell, so the first
//! group feeds the real file text into the in-process lexer -> parser ->
//! with_prelude -> sema harness and the `umay check` CLI. The rest builds and
//! runs whole programs on the host, asserting exact scalar stdout and exact
//! encoded JSON strings -- no webview or native adapter needed.

use std::path::PathBuf;
use std::process::Command;

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::target::Target;

const IPC_UM: &str = include_str!("../../stdlib/ipc.um");

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_mobile_ipc_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn toolchain_available() -> bool {
    if cfg!(windows) {
        return false;
    }
    let ok = |name: &str| {
        Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

/// lex -> parse -> prelude -> `sema::analyze`, mirroring phase20_gui_core.rs.
fn analyze(src: &str) -> Diagnostics {
    let mut map = SourceMap::new();
    let file = map.add("<ipc>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, file);
    assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}\n---\n{src}");
    let parsed = parser::parse(tokens, Vec::new(), file);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}\n---\n{src}");
    let (program, prelude_diags) =
        umayc::prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
    assert!(!prelude_diags.has_errors(), "prelude errors: {prelude_diags:?}");
    let out = sema::analyze(&program, &map);
    if out.diagnostics.has_errors() {
        eprintln!("{}", umayc::render_all(&out.diagnostics, &map, false));
    }
    out.diagnostics
}

#[track_caller]
fn ok(src: &str) {
    assert!(!analyze(src).has_errors(), "expected no type errors\n---\n{src}");
}

/// The real `ipc.um` text followed by the program that uses it.
fn spliced(body: &str) -> String {
    format!("{IPC_UM}\n{body}")
}

/// `umay check` on the spliced source in a temp directory.
fn cli_check(name: &str, src: &str) -> bool {
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, src).expect("write temp source");
    let out = Command::new(compiler())
        .arg("check")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay check");
    let passed = out.status.success();
    if !passed {
        eprintln!(
            "umay check failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    passed
}

/// Builds and runs the spliced source, returning trimmed stdout. `None` means
/// the host has no assembler/linker, in which case a test returns early.
fn run(name: &str, src: &str) -> Option<String> {
    if cfg!(windows) || !toolchain_available() {
        eprintln!("skipping runtime check: no `as` or `cc`");
        return None;
    }
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, src).expect("write temp source");
    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        build.status.success(),
        "build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = dir.join("dist").join(if cfg!(windows) { "main.exe" } else { "main" });
    let out = Command::new(&exe).current_dir(&dir).output().expect("run built program");
    assert!(out.status.success(), "run failed: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    Some(stdout)
}

// (1) The file type-checks when spliced with a trivial main.
#[test]
fn ipc_spliced_typechecks_in_process() {
    ok(&spliced("fn main() {}"));
}

#[test]
fn cli_check_accepts_spliced_ipc() {
    assert!(cli_check("check", &spliced("fn main() {}")));
}

// (2) Decode a request and print id/method/arg -> "7 3 42" (one per line).
const DECODE_SRC: &str = r#"
fn main() -> i32 {
    let b = Bytes::from_str("{{\"id\":7,\"method\":3,\"arg\":42}}")
    let req = ipc_decode_request(b.data, b.len)
    println_i64(req.id)
    println_i64(req.method)
    println_i64(req.arg)
    return 0
}
"#;

#[test]
fn decode_request_reads_id_method_arg() {
    let Some(stdout) = run("decode", &spliced(DECODE_SRC)) else { return };
    assert_eq!(stdout, "7\n3\n42");
}

// (3) Encode a reply and print the NUL-terminated buffer as a string.
const ENCODE_SRC: &str = r#"
fn main() -> i32 {
    let out = alloc(256usize)
    let n = ipc_encode_reply(7i64, true, 42i64, out)
    println(out as *const u8)
    dealloc(out)
    return 0
}
"#;

#[test]
fn encode_reply_exact_json() {
    let Some(stdout) = run("encode", &spliced(ENCODE_SRC)) else { return };
    assert_eq!(stdout, r#"{"id":7,"ok":true,"result":42}"#);
}

// (4) Empty and oversized input return the all--1 sentinel, no panic. The
// oversized call passes a short buffer with a huge len: ipc_valid_size rejects
// it before any byte is read, so there is no out-of-bounds access.
const SENTINEL_SRC: &str = r#"
fn main() -> i32 {
    let b = Bytes::from_str("{{\"id\":7,\"method\":3,\"arg\":42}}")
    let zero = ipc_decode_request(b.data, 0usize)
    println_i64(zero.id)
    println_i64(zero.method)
    println_i64(zero.arg)
    let big = ipc_decode_request(b.data, 1048577usize)
    println_i64(big.id)
    println_i64(big.method)
    println_i64(big.arg)
    return 0
}
"#;

#[test]
fn zero_and_oversized_return_minus_one_sentinels() {
    let Some(stdout) = run("sentinel", &spliced(SENTINEL_SRC)) else { return };
    assert_eq!(stdout, "-1\n-1\n-1\n-1\n-1\n-1");
}

// (5) The monotonic counter returns 1, 2, 3 from 0.
const NEXT_ID_SRC: &str = r#"
fn main() -> i32 {
    let mut id = 0i64
    println_i64(ipc_next_id(&mut id))
    println_i64(ipc_next_id(&mut id))
    println_i64(ipc_next_id(&mut id))
    return 0
}
"#;

#[test]
fn next_id_is_monotonic() {
    let Some(stdout) = run("next_id", &spliced(NEXT_ID_SRC)) else { return };
    assert_eq!(stdout, "1\n2\n3");
}

// (6) Unknown extra fields are skipped; the known ones still decode.
const EXTRA_SRC: &str = r#"
fn main() -> i32 {
    let b = Bytes::from_str("{{\"extra\":\"x\",\"id\":7,\"noise\":true,\"method\":3,\"arg\":42,\"more\":9}}")
    let req = ipc_decode_request(b.data, b.len)
    println_i64(req.id)
    println_i64(req.method)
    println_i64(req.arg)
    return 0
}
"#;

#[test]
fn unknown_fields_still_decode_known_fields() {
    let Some(stdout) = run("extra", &spliced(EXTRA_SRC)) else { return };
    assert_eq!(stdout, "7\n3\n42");
}

// Error replies and events: record the exact strings for the report. An error
// is ok:false + result:-1 (no `error` object); an event is integer id + arg.
const SHAPES_SRC: &str = r#"
fn main() -> i32 {
    let out = alloc(256usize)
    let n = ipc_encode_reply(9i64, false, 0i64, out)
    println(out as *const u8)
    let m = ipc_encode_event(4i64, 55i64, out)
    println(out as *const u8)
    dealloc(out)
    return 0
}
"#;

#[test]
fn error_reply_and_event_exact_json() {
    let Some(stdout) = run("shapes", &spliced(SHAPES_SRC)) else { return };
    assert_eq!(stdout, "{\"id\":9,\"ok\":false,\"result\":-1}\n{\"event\":4,\"arg\":55}");
}
