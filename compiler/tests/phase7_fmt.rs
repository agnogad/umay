//! The formatter's contract, held against the real standard library.
//!
//! A formatter is easy to test on toy inputs and easy to get wrong on real ones,
//! which is the lesson these tests were written from: an early version passed
//! every unit test and reflowed `unsafe { … }`, `Vec { … }` literals and one-line
//! `if` expressions into something no reader would write. A pass over the whole
//! standard library is the test that catches that class of mistake, so the corpus
//! is the test.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate sits inside the repository")
        .to_path_buf()
}

/// Every `.um` file in `stdlib/` and `examples/`.
fn corpus() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["stdlib", "examples"] {
        let path = repo_root().join(dir);
        let Ok(entries) = std::fs::read_dir(&path) else {
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

#[test]
fn the_standard_library_is_already_formatted() {
    let files = corpus();
    assert!(!files.is_empty(), "the corpus should not be empty");
    let mut unformatted = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).expect("corpus file should be readable");
        let out = umayc::fmt::format(&source);
        assert_eq!(out.errors, 0, "{} should lex", file.display());
        if out.text != source {
            unformatted.push(file.display().to_string());
        }
    }
    assert!(
        unformatted.is_empty(),
        "these files are not formatted:\n  {}",
        unformatted.join("\n  ")
    );
}

#[test]
fn formatting_the_corpus_is_idempotent() {
    for file in corpus() {
        let source = std::fs::read_to_string(&file).expect("corpus file should be readable");
        let once = umayc::fmt::format(&source).text;
        let twice = umayc::fmt::format(&once).text;
        assert_eq!(
            once,
            twice,
            "formatting {} twice differs from formatting it once",
            file.display()
        );
    }
}

#[test]
fn the_tokens_of_a_corpus_file_are_unchanged() {
    // The formatter's whole claim is that it rewrites gaps and nothing else. That
    // claim is checked by comparing what the lexer reads before and after: same
    // kinds, same order, same text.
    for file in corpus() {
        let source = std::fs::read_to_string(&file).expect("corpus file should be readable");
        let formatted = umayc::fmt::format(&source).text;
        let before = tokens_of(&source);
        let after = tokens_of(&formatted);
        assert_eq!(
            before,
            after,
            "formatting {} changed its tokens",
            file.display()
        );
    }
}

/// The token texts of a source, in order, with comments and whitespace dropped.
fn tokens_of(src: &str) -> Vec<String> {
    let (tokens, errors) = umayc::lexer::tokenize(src, umayc::source::FileId::DUMMY);
    assert!(errors.is_empty(), "source should lex");
    tokens
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                umayc::token::TokenKind::Eof | umayc::token::TokenKind::Error
            )
        })
        .map(|t| src[t.span.start as usize..t.span.end as usize].to_string())
        .collect()
}

#[test]
fn an_empty_block_does_not_shift_the_lines_after_it() {
    // A `{}` once left the formatter waiting for its `}` after it had already
    // been written, so the next `}` was taken as an empty block's and never
    // dedented: every line after it came out one level too deep.
    let source = "fn f(x: i64) {
    match x {
        1 => {}
        _ => {}
    }
}

fn g(c: bool) {
    while c {}
}

fn h() {
}
";
    let once = umayc::fmt::format(source);
    assert_eq!(once.errors, 0);
    assert_eq!(once.text, source);
    assert_eq!(umayc::fmt::format(&once.text).text, once.text);
}
