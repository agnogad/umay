//! Phase 1 integration tests: lexer + parser behaviour on whole programs.
//!
//! These tests exercise the public frontend API the way the driver does, and
//! pin down the *stability contract* documented in `docs/diagnostics.md`:
//! error codes, ordering, JSON schema shape and exit-relevant error counts.

use umayc::{check_source, diagnostics::Severity, parse_source};

/// Asserts that `src` parses with no diagnostics at all.
fn assert_clean(src: &str) {
    let (diags, map) = check_source("test.um", src);
    let rendered = umayc::render_all(&diags, &map, false);
    assert!(diags.is_empty(), "expected a clean parse, got:\n{rendered}");
}

/// Asserts the exact sequence of diagnostic codes produced for `src`.
fn assert_codes(src: &str, expected: &[&str]) {
    let (diags, map) = check_source("test.um", src);
    let actual: Vec<&str> = diags.items().iter().map(|d| d.code).collect();
    let rendered = umayc::render_all(&diags, &map, false);
    assert_eq!(actual, expected, "unexpected diagnostics:\n{rendered}");
}

/// Asserts that parsing fails and reports at least one error.
fn assert_rejects(src: &str) {
    let (diags, map) = check_source("test.um", src);
    assert!(diags.has_errors(), "expected at least one error for:\n{src}");
    let rendered = umayc::render_all(&diags, &map, false);
    assert!(!rendered.is_empty());
}

// ---------------------------------------------------------------------------
// Accepted programs
// ---------------------------------------------------------------------------

#[test]
fn spec_intro_example_parses() {
    assert_clean(
        r#"
fn main() {
    let name = "Umay"
    let mut version = 1

    print("Hello, {name}")

    version += 1

    print("Version: {version}")
}
"#,
    );
}

#[test]
fn struct_and_enum_declarations_parse() {
    assert_clean(
        r#"
struct User {
    id: u64,
    name: String,
    active: bool
}

enum Status {
    Pending,
    Active,
    Disabled
}
"#,
    );
}

#[test]
fn function_with_return_parses() {
    assert_clean(
        r#"
fn add(a: i32, b: i32) -> i32 {
    return a + b
}
"#,
    );
}

#[test]
fn generic_struct_and_enum_parse() {
    assert_clean(
        r#"
struct Pair<A, B> {
    first: A,
    second: B
}

enum Option<T> {
    None,
    Some(T)
}

enum Result<T, E> {
    Ok(T),
    Err(E)
}
"#,
    );
}

#[test]
fn trait_and_impl_parse() {
    assert_clean(
        r#"
trait Display {
    fn display(&self) -> String
}

struct Point {
    x: i32,
    y: i32
}

impl Display for Point {
    fn display(&self) -> String {
        return "point"
    }
}
"#,
    );
}

#[test]
fn semicolons_are_optional() {
    assert_clean(
        r#"
fn main() {
    let a = 1
    let b = 2;
    let c = a + b
}
"#,
    );
}

#[test]
fn multiline_expression_continues_after_operator() {
    assert_clean(
        r#"
fn main() {
    let total = 1 +
        2 +
        3
}
"#,
    );
}

#[test]
fn postfix_on_next_line_continues_expression() {
    assert_clean(
        r#"
fn main() {
    let x = value
        .method()
        .other()
}
"#,
    );
}

#[test]
fn two_statements_on_one_line() {
    assert_clean(
        r#"
fn main() {
    let a = 1 let b = 2
    print("{}")
}
"#,
    );
}

#[test]
fn operators_and_precedence_parse() {
    assert_clean(
        r#"
fn main() {
    let a = 1 + 2 * 3
    let b = (1 + 2) * 3
    let c = a == b && a != b || false
    let d = a << 2 | b >> 1 & 0xFF ^ 1
    let e = -a
    let f = !true
}
"#,
    );
}

#[test]
fn compound_assignment_parse() {
    assert_clean(
        r#"
fn main() {
    let mut x = 1
    x += 1
    x -= 1
    x *= 2
    x /= 2
    x %= 2
    x &= 1
    x |= 1
    x ^= 1
    x <<= 1
    x >>= 1
}
"#,
    );
}

#[test]
fn references_and_moves_parse() {
    assert_clean(
        r#"
fn main() {
    let name = String.from("Umay")
    let first = &name
    let second = &name

    let mut count = 10
    let reference = &mut count
    *reference = 20

    let moved = move name
}
"#,
    );
}

