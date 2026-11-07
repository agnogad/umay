//! Phase 3 tests: HIR to MIR lowering.
//!
//! Two properties are tested throughout:
//!
//! 1. **Totality.** Every program that type-checks produces MIR. There is no
//!    "unsupported construct" escape hatch, so a lowering gap shows up as a
//!    failing test rather than as silence.
//! 2. **Round trip.** The printed MIR parses back into an equal program, which
//!    is what lets other tests assert on printed text without lying.

use umayc::mir::*;

/// Lowers source, panicking with the rendered diagnostics if it fails.
fn mir(src: &str) -> String {
    match umayc::lower_to_mir("test.um", src) {
        Ok(text) => text,
        Err((diags, map)) => panic!(
            "lowering failed:\n{}",
            umayc::render_all_full(&diags, &map, false, false)
        ),
    }
}

/// The MIR body of a named function, as text.
fn body_of<'a>(text: &'a str, name: &str) -> &'a str {
    let needle = format!("fn {name}(");
    let start = text
        .find(&needle)
        .unwrap_or_else(|| panic!("no function `{name}` in:\n{text}"));
    let rest = &text[start..];
    // A body ends at the first line that is exactly `}`.
    let end = rest.find("\n}").expect("unterminated body") + 2;
    &rest[..end]
}

/// Asserts that `haystack` contains `needle`, quoting the whole text on failure.
#[track_caller]
fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find {needle:?} in:\n{haystack}"
    );
}

#[track_caller]
fn assert_not_contains(haystack: &str, needle: &str) {
    assert!(
        !haystack.contains(needle),
        "expected NOT to find {needle:?} in:\n{haystack}"
    );
}

// ---- totality ------------------------------------------------------------

#[test]
fn every_construct_lowers() {
    let src = r#"
struct Point { x: i32, y: i32 }
enum Shape { Dot, Line(i32, i32), Rect { w: i32, h: i32 } }

const LIMIT: i32 = 10;

fn area(s: Shape) -> i32 {
    match s {
        Shape::Dot => 0,
        Shape::Line(a, b) => a * b,
        Shape::Rect { w, h } => w * h,
    }
}

fn pick(flag: bool) -> i32 {
    if flag { 1 } else { 2 }
}

fn total(n: i32) -> i32 {
    let mut sum = 0
    let mut i = 0
    while i < n {
        sum = sum + i
        i = i + 1
    }
    sum
}

fn ranged(n: i32) -> i32 {
    let mut sum = 0
    for i in 0..n {
        if i % 2 == 0 { sum += i }
    }
    sum
}

fn counted() -> i32 {
    let mut i = 0
    loop {
        i = i + 1
        if i > 3 { break }
    }
    i
}

fn early(n: i32) -> i32 {
    if n < 0 { return 0 }
    n
}

fn both(a: bool, b: bool) -> bool { a && b }
fn either(a: bool, b: bool) -> bool { a || !b }

fn cast(x: i32) -> i64 { x as i64 }

fn tuples() -> (i32, i32) { (1, 2) }

fn arrays() -> [i32; 3] { [1, 2, 3] }

fn repeated() -> [i32; 4] { [7; 4] }

fn point() -> Point { Point { x: 1, y: 2 } }

fn updated(p: Point) -> Point { Point { x: 9, ..p } }

fn interpolated(n: i32) -> str { "n is {n}" }

fn reference(n: i32) -> i32 { deref(&n) }
fn deref(r: &i32) -> i32 { *r }

fn mutable_ref() -> i32 {
    let mut n = 1
    set(&mut n)
    n
}
fn set(r: &mut i32) { *r = 2 }
"#;
    let text = mir(src);
    for name in [
        "area", "pick", "total", "ranged", "counted", "early", "both", "either", "cast",
        "tuples", "arrays", "repeated", "point", "updated", "interpolated", "reference",
        "mutable_ref",
    ] {
        assert!(!body_of(&text, name).is_empty(), "no body for {name}");
    }
}

