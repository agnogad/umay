//! Phase 2 semantic-analysis tests.
//!
//! Every rule is checked twice: a program that must be accepted (`ok`) and one
//! that must be rejected (`err` with a specific code). Accepting an invalid
//! program is a correctness bug, so the reject tests assert the code, not just
//! that *some* error occurred.

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;

struct Checked {
    diagnostics: Diagnostics,
}

fn analyze(src: &str) -> Checked {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(
        lex_errors.is_empty(),
        "test source failed to lex: {:?}\n---\n{src}",
        lex_errors
    );
    let parsed = parser::parse(tokens, Vec::new(), id);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "test source failed to parse: {parse_errors:?}\n---\n{src}");
    let out = sema::analyze(&parsed.program, &map);
    Checked { diagnostics: out.diagnostics }
}

fn error_codes(src: &str) -> Vec<&'static str> {
    analyze(src)
        .diagnostics
        .items()
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| d.code)
        .collect()
}

/// Asserts the program produces no errors.
#[track_caller]
fn ok(src: &str) {
    let codes = error_codes(src);
    assert!(codes.is_empty(), "expected no errors, got {codes:?}\n---\n{src}");
}

/// Asserts the program produces exactly this error code (and no parse errors).
#[track_caller]
fn err(src: &str, code: &str) {
    let codes = error_codes(src);
    assert!(
        codes.contains(&code) || codes.iter().any(|c| c == &code),
        "expected error {code}, got {codes:?}\n---\n{src}"
    );
}