#[test]
fn control_flow_parses() {
    assert_clean(
        r#"
fn main() {
    if true {
        print("yes")
    } else if false {
        print("no")
    } else {
        print("maybe")
    }

    while true {
        break
    }

    loop {
        continue
    }

    for i in 0..10 {
        print("{i}")
    }

    for item in list {
        print(item)
    }
}
"#,
    );
}

#[test]
fn match_with_patterns_parses() {
    assert_clean(
        r#"
fn classify(value: Status) -> str {
    match value {
        Status::Pending => "waiting",
        Status::Active => "running",
        Status::Disabled => "off"
    }
}

fn deeper(pair: (i32, i32)) -> i32 {
    match pair {
        (0, 0) => 0,
        (x, 0) => x,
        (0, y) => y,
        (x, y) if x == y => 1,
        (x, y) => x + y
    }
}
"#,
    );
}

#[test]
fn struct_literals_and_field_access_parse() {
    assert_clean(
        r#"
fn main() {
    let user = User { id: 1, name: "ada", active: true }
    let other = User { id: 2, ..user }
    let id = user.id
    let name = user.name
}
"#,
    );
}

#[test]
fn arrays_tuples_and_indexing_parse() {
    assert_clean(
        r#"
fn main() {
    let numbers = [1, 2, 3]
    let zeros = [0; 16]
    let pair = (1, "two")
    let unit = ()
    let first = numbers[0]
    let elem = pair.0
}
"#,
    );
}

#[test]
fn strings_and_escapes_parse() {
    assert_clean(
        r#"
fn main() {
    let plain = "hello"
    let escaped = "tab\there\nnewline"
    let quoted = "she said \"hi\""
    let braces = "literal {{braces}}"
    let unicode = "café ☕"
    let ch = 'x'
}
"#,
    );
}

#[test]
fn result_and_option_usage_parses() {
    assert_clean(
        r#"
fn read_config() -> Result<String, Error> {
    let content = fs.read("config.toml")?

    return Ok(content)
}
"#,
    );
}

#[test]
fn const_and_type_alias_parse() {
    assert_clean(
        r#"
const MAX_SIZE: u32 = 1024
const NAME = "umay"

type UserId = u64
type Result<T> = std::Result<T, Error>
"#,
    );
}

#[test]
fn use_and_mod_parse() {
    assert_clean(
        r#"
use std::io
use std::collections::Vector
use std::cmp as compare

mod helpers {
    fn assist() -> i32 {
        return 1
    }
}
"#,
    );
}

#[test]
fn methods_and_self_receivers_parse() {
    assert_clean(
        r#"
struct Counter {
    value: i32
}

impl Counter {
    fn new() -> Counter {
        return Counter { value: 0 }
    }

    fn bump(&mut self) {
        self.value += 1
    }

    fn get(&self) -> i32 {
        return self.value
    }

    fn consume(self) -> i32 {
        return self.value
    }
}
"#,
    );
}

#[test]
fn trailing_comma_everywhere_parses() {
    assert_clean(
        r#"
struct S {
    a: i32,
    b: i32,
}

enum E {
    A,
    B,
}

fn f(a: i32, b: i32,) -> i32 {
    return a
}

fn main() {
    f(1, 2,)
    let t = (1, 2,)
    let a = [1, 2,]
}
"#,
    );
}

#[test]
fn nested_blocks_parse() {
    assert_clean(
        r#"
fn main() {
    {
        let inner = 1
        {
            let deeper = 2
        }
    }

    let x = {
        let y = 1
        y + 1
    }
}
"#,
    );
}

#[test]
fn casts_parse() {
    assert_clean(
        r#"
fn main() {
    let a = 1 as i64
    let b = 1.5 as f32
    let c = x as u8
}
"#,
    );
}

#[test]
fn unsafe_and_defer_parse() {
    assert_clean(
        r#"
fn main() {
    unsafe {
        let p = raw
    }

    defer {
        print("cleanup")
    }
}
"#,
    );
}

#[test]
fn comments_are_ignored() {
    assert_clean(
        r#"
// leading comment
fn main() {
    // inner comment
    let a = 1 // trailing comment
    /* block
       comment */
    let b = 2
}
"#,
    );
}

#[test]
fn multiline_strings_parse() {
    assert_clean(
        r#"fn main() {
    let text = "line one
line two"
}
"#,
    );
}

#[test]
fn empty_function_and_empty_program() {
    assert_clean("fn main() { }");
    assert_clean("");
}

// ---------------------------------------------------------------------------
// Rejected programs — each must produce a diagnostic with a stable code
// ---------------------------------------------------------------------------