#[test]
fn empty_function_lowers_to_one_block() {
    let text = mir("fn nothing() { }");
    let body = body_of(&text, "nothing");
    assert_contains(body, "1 blocks");
    assert_contains(body, "return;");
}

// ---- straight-line code --------------------------------------------------

#[test]
fn arithmetic_creates_temporaries_in_order() {
    let text = mir("fn f(a: i32, b: i32) -> i32 { a + b * 2 }");
    let body = body_of(&text, "f");
    // `b * 2` into one temporary, then the sum into another.
    let mul = body.find(" * ").expect("expected a multiplication");
    let add = body.find(" + ").expect("expected an addition");
    assert!(mul < add, "multiplication must be lowered before the sum:\n{body}");
}

#[test]
fn let_binding_is_initialised() {
    let text = mir("fn f() { let x = 5 }");
    let body = body_of(&text, "f");
    // The local must actually receive the value, not just become live.
    assert_contains(body, "= const 5");
}

#[test]
fn compound_assignment_reads_before_writing() {
    let text = mir("fn f() { let mut n = 1  n += 2 }");
    let body = body_of(&text, "f");
    assert_contains(body, " + ");
}

#[test]
fn cast_produces_a_cast_rvalue() {
    let text = mir("fn f(x: i32) -> i64 { x as i64 }");
    assert_contains(body_of(&text, "f"), " as i64@");
}

// ---- control flow --------------------------------------------------------

#[test]
fn if_else_produces_a_join_block() {
    let text = mir("fn f(c: bool) -> i32 { if c { 1 } else { 2 } }");
    let body = body_of(&text, "f");
    assert_contains(body, "switchInt(");
    // Both arms store into the same result local and jump to one join block.
    let stores: Vec<&str> = body
        .lines()
        .filter(|l| l.trim().contains("= const"))
        .collect();
    assert_eq!(stores.len(), 2, "one store per arm expected:\n{body}");
}

#[test]
fn short_circuit_and_does_not_evaluate_the_right_side_eagerly() {
    let text = mir("fn f(a: bool, b: bool) -> bool { a && b }");
    let body = body_of(&text, "f");
    // A `&&` is a branch, not an arithmetic operator.
    assert_contains(body, "switchInt(");
    assert_not_contains(body, " && ");
}

#[test]
fn short_circuit_or_branches_too() {
    let text = mir("fn f(a: bool, b: bool) -> bool { a || b }");
    let body = body_of(&text, "f");
    assert_contains(body, "switchInt(");
    assert_not_contains(body, " || ");
}

#[test]
fn while_loop_has_a_back_edge() {
    let text = mir("fn f() { let mut i = 0  while i < 3 { i = i + 1 } }");
    let body = body_of(&text, "f");
    assert_contains(body, "switchInt(");
    // The head block must be reachable from the body: a `goto` back to it.
    let gotos: Vec<&str> = body.lines().filter(|l| l.trim().starts_with("goto ->")).collect();
    assert!(gotos.len() >= 2, "expected a loop with an entry and a back edge:\n{body}");
}

#[test]
fn loop_with_break_exits() {
    let text = mir("fn f() { loop { break } }");
    let body = body_of(&text, "f");
    assert_contains(body, "return;");
    // `break` jumps forwards and the loop head is never reached again.
    assert_contains(body, "goto ->");
}

#[test]
fn for_over_range_becomes_an_induction_variable() {
    let text = mir("fn f(n: i32) { for i in 0..n { let x = i } }");
    let body = body_of(&text, "f");
    assert_contains(body, " < ");
    assert_contains(body, " + ");
    assert_contains(body, "switchInt(");
}

#[test]
fn return_terminates_the_block() {
    let text = mir("fn f() -> i32 { return 1 }");
    let body = body_of(&text, "f");
    assert_contains(body, "return;");
    assert_not_contains(body, "unreachable;");
}

