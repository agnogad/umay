//! Phase 17: parser support for the syntax `umay bindgen` emits (v2 spec §5).
//!
//! Two additions are covered here: crate-level `#![feature(...)]` inner
//! attributes and grouped `extern "C" { ... }` blocks. The v1 spellings must
//! keep working unchanged.

use umayc::ast::Item;
use umayc::{analyze_source, parse_source};

fn parse_clean(src: &str) -> umayc::ast::Program {
    let (out, map) = parse_source("v2.um", src);
    let rendered = umayc::render_all(&out.diagnostics, &map, false);
    assert!(
        out.diagnostics.is_empty(),
        "expected a clean parse, got:\n{rendered}\n---\n{src}"
    );
    out.program
}

#[test]
fn bindgen_shape_parses_and_checks() {
    // The exact shape emitted for a header with an opaque struct pointer.
    let src = r#"
#![feature(extern_abi)]

extern "C" {
    fn add(a: i32, b: i32) -> i32;
    type Widget;
    fn widget_create() -> *mut Widget;
}
"#;
    let program = parse_clean(src);
    assert_eq!(
        program.features.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["extern_abi"]
    );
    assert_eq!(program.features[0].name_span.start, 12);
    assert_eq!(program.items.len(), 3);

    let fns: Vec<_> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(fns.len(), 2);
    assert_eq!(fns[0].name, "add");
    assert_eq!(fns[0].abi(), Some("C"));
    assert!(fns[0].is_extern);
    assert!(fns[0].body.is_none());
    assert_eq!(fns[1].name, "widget_create");
    assert_eq!(fns[1].abi(), Some("C"));

    // The opaque `type Widget;` becomes the smallest existing named-type item.
    assert!(matches!(&program.items[1], Item::Struct(s) if s.name == "Widget" && s.fields.is_empty()));

    let (diags, map) = analyze_source("v2.um", src);
    let rendered = umayc::render_all(&diags, &map, false);
    assert!(!diags.has_errors(), "check rejected generated syntax:\n{rendered}");
}

#[test]
fn extern_block_with_several_fns_and_variadic() {
    let program = parse_clean(
        r#"
#![feature(extern_abi)]

extern "C" {
    fn printf(fmt: *const u8, ...) -> i32;
    fn puts(s: *const u8) -> i32;
    fn noargs();
}
"#,
    );
    assert_eq!(program.items.len(), 3);
    let printf = match &program.items[0] {
        Item::Fn(f) => f,
        other => panic!("expected a fn, got {other:?}"),
    };
    assert_eq!(printf.name, "printf");
    assert!(printf.is_variadic());
    assert_eq!(printf.params.len(), 1);
    let puts = match &program.items[1] {
        Item::Fn(f) => f,
        other => panic!("expected a fn, got {other:?}"),
    };
    assert!(!puts.is_variadic());
    assert_eq!(puts.abi(), Some("C"));
}

#[test]
fn two_feature_attributes_union_their_names() {
    let program = parse_clean(
        r#"
#![feature(extern_abi)]
// a comment between the attributes
#![feature(closures, dyn_trait,)]

fn main() {}
"#,
    );
    let names: Vec<_> = program.features.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["extern_abi", "closures", "dyn_trait"]);
    assert_eq!(program.items.len(), 1);
}

#[test]
fn omitted_abi_defaults_to_c() {
    let program = parse_clean("extern { fn f(x: i32) -> i32; }\n");
    match &program.items[0] {
        Item::Fn(f) => {
            assert!(f.is_extern);
            assert_eq!(f.abi(), Some("C"));
        }
        other => panic!("expected a fn, got {other:?}"),
    }
}

#[test]
fn malformed_feature_attribute_recovers_without_panic() {
    // Missing `)` and `]`, then a valid attribute and an item. Parsing must
    // terminate and the following item must still be collected.
    let (out, _map) = parse_source("v2.um", "#![feature(extern_abi\n#![feature(unknown_name)]\nfn ok() {}\n");
    let has_error = out.diagnostics.has_errors();
    assert!(has_error, "missing `]` must be diagnosed");
    assert!(
        out.program.items.iter().any(|i| matches!(i, Item::Fn(f) if f.name == "ok")),
        "parser must recover and still see the following item: {:#?}",
        out.program.items
    );
    // Unknown names are not validated by the parser; the gate pass owns that.
    assert!(
        out.program.features.iter().any(|f| f.name == "unknown_name"),
        "well-formed unknown names are still recorded"
    );
}

#[test]
fn malformed_extern_block_recovers_without_panic() {
    let (out, _map) = parse_source("v2.um", "extern \"C\" { fn f(x: i32)\n");
    assert!(
        out.program.items.iter().any(|i| matches!(i, Item::Fn(f) if f.name == "f")),
        "the complete declaration inside the broken block is still collected"
    );
}

#[test]
fn v1_extern_fn_and_operator_syntax_still_work() {
    let program = parse_clean(
        r#"
extern fn "c_add" c_add(a: i32, b: i32) -> i32;

struct P { x: i32, y: i32 }

fn main() {
    let p = P { x: 1, y: 2 };
    let bits = p.x | p.y;
    let f = |x: i32| x + 1;
    let _ = bits + f(p.x);
}
"#,
    );
    assert_eq!(program.features.len(), 0);
    let first = match &program.items[0] {
        Item::Fn(f) => f,
        other => panic!("expected a fn, got {other:?}"),
    };
    assert_eq!(first.link_name.as_deref(), Some("c_add"));
    assert!(first.is_extern);
}