#[test]
fn let_without_initializer_is_rejected() {
    let (diags, map) = check_source("t.um", "fn main() {\n    let x: i32\n}\n");
    assert!(diags.has_errors());
    let text = umayc::render_all(&diags, &map, false);
    assert!(text.contains("E0303"), "{text}");
    assert!(text.contains("no initializer"), "{text}");
}

#[test]
fn missing_closing_brace_is_reported() {
    assert_rejects("fn main() {\n    let x = 1\n");
}

#[test]
fn unclosed_paren_in_call_is_reported() {
    assert_rejects("fn main() {\n    print(\"x\"\n}\n");
}

#[test]
fn unexpected_top_level_token_is_reported() {
    assert_codes("let x = 1\n", &["E0100"]);
}

#[test]
fn keyword_as_identifier_is_rejected_with_suggestion() {
    let (diags, map) = check_source("t.um", "fn struct() { }\n");
    assert!(diags.has_errors());
    let text = umayc::render_all(&diags, &map, false);
    assert!(text.contains("E0110"), "{text}");
    assert!(text.contains("struct_"), "{s}", s = text);
}

#[test]
fn unterminated_string_reports_lex_error() {
    // An unterminated string swallows the rest of the file, so follow-on
    // "block not closed" errors are expected. The lexical error must be first.
    let (diags, _) = check_source("t.um", "fn main() {\n    let x = \"oops\n}\n");
    assert_eq!(diags.items()[0].code, "E0002");
}

#[test]
fn unterminated_block_comment_reports_lex_error() {
    assert_codes("/* nope\n", &["E0004"]);
}

#[test]
fn invalid_char_reports_lex_error() {
    assert_codes("fn main() { let x = 1 $ 2 }\n", &["E0001"]);
}

#[test]
fn malformed_match_reports_error() {
    assert_rejects(
        r#"
fn f(x: i32) {
    match x {
        1 => 
    }
}
"#,
    );
}

#[test]
fn report_multiple_errors_without_stopping() {
    // Three independent broken statements: the parser must recover and report
    // more than one diagnostic rather than bailing out at the first.
    let (diags, _map) = check_source(
        "t.um",
        r#"
fn main() {
    let a = (1 +
    let b = )
    let c = ]
}
"#,
    );
    assert!(diags.error_count() >= 2, "expected several errors, got {:?}", diags.items());
}

// ---------------------------------------------------------------------------
// Robustness guarantees that later phases depend on
// ---------------------------------------------------------------------------

#[test]
fn parser_never_panics_on_truncated_input() {
    let full = r#"
struct User { id: u64, name: String }
enum Status { A, B(i32) }
fn f(a: i32) -> i32 { return a + 1 }
fn main() { let x = [1, 2, 3][0]; match x { 1 => print("a"), _ => print("b") } }
"#;
    // Every prefix must be handled without panicking.
    for i in 0..full.len() {
        if !full.is_char_boundary(i) {
            continue;
        }
        let _ = check_source("t.um", &full[..i]);
    }
}

#[test]
fn parser_never_panics_on_random_bytes() {
    // A cheap deterministic fuzz loop: mutate a valid program with random ASCII.
    let seed_src = r#"fn main() { let x = 1 + 2 print("hi") }"#;
    let mut state: u64 = 0x243F_6A88_85A3_08D3;
    let alphabet: &[u8] = b"{}()[]<>+-*/%=!&|^.,:;\"'\\ \nabcletfnnmatchifelse0123456789_$#@?";
    for _ in 0..500 {
        let mut s: Vec<u8> = seed_src.as_bytes().to_vec();
        for _ in 0..4 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let pos = (state >> 33) as usize % s.len();
            let ch = alphabet[(state >> 11) as usize % alphabet.len()];
            s[pos] = ch;
        }
        if let Ok(text) = String::from_utf8(s) {
            let _ = check_source("fuzz.um", &text);
        }
    }
}

#[test]
fn deep_nesting_is_rejected_not_overflowed() {
    // 400 nested parenthesis levels exceed the nesting guard.
    let mut src = String::from("fn main() { let x = ");
    src.push_str(&"(".repeat(400));
    src.push('1');
    src.push_str(&")".repeat(400));
    src.push_str(" }\n");
    let (diags, _) = check_source("deep.um", &src);
    assert!(diags.items().iter().any(|d| d.code == "E0052"), "expected nesting guard to fire");
}

// ---------------------------------------------------------------------------
// Diagnostics contract
// ---------------------------------------------------------------------------