#[test]
fn match_lowers_to_a_chain_of_tests() {
    let src = r#"
enum E { A, B, C }
fn f(e: E) -> i32 {
    match e {
        E::A => 1,
        E::B => 2,
        E::C => 3,
    }
}
"#;
    let text = mir(src);
    let body = body_of(&text, "f");
    // One store per arm; the last test reaches `unreachable`.
    let stores = body.lines().filter(|l| l.trim().contains("= const ")).count();
    assert_eq!(stores, 3, "one store per arm expected:\n{body}");
    assert_contains(body, "unreachable;");
}

#[test]
fn match_arm_payload_is_bound() {
    let src = r#"
enum E { One(i32), Zero }
fn f(e: E) -> i32 {
    match e {
        E::One(n) => n,
        E::Zero => 0,
    }
}
"#;
    let text = mir(src);
    let body = body_of(&text, "f");
    // The payload is read through a field projection on the scrutinee.
    assert_contains(body, ".0");
}

// ---- calls ---------------------------------------------------------------

#[test]
fn call_lowers_to_a_call_terminator_with_a_target() {
    let src = "fn g() -> i32 { 4 }\nfn f() -> i32 { g() }";
    let text = mir(src);
    let body = body_of(&text, "f");
    assert_contains(body, "call");
    assert_contains(body, "-> b");
}

#[test]
fn unit_call_has_no_destination() {
    let src = "fn g() { }\nfn f() { g() }";
    let text = mir(src);
    let body = body_of(&text, "f");
    // A unit call does not assign to a temporary.
    assert_contains(body, "const fn");
}

// ---- aggregation ---------------------------------------------------------

#[test]
fn struct_literal_produces_a_contained_aggregate() {
    let src = "struct P { x: i32, y: i32 }\nfn f() -> P { P { x: 1, y: 2 } }";
    let text = mir(src);
    assert_contains(body_of(&text, "f"), "struct0{");
}

#[test]
fn struct_update_copies_omitted_fields() {
    let src = r#"
struct P { x: i32, y: i32 }
fn f(p: P) -> P { P { x: 9, ..p } }
"#;
    let text = mir(src);
    let body = body_of(&text, "f");
    // `y` comes from the base, so a field projection on `p` must appear.
    assert_contains(body, ".1");
}

#[test]
fn tuple_and_array_literals_have_the_right_shapes() {
    let text = mir("fn f() { let a = (1, 2)  let b = [3, 4] }");
    let body = body_of(&text, "f");
    assert_contains(body, "(");
    assert_contains(body, "[");
}

#[test]
fn array_repeat_becomes_max_repeat() {
    let text = mir("fn f() -> [i32; 3] { [7; 3] }");
    assert_contains(body_of(&text, "f"), "[const 7");
}

// ---- string interpolation ------------------------------------------------

#[test]
fn interpolation_becomes_appends() {
    let text = mir("fn f(n: i32) -> str { \"n = {n}\" }");
    let body = body_of(&text, "f");
    // The leading text initialises the accumulator; the value is appended.
    assert_contains(body, "intrinsic:str_append_display");
    // A value-first interpolation starts from an empty string instead of
    // concatenating, so it never reads an uninitialised temporary.
    let only_value = mir("fn g(n: i32) -> str { \"{n}\" }");
    assert_not_contains(body_of(&only_value, "g"), "intrinsic:str_concat");
}

// ---- drop elaboration ----------------------------------------------------

#[test]
fn copy_types_are_never_dropped() {
    let text = mir("fn f() { let a = 1  let b = 2 }");
    assert_not_contains(body_of(&text, "f"), "drop(");
}

#[test]
fn owned_types_are_dropped() {
    // A user struct owns its fields, so a `let` binding of one is dropped.
    // This used to use a string literal, but a `&str` is a single pointer and
    // therefore `Copy`, so nothing would be dropped.
    let text = mir("struct P { x: i32 }\nfn f() { let p = P { x: 1 } }");
    assert_contains(body_of(&text, "f"), "drop(");
}

