//! `umay metadata` — what a program declares.
//!
//! The output is a contract with tools that are not the compiler: an editor's
//! completion, a doc generator, a test harness. Two properties matter and both
//! are tested here. It has to describe the program the user wrote -- generic
//! arguments and trait bounds included, since dropping them describes a
//! different program. And it has to keep describing it the same way, so a driver
//! can compare two documents and mean something by the difference.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate sits inside the repository")
        .to_path_buf()
}

fn metadata_of(src: &str) -> Vec<umayc::metadata::Symbol> {
    match umayc::metadata_source("t.um", src) {
        Ok(symbols) => symbols,
        Err((diags, _map)) => panic!("the source should parse, got {} error(s)", diags.len()),
    }
}

fn text_of(src: &str) -> String {
    umayc::metadata::to_text(&metadata_of(src))
}

fn json_of(src: &str) -> String {
    umayc::metadata::to_json(&metadata_of(src))
}

#[test]
fn a_function_is_described_by_its_signature() {
    let text = text_of("fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n");
    assert_eq!(text.trim(), "fn add(a: i64, b: i64) -> i64");
}

#[test]
fn generic_arguments_are_kept() {
    // A path segment's span covers only its name, so an implementation that
    // sliced the source text produced `Pair<i64>` as `Pair` and described a
    // different type. The text is built from the tree instead.
    let text = text_of(
        "struct Pair<T> {\n    first: T,\n}\nfn make() -> Pair<i64> {\n    return Pair { first: 1i64 }\n}\n",
    );
    assert!(text.contains("fn make() -> Pair<i64>"), "got:\n{text}");
    assert!(text.contains("struct Pair<T>"), "got:\n{text}");
}

#[test]
fn a_trait_bound_is_part_of_the_signature() {
    // The bound says what a caller may rely on; leaving it out would describe a
    // function that accepts more than this one does.
    let text = text_of(
        "trait Size {\n    fn size(&self) -> usize\n}\nfn measure<T: Size>(value: T) -> usize {\n    return value.size()\n}\n",
    );
    assert!(text.contains("fn measure<T: Size>(value: T) -> usize"), "got:\n{text}");
}

#[test]
fn a_method_records_its_receiver() {
    let text = text_of(
        "struct S {\n    n: i64,\n}\nimpl S {\n    fn get(&self) -> i64 {\n        return self.n\n    }\n    fn take(self) -> i64 {\n        return 1i64\n    }\n}\n",
    );
    assert!(text.contains("fn S::get(&self) -> i64"), "got:\n{text}");
    assert!(text.contains("fn S::take(self) -> i64"), "got:\n{text}");
}

#[test]
fn an_extern_function_names_its_linker_symbol() {
    let text = text_of("extern fn \"umay_write\" write_byte(at: *mut u8, byte: u8)\n");
    assert!(text.contains(r#"[extern "umay_write"]"#), "got:\n{text}");
}

#[test]
fn an_enum_lists_its_variants_with_their_payloads() {
    let json = json_of("enum Shape {\n    Dot,\n    Circle(i64),\n}\n");
    assert!(json.contains(r#"{"name":"Dot","tuple":[],"struct":[]}"#), "got:\n{json}");
    assert!(json.contains(r#"{"name":"Circle","tuple":["i64"],"struct":[]}"#), "got:\n{json}");
}

#[test]
fn a_trait_impl_names_both_the_trait_and_the_type() {
    let text = text_of(
        "trait Size {\n    fn size(&self) -> usize\n}\nstruct S {\n    n: i64,\n}\nimpl Size for S {\n    fn size(&self) -> usize {\n        return 1usize\n    }\n}\n",
    );
    assert!(text.contains("impl Size for S { size }"), "got:\n{text}");
}

#[test]
fn a_module_keeps_its_path_in_the_name() {
    // Two modules may each declare `open`; the path is what tells them apart.
    let text = text_of(
        "mod file {\n    fn open() -> i64 {\n        return 1i64\n    }\n}\nmod net {\n    fn open() -> i64 {\n        return 2i64\n    }\n}\n",
    );
    assert!(text.contains("fn file::open()"), "got:\n{text}");
    assert!(text.contains("fn net::open()"), "got:\n{text}");
}

#[test]
fn a_use_declaration_is_not_a_symbol() {
    // An import declares nothing this module owns; listing it would make every
    // import look like a symbol of its own.
    let json = json_of("use std::io::File\nfn f() -> i64 {\n    return 0i64\n}\n");
    assert_eq!(json.matches("\"kind\"").count(), 1, "got:\n{json}");
}

#[test]
fn the_document_carries_its_version() {
    let json = json_of("fn f() -> i64 {\n    return 0i64\n}\n");
    assert!(json.starts_with(r#"{"version":1,"symbols":["#), "got:\n{json}");
}

#[test]
fn a_program_with_a_type_error_still_describes_what_it_declares() {
    // Metadata is a question about syntax, not about well-typedness. An editor
    // asks it while the program is still being written.
    let src = "fn f() -> i64 {\n    return \"not an integer\"\n}\n";
    let symbols = metadata_of(src);
    assert_eq!(symbols.len(), 1);
}

#[test]
fn a_file_that_does_not_parse_reports_the_parse_error() {
    let result = umayc::metadata_source("t.um", "fn f( {\n");
    assert!(result.is_err(), "a file that does not parse has no symbols to report");
}

#[test]
fn the_standard_library_describes_itself() {
    for name in ["option.um", "bytes.um", "thread.um"] {
        let path = repo_root().join("stdlib").join(name);
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let symbols = match umayc::metadata_source(path.to_string_lossy().to_string(), source) {
            Ok(symbols) => symbols,
            Err((diags, _map)) => panic!("{name} should parse, got {} error(s)", diags.len()),
        };
        assert!(!symbols.is_empty(), "{name} declares nothing");
        // Every document must be JSON that at least begins and ends correctly.
        let json = umayc::metadata::to_json(&symbols);
        assert!(json.starts_with('{') && json.ends_with('}'), "{name}: {json}");
        // And the text rendering must mention every symbol's name.
        let text = umayc::metadata::to_text(&symbols);
        for symbol in &symbols {
            let name = match symbol {
                umayc::metadata::Symbol::Fn { name, .. }
                | umayc::metadata::Symbol::Struct { name, .. }
                | umayc::metadata::Symbol::Enum { name, .. }
                | umayc::metadata::Symbol::Trait { name, .. }
                | umayc::metadata::Symbol::Const { name, .. }
                | umayc::metadata::Symbol::Alias { name, .. }
                | umayc::metadata::Symbol::Module { name } => name,
                umayc::metadata::Symbol::Impl { owner, .. } => owner,
            };
            assert!(text.contains(name.as_str()), "{name} missing from {text}");
        }
    }
}