#[test]
fn diagnostics_are_sorted_by_position() {
    let (diags, _) = check_source(
        "t.um",
        r#"
fn main() {
    let a = )
    let b = ]
    let c = }
}
"#,
    );
    let items = diags.items();
    assert!(items.len() >= 2);
    for window in items.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        if let (Some(sa), Some(sb)) = (a.span, b.span) {
            assert!(
                (sa.file, sa.start, sa.end, a.severity.rank()) <= (sb.file, sb.start, sb.end, b.severity.rank()),
                "diagnostics not sorted: {a:?} then {b:?}"
            );
        }
    }
}

#[test]
fn every_diagnostic_code_has_an_explanation() {
    // Exercise a range of malformed snippets, then check every code produced is
    // documented. This is what makes `umay explain <code>` always useful.
    let snippets = [
        "let x = 1",
        "fn () { }",
        "fn f( { }",
        "struct { }",
        "enum { }",
        "impl { }",
        "const X",
        "use",
        "type",
        "fn main() { let x = \"unterminated }",
        "fn main() { /* }",
        "fn main() { let x = 1 $ 2 }",
        "fn main() { let c = 'ab' }",
        "fn main() { let x = add(1 2) }",
        "fn main() { match x { } }",
        "fn main() { let x = [1, 2 }",
        "fn main() { if { } }",
        "fn main() { for x { } }",
        "@",
    ];
    for snippet in snippets {
        let (diags, _) = check_source("t.um", snippet);
        for d in diags.items() {
            assert!(
                umayc::explain_code(d.code).is_some(),
                "code {} emitted for {snippet:?} has no explain entry",
                d.code
            );
        }
    }
}

#[test]
fn json_output_is_valid_and_versioned() {
    let (diags, map) = check_source("t.um", "fn main() {\n    let x = )\n}\n");
    let json = umayc::render_json(&diags, &map);
    assert!(json.starts_with("{\"version\":1,\"diagnostics\":["), "{json}");
    assert!(json.ends_with("]}"), "{json}");
    // Minimal structural validation without a JSON dependency.
    let opens = json.matches('{').count();
    let closes = json.matches('}').count();
    assert_eq!(opens, closes, "unbalanced braces in JSON: {json}");
    assert!(json.contains("\"code\":\"E"), "{json}");
    assert!(json.contains("\"severity\":\"error\""), "{json}");
}

#[test]
fn json_is_empty_when_no_diagnostics() {
    let (diags, map) = check_source("t.um", "fn main() { }\n");
    assert_eq!(umayc::render_json(&diags, &map), "{\"version\":1,\"diagnostics\":[]}");
}

#[test]
fn severities_are_reported_correctly() {
    let (diags, _) = check_source("t.um", "#[unknown]\nfn f() { }\n");
    assert_eq!(diags.items().len(), 1);
    assert_eq!(diags.items()[0].severity, Severity::Warning);
    assert_eq!(diags.items()[0].code, "W0010");
    assert!(!diags.has_errors());
}

#[test]
fn line_and_column_are_one_based_in_json() {
    let src = "fn main() {\n    let x = )\n}\n";
    let (diags, map) = check_source("t.um", src);
    let json = umayc::render_json(&diags, &map);
    assert!(json.contains("\"line\":2"), "{json}");
}

#[test]
fn ast_dump_lists_items() {
    let (out, map) = parse_source("t.um", "struct A { x: i32 }\nfn f() { }");
    let json = umayc::ast_json(&out.program, &map);
    assert!(json.contains("\"kind\":\"Struct\""), "{json}");
    assert!(json.contains("\"kind\":\"Fn\""), "{json}");
}

// ---------------------------------------------------------------------------
// Stability: codes used in documentation must exist
// ---------------------------------------------------------------------------

#[test]
fn documented_example_codes_exist() {
    for code in ["E0101", "E0201", "E0204", "E0207", "E0211", "E0001", "E0303"] {
        assert!(
            umayc::explain_code(code).is_some(),
            "{code} must have an explain entry"
        );
    }
}

#[test]
fn if_condition_brace_opens_the_body_not_a_struct_literal() {
    // `if a > b { a }` must read the brace as the body. Parsing it as a struct
    // literal swallowed the block and made the `else` a syntax error.
    assert_clean("fn f(a: i32, b: i32) -> i32 { if a > b { a } else { b } }");
}

#[test]
fn struct_literals_still_parse_outside_conditions() {
    assert_clean("struct P { x: i32 }\nfn main() {\n    let p = P { x: 1 }\n}");
}

#[test]
fn while_condition_brace_opens_the_body() {
    assert_clean("fn f(c: bool) {\n    while c { let n = 1 }\n}");
}