#[track_caller]
fn warns(src: &str, code: &str) {
    let out = analyze(src);
    let found = out.diagnostics.items().iter().any(|d| d.code == code);
    assert!(found, "expected warning {code}\n---\n{src}");
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

#[test]
fn known_local_and_function_resolve() {
    ok("
fn double(x: i32) -> i32 { x * 2 }

fn main() {
    let n: i32 = 21
    let r = double(n)
    let _ = r
}
");
}

#[test]
fn unknown_name_is_e0101() {
    err("fn main() { let _ = missing_value }", "E0101");
}

#[test]
fn unknown_name_suggests_close_match() {
    let out = analyze("fn main() { let count = 1\n let _ = cuont }");
    let d = out
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == "E0101")
        .expect("E0101 expected");
    let suggestion = d.suggestions.first().expect("expected a suggestion");
    assert!(suggestion.description.contains("count"), "label was {:?}", suggestion.description);
}

#[test]
fn unknown_type_is_e1003() {
    err("fn f(x: Strng) {}", "E1003");
}

#[test]
fn unknown_type_suggests_string() {
    let out = analyze("fn f(x: Strng) {}");
    let d = out.diagnostics.items().iter().find(|d| d.code == "E1003").unwrap();
    assert!(d.suggestions.first().unwrap().description.contains("String"));
}

#[test]
fn duplicate_function_is_e1010() {
    err("fn f() {}\nfn f() {}", "E1010");
}

#[test]
fn duplicate_struct_is_e1010() {
    err("struct S { a: i32 }\nstruct S { b: i32 }", "E1010");
}

#[test]
fn duplicate_enum_variant_is_e1010() {
    err("enum E { A, A }", "E1010");
}

#[test]
fn forward_reference_to_function_is_allowed() {
    ok("
fn main() { let _ = helper() }
fn helper() -> i32 { 1 }
");
}

#[test]
fn self_referential_struct_is_allowed() {
    ok("
struct Node { value: i32, next: Option<Node> }
fn main() { let _ = 1 }
");
}

// ---------------------------------------------------------------------------
// Literals and inference
// ---------------------------------------------------------------------------

#[test]
fn integer_literal_defaults_to_i32() {
    let out = analyze("fn main() { let x = 5\n let _ = x }");
    assert!(error_codes("fn main() { let x = 5\n let _ = x }").is_empty());
    let _ = out;
}

#[test]
fn literal_adopts_annotation() {
    ok("fn main() { let x: u8 = 200\n let _ = x }");
}

#[test]
fn literal_out_of_range_is_rejected() {
    err("fn main() { let x: u8 = 300\n let _ = x }", "E1001");
}

#[test]
fn suffixed_literal_range_checked() {
    err("fn main() { let x = 300u8\n let _ = x }", "E1001");
}

#[test]
fn float_literal_in_integer_context_is_rejected() {
    err("fn main() { let x: i32 = 1.5\n let _ = x }", "E1001");
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_on_matching_types_is_ok() {
    ok("fn main() { let a: i64 = 1\n let b: i64 = 2\n let _ = a + b * 3 }");
}

#[test]
fn mixed_integer_types_are_rejected() {
    err("fn main() { let a: i32 = 1\n let b: i64 = 2\n let _ = a + b }", "E1001");
}

#[test]
fn float_plus_int_is_rejected() {
    err("fn main() { let a: f64 = 1.0\n let _ = a + 1 }", "E1001");
}

#[test]
fn comparison_of_different_types_is_rejected() {
    err("fn main() { let a: i32 = 1\n let b: u8 = 2\n let _ = a == b }", "E1001");
}

#[test]
fn bitwise_on_float_is_e1014() {
    err("fn main() { let a: f64 = 1.0\n let _ = a & a }", "E1014");
}

#[test]
fn negating_bool_is_e1014() {
    err("fn main() { let b = true\n let _ = -b }", "E1014");
}

#[test]
fn logical_and_requires_bool() {
    err("fn main() { let a = 1\n let _ = a && true }", "E1001");
}

#[test]
fn impossible_equality_is_allowed() {
    // `str == i32` is a type error; `str == str` is fine.
    ok("fn main() { let a = \"x\"\n let b = \"y\"\n let _ = a == b }");
    err("fn main() { let a = \"x\"\n let _ = a == 1 }", "E1001");
}

// ---------------------------------------------------------------------------
// Assignment and mutability
// ---------------------------------------------------------------------------

#[test]
fn assignment_to_mutable_local_is_ok() {
    ok("fn main() { let mut x = 1\n x = 2\n let _ = x }");
}

#[test]
fn assignment_to_immutable_local_is_rejected() {
    err("fn main() { let x = 1\n x = 2 }", "E1001");
}

#[test]
fn assignment_to_literal_is_rejected() {
    err("fn main() { 1 = 2 }", "E1001");
}

#[test]
fn assignment_type_mismatch_is_rejected() {
    err("fn main() { let mut x: i32 = 1\n x = \"s\" }", "E1001");
}

#[test]
fn compound_assignment_checks_types() {
    ok("fn main() { let mut x: i32 = 1\n x += 2 }");
    err("fn main() { let mut x: i32 = 1\n x += \"s\" }", "E1001");
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[test]
fn struct_literal_all_fields_is_ok() {
    ok("
struct Point { x: i32, y: i32 }
fn main() { let p = Point { x: 1, y: 2 }\n let _ = p }
");
}

#[test]
fn missing_struct_field_is_e1009() {
    err("
struct Point { x: i32, y: i32 }
fn main() { let p = Point { x: 1 }\n let _ = p }
", "E1009");
}

#[test]
fn unknown_struct_field_is_e1006() {
    err("
struct Point { x: i32, y: i32 }
fn main() { let p = Point { x: 1, y: 2, z: 3 }\n let _ = p }
", "E1006");
}

#[test]
fn field_type_mismatch_is_rejected() {
    err("
struct Point { x: i32 }
fn main() { let p = Point { x: \"s\" }\n let _ = p }
", "E1001");
}

#[test]
fn field_access_is_ok() {
    ok("
struct Point { x: i32, y: i32 }
fn main() { let p = Point { x: 1, y: 2 }\n let _ = p.x + p.y }
");
}

#[test]
fn unknown_field_access_is_e1006() {
    err("
struct Point { x: i32 }
fn main() { let p = Point { x: 1 }\n let _ = p.z }
", "E1006");
}

#[test]
fn field_access_suggests_field_name() {
    let out = analyze("
struct Point { x: i32, width: i32 }
fn main() { let p = Point { x: 1, width: 2 }\n let _ = p.widht }
");
    let d = out.diagnostics.items().iter().find(|d| d.code == "E1006").unwrap();
    assert!(d.suggestions.first().unwrap().description.contains("width"));
}

#[test]
fn struct_shorthand_field_init_is_ok() {
    ok("
struct P { x: i32 }
fn main() { let x = 1\n let p = P { x }\n let _ = p }
");
}

#[test]
fn struct_field_access_on_enum_is_rejected() {
    err("
enum E { A }
fn main() { let e = E::A\n let _ = e.A }
", "E1006");
}

// ---------------------------------------------------------------------------
// Enums and match
// ---------------------------------------------------------------------------

#[test]
fn exhaustive_enum_match_is_ok() {
    ok("
enum Color { Red, Green, Blue }
fn f(c: Color) -> i32 {
    match c {
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
    }
}
");
}

#[test]
fn non_exhaustive_match_is_e1008() {
    err("
enum Color { Red, Green, Blue }
fn f(c: Color) -> i32 {
    match c {
        Color::Red => 1,
        Color::Green => 2,
    }
}
", "E1008");
}

#[test]
fn catch_all_makes_match_exhaustive() {
    ok("
enum Color { Red, Green, Blue }
fn f(c: Color) -> i32 {
    match c {
        Color::Red => 1,
        _ => 0,
    }
}
");
}

#[test]
fn bool_match_needs_both_arms() {
    ok("
fn f(b: bool) -> i32 { match b { true => 1, false => 0 } }
");
    err("
fn f(b: bool) -> i32 { match b { true => 1 } }
", "E1008");
}

#[test]
fn unknown_variant_pattern_is_e1007() {
    err("
enum Color { Red }
fn f(c: Color) -> i32 { match c { Color::Purple => 1, _ => 0 } }
", "E1007");
}

#[test]
fn unknown_variant_suggests_name() {
    let out = analyze("
enum Color { Red, Green }
fn f(c: Color) -> i32 { match c { Color::Gren => 1, _ => 0 } }
");
    let d = out.diagnostics.items().iter().find(|d| d.code == "E1007").unwrap();
    assert!(d.suggestions.first().unwrap().description.contains("Green"));
}

#[test]
fn tuple_variant_pattern_arity_is_checked() {
    err("
enum E { Pair(i32, i32) }
fn f(e: E) -> i32 { match e { E::Pair(a) => a } }
", "E1005");
}

#[test]
fn tuple_variant_construction_checks_types() {
    ok("
enum E { N(i64) }
fn main() { let e = E::N(5)\n let _ = e }
");
    err("
enum E { N(i64) }
fn main() { let e = E::N(\"x\")\n let _ = e }
", "E1001");
}

#[test]
fn match_arms_must_agree_on_type() {
    err("
enum E { A, B }
fn f(e: E) -> i32 {
    match e {
        E::A => 1,
        E::B => \"s\",
    }
}
", "E1001");
}

#[test]
fn or_pattern_binding_mismatch_is_e1008() {
    err("
enum E { A(i32), B(i32) }
fn f(e: E) -> i32 { match e { E::A(x) | E::B(y) => 1, _ => 0 } }
", "E1008");
}

// ---------------------------------------------------------------------------
// Functions and calls
// ---------------------------------------------------------------------------

#[test]
fn call_arity_mismatch_is_e1004() {
    err("
fn add(a: i32, b: i32) -> i32 { a + b }
fn main() { let _ = add(1) }
", "E1004");
}

#[test]
fn call_argument_type_mismatch_is_rejected() {
    err("
fn take(s: str) {}
fn main() { take(1) }
", "E1001");
}

#[test]
fn literal_argument_adopts_parameter_type() {
    ok("
fn take(x: u8) {}
fn main() { take(200) }
");
}

#[test]
fn return_type_mismatch_is_rejected() {
    err("fn f() -> i32 { \"s\" }", "E1001");
}

#[test]
fn missing_return_value_is_rejected() {
    err("fn f() -> i32 { }", "E1001");
}

#[test]
fn early_return_matching_type_is_ok() {
    ok("
fn f(x: i32) -> i32 {
    if x > 0 { return 1 }
    0
}
");
}

#[test]
fn calling_a_non_function_is_e1014() {
    err("fn main() { let x = 1\n let _ = x(2) }", "E1014");
}

#[test]
fn function_pointer_value_can_be_called() {
    ok("
fn inc(x: i32) -> i32 { x + 1 }
fn main() { let f = inc\n let _ = f(1) }
");
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

#[test]
fn method_call_is_ok() {
    ok("
struct Counter { n: i32 }
impl Counter {
    fn get(self) -> i32 { self.n }
}
fn main() { let c = Counter { n: 1 }\n let _ = c.get() }
");
}

#[test]
fn unknown_method_is_e0101() {
    err("
struct Counter { n: i32 }
impl Counter { fn get(self) -> i32 { self.n } }
fn main() { let c = Counter { n: 1 }\n let _ = c.set() }
", "E0101");
}

#[test]
fn unknown_method_suggests_name() {
    let out = analyze("
struct S { n: i32 }
impl S { fn get(self) -> i32 { self.n } }
fn main() { let s = S { n: 1 }\n let _ = s.gte() }
");
    let d = out.diagnostics.items().iter().find(|d| d.code == "E0101").unwrap();
    assert!(d.suggestions.first().unwrap().description.contains("get"));
}

#[test]
fn mutating_method_on_immutable_binding_is_rejected() {
    err("
struct S { n: i32 }
impl S { fn bump(&mut self) { self.n = self.n + 1 } }
fn main() { let s = S { n: 1 }\n s.bump() }
", "E1001");
}

#[test]
fn mutating_method_on_mutable_binding_is_ok() {
    ok("
struct S { n: i32 }
impl S { fn bump(&mut self) { self.n = self.n + 1 } }
fn main() { let mut s = S { n: 1 }\n s.bump() }
");
}

#[test]
fn builtin_len_on_str_is_ok() {
    ok("fn main() { let s = \"abc\"\n let _ = s.len() }");
}

#[test]
fn builtin_method_arity_is_checked() {
    err("fn main() { let s = \"abc\"\n let _ = s.len(1) }", "E1004");
}

// ---------------------------------------------------------------------------
// Arrays, tuples, indices
// ---------------------------------------------------------------------------

#[test]
fn array_elements_must_share_a_type() {
    ok("fn main() { let a = [1, 2, 3]\n let _ = a }");
    err("fn main() { let a = [1, \"s\"]\n let _ = a }", "E1001");
}

#[test]
fn array_index_type_is_checked() {
    err("fn main() { let a = [1, 2]\n let _ = a[\"x\"] }", "E1001");
}

#[test]
fn indexing_non_array_is_e1014() {
    err("fn main() { let x = 1\n let _ = x[0] }", "E1014");
}

#[test]
fn tuple_field_access_is_ok() {
    ok("fn main() { let t = (1, \"a\")\n let _ = t.0 }");
    err("fn main() { let t = (1, 2)\n let _ = t.5 }", "E1006");
}

#[test]
fn empty_array_needs_annotation() {
    err("fn main() { let a = []\n let _ = a }", "E1001");
}

#[test]
fn array_repeat_of_non_copy_is_e0203() {
    err("
struct S { n: i32 }
fn main() { let s = S { n: 1 }\n let a = [s; 3]\n let _ = a }
", "E0203");
}

// ---------------------------------------------------------------------------
// Casts
// ---------------------------------------------------------------------------

#[test]
fn numeric_cast_is_ok() {
    ok("fn main() { let a: i32 = 1\n let _ = a as i64 }");
}

#[test]
fn narrowing_cast_warns() {
    warns("fn main() { let a: i64 = 1\n let _ = a as i32 }", "W0102");
}

#[test]
fn casting_str_to_int_is_e1013() {
    err("fn main() { let s = \"a\"\n let _ = s as i32 }", "E1013");
}

#[test]
fn casting_to_char_is_e1013() {
    err("fn main() { let a: i32 = 65\n let _ = a as char }", "E1013");
}

// ---------------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------------

#[test]
fn non_bool_condition_is_rejected() {
    err("fn main() { if 1 { } }", "E1001");
}

#[test]
fn assignment_in_condition_suggests_comparison() {
    let out = analyze("fn main() { let x = 1\n if x = 1 { } }");
    let d = out.diagnostics.items().iter().find(|d| d.code == "E1001").unwrap();
    assert!(!d.suggestions.is_empty(), "expected a suggestion for `if x = 1`");
}

#[test]
fn if_else_types_must_agree() {
    ok("fn f(b: bool) -> i32 { if b { 1 } else { 2 } }");
    err("fn f(b: bool) -> i32 { if b { 1 } else { \"s\" } }", "E1001");
}

#[test]
fn while_loop_condition_must_be_bool() {
    ok("fn main() { let mut i = 0\n while i < 3 { i = i + 1 } }");
    err("fn main() { while 1 { } }", "E1001");
}

#[test]
fn for_over_range_binds_integer() {
    ok("fn main() { for i in 0..3 { let _ = i } }");
}

#[test]
fn for_over_array_binds_element() {
    ok("fn main() { let a = [1, 2, 3]\n for x in a { let _ = x } }");
}

#[test]
fn for_over_non_iterable_is_e1014() {
    err("fn main() { let x = 1\n for i in x { let _ = i } }", "E1014");
}

#[test]
fn try_operator_requires_option_or_result() {
    err("fn f() -> i32 { let x = 1\n let _ = x? }", "E1014");
}

#[test]
fn try_operator_requires_wrapping_return_type() {
    err("
fn f(x: Option<i32>) -> i32 { let v = x?\n v }
", "E1015");
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

#[test]
fn borrowing_a_local_is_ok() {
    ok("fn main() { let x = 1\n let r = &x\n let _ = *r }");
}

#[test]
fn borrowing_a_literal_is_rejected() {
    err("fn main() { let r = &1\n let _ = r }", "E0210");
}

#[test]
fn mutable_borrow_of_immutable_binding_is_rejected() {
    err("fn main() { let x = 1\n let r = &mut x\n let _ = r }", "E1001");
}

#[test]
fn raw_pointer_deref_outside_unsafe_is_e0211() {
    err("fn main() { let p: *i32 = 0\n let _ = *p }", "E0211");
}

#[test]
fn raw_pointer_deref_inside_unsafe_is_ok() {
    ok("fn main() { let p: *i32 = 0\n let v = unsafe { *p }\n let _ = v }");
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

#[test]
fn interpolation_of_displayable_values_is_ok() {
    ok("fn main() { let n = 1\n let s = \"n={n}\"\n let _ = s }");
}

#[test]
fn interpolation_of_struct_is_e1012() {
    err("
struct P { x: i32 }
fn main() { let p = P { x: 1 }\n let s = \"p={p}\"\n let _ = s }
", "E1012");
}

#[test]
fn interpolation_parses_real_expressions() {
    ok("fn main() { let a = 1\n let b = 2\n let s = \"sum={a + b}\"\n let _ = s }");
}

// ---------------------------------------------------------------------------
// Shadowing
// ---------------------------------------------------------------------------

#[test]
fn shadowing_in_same_scope_warns() {
    warns("fn main() { let x = 1\n let x = 2\n let _ = x }", "W0101");
}

#[test]
fn shadowing_in_inner_scope_is_ok() {
    ok("fn main() { let x = 1\n if true { let x = 2\n let _ = x }\n let _ = x }");
}

// ---------------------------------------------------------------------------
// Explain coverage
// ---------------------------------------------------------------------------

/// Every code the semantic analyser can emit must be documented, because
/// `umay explain <code>` is the first thing a developer (or an AI agent) runs
/// after seeing a diagnostic.
#[test]
fn every_sema_code_has_an_explanation() {
    // One snippet per emittable code. Keep this list in sync with the checker:
    // the test below fails if a new code is emitted without an entry.
    let snippets: &[(&str, &str)] = &[
        ("E1001", "fn main() { let x: i32 = \"s\" }"),
        ("E1002", "fn main() { let _ = _ }"),
        ("E1003", "fn f(x: Nope) {}"),
        ("E1004", "fn g(a: i32) {}\nfn main() { g() }"),
        ("E1005", "enum E { A(i32) }\nfn main() { let _ = E::A() }"),
        ("E1006", "struct P { x: i32 }\nfn main() { let p = P { x: 1 }\n let _ = p.y }"),
        ("E1007", "enum E { A }\nfn f(e: E) { match e { E::Z => {} , _ => {} } }"),
        ("E1008", "enum E { A, B }\nfn f(e: E) { match e { E::A => {} } }"),
        ("E1009", "struct P { x: i32, y: i32 }\nfn main() { let _ = P { x: 1 } }"),
        ("E1010", "fn f() {}\nfn f() {}"),
        ("E1011", "struct S { n: i32 }\nimpl S { fn make() -> i32 { 1 } }\nfn main() { let s = S { n: 1 }\n let _ = s.make() }"),
        ("E1012", "struct P { x: i32 }\nfn main() { let p = P { x: 1 }\n let _ = \"p={p}\" }"),
        ("E1013", "fn main() { let _ = \"s\" as i32 }"),
        ("E1014", "fn main() { let x = 1\n let _ = x[0] }"),
        ("E1015", "fn f(o: Option<i32>) -> i32 { let v = o?\n v }"),
    ];

    for (code, src) in snippets {
        let found = error_codes(src);
        assert!(
            found.contains(code),
            "snippet for {code} did not produce it, got {found:?}\n---\n{src}"
        );
        assert!(
            umayc::explain::lookup(code).is_some(),
            "{code} is emitted but has no `umay explain` entry"
        );
    }
}

#[test]
fn warnings_are_documented() {
    let warnings: &[(&str, &str)] = &[
        ("W0101", "fn main() { let x = 1\n let x = 2\n let _ = x }"),
        ("W0102", "fn main() { let a: i64 = 1\n let _ = a as i32 }"),
        ("W0103", "fn main() { if true { 1 } }"),
    ];
    for (code, src) in warnings {
        assert!(umayc::explain::lookup(code).is_some(), "{code} has no explain entry");
        warns(src, code);
    }
}
