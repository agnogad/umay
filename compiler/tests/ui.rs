//! Dependency-free compile-fail / UI snapshot harness.
//!
//! For every `tests/ui/<case>.um`, run the built compiler as
//! `umay check --color never <case>.um` with the working directory set to
//! `tests/ui/`, so source names and spans are stable relative paths. The
//! rendered diagnostics (stdout) plus the summary line (stderr) are compared
//! to `<case>.stderr`.
//!
//! NOTE: `umay check` writes rendered diagnostics to STDOUT and only the
//! `<file>: N error(s), M warning(s)` summary to STDERR. The snapshots are
//! named `.stderr` for documentation compatibility, but they capture
//! stdout followed by stderr. See `docs/v2/ci-matrix.md` §3.
//!
//! Set `BLESS=1` to (re)write every mismatching snapshot, then review the diff.
//! A bare mismatch fails the test and prints a small line diff.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("ui")
}

/// Strip trailing whitespace from each line only. Nothing else is normalised:
/// paths, spans, codes, blank lines and diagnostic text are compared verbatim.
fn normalize(text: &str) -> String {
    text.lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A compact, deterministic line diff (expected `-` / actual `+`).
fn line_diff(expected: &str, actual: &str) -> String {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    let mut out = String::new();
    for i in 0..e.len().max(a.len()) {
        match (e.get(i), a.get(i)) {
            (Some(x), Some(y)) if x == y => {}
            (Some(x), Some(y)) => {
                out.push_str(&format!("    {}: -{:?}\n        +{:?}\n", i + 1, x, y));
            }
            (Some(x), None) => out.push_str(&format!("    {}: -{:?}\n", i + 1, x)),
            (None, Some(y)) => out.push_str(&format!("    {}: +{:?}\n", i + 1, y)),
            (None, None) => {}
        }
    }
    out
}

#[test]
fn ui_snapshots() {
    let bin = env!("CARGO_BIN_EXE_umay");
    if !Path::new(bin).exists() {
        eprintln!("skipping UI snapshots: compiler binary unavailable at {bin}");
        return;
    }

    let dir = ui_dir();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read compiler/tests/ui")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("um"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no .um cases found in compiler/tests/ui");

    let bless = std::env::var_os("BLESS").map(|v| v != "0").unwrap_or(false);
    let mut failures: Vec<String> = Vec::new();

    for case in cases {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let output = Command::new(bin)
            .arg("check")
            .arg("--color")
            .arg("never")
            .arg(&name)
            .current_dir(&dir)
            .output()
            .unwrap_or_else(|e| panic!("run umay check for {name}: {e}"));

        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        let actual = normalize(&combined);

        let snapshot = case.with_extension("stderr");
        let expected = std::fs::read_to_string(&snapshot).unwrap_or_default();
        let expected = normalize(&expected);

        if actual == expected {
            continue;
        }

        if bless {
            std::fs::write(&snapshot, format!("{actual}\n"))
                .unwrap_or_else(|e| panic!("bless {}: {e}", snapshot.display()));
            eprintln!("blessed {name}");
            continue;
        }

        let mut msg = format!("UI snapshot mismatch for {name}:\n");
        msg.push_str(&line_diff(&expected, &actual));
        failures.push(msg);
    }

    assert!(
        failures.is_empty(),
        "{} UI snapshot(s) failed (set BLESS=1 to update):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