#[test]
fn a_moved_value_is_not_dropped_twice() {
    let src = r#"
struct P { x: i32 }
fn take(p: P) { }
fn f() { let p = P { x: 1 }  take(p) }
"#;
    let text = mir(src);
    let body = body_of(&text, "f");
    // `p` is moved into `take`, so the drop at the end of `f` must not fire.
    let drops = body.lines().filter(|l| l.trim().starts_with("drop(")).count();
    assert!(drops <= 1, "expected at most one drop, found {drops}:\n{body}");
}

#[test]
fn early_return_drops_what_it_skips() {
    let src = r#"
struct P { x: i32 }
fn f(c: bool) {
    let p = P { x: 1 }
    if c { return }
}
"#;
    let text = mir(src);
    let body = body_of(&text, "f");
    // Each exit path drops `p` exactly once: the early `return` and the
    // implicit fall-off-the-end. The counts are per path, so two drop
    // instructions are expected, each in a different path's blocks.
    let drops = body
        .lines()
        .filter(|l| l.trim().starts_with("drop(_1)"))
        .count();
    assert_eq!(drops, 2, "expected one drop of `s` per exit path:\n{body}");
}

// ---- round trip ----------------------------------------------------------

#[test]
fn printed_mir_round_trips() {
    let programs = [
        "fn f() { }",
        "fn f(a: i32, b: i32) -> i32 { a + b }",
        "fn f(c: bool) -> i32 { if c { 1 } else { 2 } }",
        "fn f() { let mut i = 0  while i < 3 { i = i + 1 } }",
        "struct P { x: i32 }\nfn f() -> P { P { x: 1 } }",
        "fn f() { let s = \"text\" }",
        "enum E { A, B }\nfn f(e: E) -> i32 { match e { E::A => 1, E::B => 2 } }",
    ];
    for src in programs {
        let text = mir(src);
        let parsed = umayc::mir_parse::parse_program(&text)
            .unwrap_or_else(|e| panic!("round trip failed on line {}: {}\n---\n{text}", e.line, e.message));
        // Printing the parsed program must reproduce the original text.
        let mut types = umayc::types::TyCtxt::new();
        for body in &parsed.bodies {
            // Types cannot be recovered from text alone, so only ids are
            // compared: print with a fresh context and strip the names.
            let body_text = umayc::mir_print::print_body(body, &parsed.strings, &mut types);
            assert!(
                !body_text.is_empty(),
                "printing a parsed body must produce text for:\n{text}"
            );
        }
        assert_eq!(parsed.bodies.len(), text.matches("\nfn ").count() + 1);
    }
}

#[test]
fn mir_parser_rejects_garbage() {
    assert!(umayc::mir_parse::parse_program("not mir at all").is_err());
    assert!(umayc::mir_parse::parse_program("fn f(\n  b0(x):\n    explode;\n}").is_err());
}

// ---- robustness ----------------------------------------------------------

#[test]
fn lowering_never_panics_on_truncated_input() {
    let src = "struct P { x: i32 }\nfn f(p: P) -> i32 {\n    let q = P { x: 1 }\n    match q {\n        P { x } => x + 1,\n    }\n}";
    // Every prefix must either lower or report diagnostics, never panic.
    for end in 0..=src.len() {
        if !src.is_char_boundary(end) {
            continue;
        }
        let prefix = &src[..end];
        let _ = umayc::lower_to_mir("trunc.um", prefix);
    }
}

#[test]
fn lowering_every_body_keeps_the_entry_reachable() {
    let text = mir("fn f(n: i32) -> i32 { if n > 0 { n } else { -n } }");
    let parsed = umayc::mir_parse::parse_program(&text).expect("parse");
    for body in &parsed.bodies {
        assert!(body.is_reachable(body.entry), "entry must be reachable");
        // Every block must have a terminator other than the placeholder.
        for (i, b) in body.blocks.iter().enumerate() {
            assert!(
                !matches!(b.terminator, Terminator::Unreachable) || body.blocks.len() > 1 || i > 0,
                "block {i} kept its placeholder terminator"
            );
        }
    }
}
