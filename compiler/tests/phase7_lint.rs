//! `umay lint`, held against the real standard library.
//!
//! The lesson these tests were written from: a lint that walks the syntax tree
//! can look right and miss most of the program. The first version of `L0105`
//! reported 180 parameters as unused because it never looked at a block's
//! trailing expression, at an `unsafe` block's value, or at a match arm's
//! pattern. Only a pass over a real corpus showed that.
//!
//! So there are two kinds of test here: each rule has to fire on a case built to
//! trigger it, and the whole standard library has to come out clean. The first
//! catches a rule that never fires; the second catches one that fires on code a
//! reader would call correct.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate sits inside the repository")
        .to_path_buf()
}

fn corpus() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["stdlib", "examples"] {
        let Ok(entries) = std::fs::read_dir(repo_root().join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "um") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The codes a lint pass reports, in sorted order.
fn codes(src: &str) -> Vec<String> {
    let (diags, _map) = umayc::lint_source("t.um", src);
    let mut out: Vec<String> = diags.items().iter().map(|d| d.code.to_string()).collect();
    out.sort();
    out
}

#[test]
fn the_standard_library_has_no_lint_findings() {
    let files = corpus();
    assert!(!files.is_empty(), "the corpus should not be empty");
    let mut dirty = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).expect("corpus file should be readable");
        let (diags, _map) = umayc::lint_source(file.to_string_lossy().to_string(), source);
        if !diags.items().is_empty() {
            let listed: Vec<&str> = diags.items().iter().map(|d| d.code).collect();
            dirty.push(format!("{}: {}", file.display(), listed.join(", ")));
        }
    }
    assert!(
        dirty.is_empty(),
        "these files have lint findings:\n  {}",
        dirty.join("\n  ")
    );
}

#[test]
fn trailing_whitespace_is_reported() {
    assert!(codes("fn f() -> i64 {\n    return 1i64   \n}\n").contains(&"L0101".to_string()));
    // A line with no trailing space is not reported.
    assert!(!codes("fn f() -> i64 {\n    return 1i64\n}\n").contains(&"L0101".to_string()));
}

#[test]
fn only_upper_case_markers_are_reported() {
    // The marker is a convention, and prose that happens to use the word is not
    // one. Reporting it would make the rule unusable.
    assert!(codes("// TODO: later\nfn f() -> i64 {\n    return 1i64\n}\n").contains(&"L0102".to_string()));
    assert!(!codes("// todo list of things\nfn f() -> i64 {\n    return 1i64\n}\n")
        .contains(&"L0102".to_string()));
}

#[test]
fn an_empty_body_that_promises_a_value_is_reported() {
    assert!(codes("fn f() -> i64 {\n}\n").contains(&"L0103".to_string()));
    // A body that returns nothing is fine.
    assert!(!codes("fn f() {\n}\n").contains(&"L0103".to_string()));
    // A body whose value is its trailing expression is not empty.
    assert!(!codes("fn f() -> i64 {\n    return 1i64\n}\n").contains(&"L0103".to_string()));
}

#[test]
fn an_else_after_a_returning_branch_is_reported() {
    let src = "fn f(c: bool) -> i64 {\n    if c {\n        return 1i64\n    } else {\n        return 2i64\n    }\n}\n";
    assert!(codes(src).contains(&"L0104".to_string()));
    // A branch that may fall through is doing real work, so the `else` stays.
    let falls_through = "fn f(c: bool) -> i64 {\n    if c {\n        g()\n    } else {\n        return 2i64\n    }\n}\n";
    assert!(!codes(falls_through).contains(&"L0104".to_string()));
}

#[test]
fn an_unused_parameter_is_reported_however_it_is_reached() {
    // Each of these reaches the parameter through a different corner of the
    // syntax tree: a struct literal's shorthand, an `unsafe` block's value, a
    // match arm's pattern, a nested `if` inside a `while`, and a block's tail.
    let cases = [
        "fn f(p: i64) -> i64 {\n    return p\n}\n",
        "struct S {\n    a: i64,\n}\nfn f(p: i64) -> S {\n    return S { a: p }\n}\n",
        "fn g(x: i64) -> i64 {\n    return x\n}\nfn f(p: i64) -> i64 {\n    unsafe {\n        g(p)\n    }\n}\n",
        "fn f(p: i64) -> i64 {\n    match p {\n        0i64 => 1i64,\n        n => n,\n    }\n}\n",
        "fn f(p: i64) -> i64 {\n    let mut i = 0i64\n    while i < 3i64 {\n        if p == 1i64 {\n            return 1i64\n        }\n        i = i + 1i64\n    }\n    return 0i64\n}\n",
        "fn f(p: i64) -> i64 {\n    let b = {\n        p\n    }\n    return b\n}\n",
    ];
    for (i, src) in cases.iter().enumerate() {
        assert!(
            !codes(src).contains(&"L0105".to_string()),
            "case {i} reported a used parameter as unused"
        );
    }
    // And the rule still fires when the parameter really is ignored.
    assert!(codes("fn f(p: i64) -> i64 {\n    return 0i64\n}\n").contains(&"L0105".to_string()));
    // A leading underscore is how a reader says "kept on purpose".
    assert!(!codes("fn f(_p: i64) -> i64 {\n    return 0i64\n}\n").contains(&"L0105".to_string()));
}

#[test]
fn a_finding_points_at_the_line_it_is_about() {
    // The line number is computed from the byte offset by the source map, and an
    // off-by-one there sends the reader to the wrong line. It did once: the
    // marker on line 1 was reported as line 2.
    let src = "// TODO: later\nfn f() -> i64 {\n    return 1i64\n}\n";
    let (diags, map) = umayc::lint_source("t.um", src);
    let marker = diags
        .items()
        .iter()
        .find(|d| d.code == "L0102")
        .expect("the marker should be reported");
    let span = marker.span.expect("the finding should carry a span");
    let file = map.file(span.file);
    let at = file.line_col(span.start);
    assert_eq!((at.line, at.column), (1, 1));
}

#[test]
fn every_documented_rule_can_be_looked_up() {
    for rule in umayc::lint::RULES {
        let found = umayc::lint::rule(rule.code).expect("rule should be findable by its code");
        assert_eq!(found.name, rule.name);
    }
    assert!(umayc::lint::rule("L9999").is_none());
}

#[test]
fn a_file_that_does_not_parse_reports_the_parse_error() {
    let (diags, _map) = umayc::lint_source("t.um", "fn f( {\n");
    assert!(
        diags.has_errors(),
        "a file that does not parse should report errors, not findings"
    );
}

#[test]
fn a_parameter_used_in_an_if_blocks_last_expression_is_used() {
    let src = "fn f(a: i64, b: i64) -> i64 {\n    let mut r = 0\n    if a > 0 {\n        r = b\n    }\n    return r\n}\n";
    assert!(!codes(src).iter().any(|c| c == "L0105"), "{:?}", codes(src));
}
