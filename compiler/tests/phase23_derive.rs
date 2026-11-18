//! Phase 2/3 slice: `#[derive(Copy)]` has a real semantic effect and macro
//! diagnostic codes are documented.
//!
//! A derived-`Copy` struct is copyable by value; the same struct without the
//! derive is moved. Attribute warnings stay honest: recognised derives are
//! silent, genuinely unknown attributes still warn.

use umayc::diagnostics::Severity;

fn analyze(src: &str) -> umayc::diagnostics::Diagnostics {
    umayc::analyze_source("test.um", src).0
}

fn codes(d: &umayc::diagnostics::Diagnostics) -> Vec<&'static str> {
    d.items().iter().map(|x| x.code).collect()
}

#[track_caller]
fn no_errors(src: &str) {
    let d = analyze(src);
    assert!(!d.has_errors(), "expected no errors, got {:?}", codes(&d));
}

// ---------------------------------------------------------------------------
// (a) derive(Copy) actually makes the ADT copyable
// ---------------------------------------------------------------------------

#[test]
fn derived_copy_struct_can_be_used_after_a_second_binding() {
    no_errors(
        r#"
#[derive(Copy)]
struct P { x: i32 }
fn take(p: P) { }
fn main() {
    let p = P { x: 1 }
    let q = p
    take(p)
    take(q)
}
"#,
    );
}

#[test]
fn struct_without_derive_is_moved() {
    let d = analyze(
        r#"
struct P { x: i32 }
fn take(p: P) { }
fn main() {
    let p = P { x: 1 }
    let q = p
    take(p)
    take(q)
}
"#,
    );
    assert!(
        codes(&d).contains(&"E0201"),
        "a non-Copy struct must be moved, got {:?}",
        codes(&d)
    );
}

// ---------------------------------------------------------------------------
// (b)/(c) attribute warnings
// ---------------------------------------------------------------------------

#[test]
fn derive_copy_does_not_warn() {
    let d = analyze(
        r#"
#[derive(Copy)]
struct P { x: i32 }
fn main() { }
"#,
    );
    assert!(
        !codes(&d).contains(&"W0010"),
        "derive(Copy) is implemented and must not warn, got {:?}",
        codes(&d)
    );
}

#[test]
fn unknown_attribute_still_warns() {
    let d = analyze(
        r#"
#[unknown]
fn f() { }
"#,
    );
    let found: Vec<_> = d.items().iter().filter(|x| x.code == "W0010").collect();
    assert_eq!(found.len(), 1, "expected exactly one W0010, got {:?}", codes(&d));
    assert_eq!(found[0].severity, Severity::Warning);
}

// ---------------------------------------------------------------------------
// (d) macro codes are documented
// ---------------------------------------------------------------------------

#[test]
fn macro_diagnostic_codes_have_explain_entries() {
    for code in ["E0125", "E0126", "E0127", "E0128", "E0129"] {
        assert!(
            umayc::explain::lookup(code).is_some(),
            "{code} must have an explain entry"
        );
    }
}
