//! # umayc — the UMAY compiler
//!
//! Module map (see `docs/compiler-architecture.md`):
//!
//! | module        | phase                          |
//! |---------------|--------------------------------|
//! | [`source`]    | source text + spans            |
//! | [`token`]     | token definitions              |
//! | [`lexer`]     | source -> tokens               |
//! | [`ast`]       | abstract syntax tree           |
//! | [`parser`]    | tokens -> AST                  |
//! | [`diagnostics`] | error reporting (human + JSON) |
//! | [`explain`]   | `umay explain` database        |
//!
//! Phases 2+ (semantic analysis, HIR/MIR, borrow checking, codegen) are not yet
//! implemented; see `ROADMAP.md`.

pub mod ast;
pub mod cache;
pub mod diagnostics;
pub mod explain;
pub mod lexer;
pub mod parser;
pub mod source;
pub mod token;

use diagnostics::{Diagnostic, Diagnostics};
use source::{FileId, SourceMap};

/// Everything produced by frontend analysis of a single file (Phases 0-1).
pub struct FrontendOutput {
    pub program: ast::Program,
    pub diagnostics: Diagnostics,
}

/// Runs the lexer and parser over one in-memory source file.
pub fn parse_source(name: impl Into<String>, text: impl Into<String>) -> (FrontendOutput, SourceMap) {
    let mut map = SourceMap::new();
    let id = map.add(name, text);
    let out = parse_file(&map, id);
    (out, map)
}

/// Runs the lexer and parser over a file already registered in `map`.
pub fn parse_file(map: &SourceMap, id: FileId) -> FrontendOutput {
    let file = map.file(id);
    let (tokens, lex_errors) = lexer::tokenize(file.src(), id);
    let parsed = parser::parse(tokens, lex_errors, id);
    let mut diagnostics = Diagnostics::new();
    // Attach the canonical cause/explanation for every code that has an entry,
    // unless the emitting phase already provided a more specific cause.
    for mut d in parsed.diagnostics {
        if let Some(entry) = explain::lookup(d.code) {
            if d.cause.is_none() {
                d.cause = Some(entry.summary.to_string());
            }
            if d.explanation.is_none() {
                d.explanation = Some(entry.explanation.to_string());
            }
        }
        diagnostics.push(d);
    }
    diagnostics.sort();
    FrontendOutput { program: parsed.program, diagnostics }
}

/// Convenience for tests: runs the frontend and returns the diagnostics.
pub fn check_source(name: &str, text: &str) -> (Diagnostics, SourceMap) {
    let (out, map) = parse_source(name.to_string(), text.to_string());
    (out.diagnostics, map)
}

/// Renders all diagnostics of a compilation for humans.
pub fn render_all(diags: &Diagnostics, map: &SourceMap, color: bool) -> String {
    let opts = diagnostics::RenderOptions { color, ..Default::default() };
    let mut out = String::new();
    for d in diags.items() {
        out.push_str(&diagnostics::render(d, map, opts));
        out.push('\n');
    }
    out
}

/// Renders all diagnostics for humans, optionally including full explanations.
pub fn render_all_full(diags: &Diagnostics, map: &SourceMap, color: bool, explain: bool) -> String {
    let opts = diagnostics::RenderOptions { color, show_explanation: explain, ..Default::default() };
    let mut out = String::new();
    for d in diags.items() {
        out.push_str(&diagnostics::render(d, map, opts));
        out.push('\n');
    }
    out
}

/// Serializes the AST to JSON (`umay ast`).
pub fn ast_json(program: &ast::Program, map: &SourceMap) -> String {
    cache::ast_json(program, map)
}

pub use cache::{cache_dir, clear_cache, ensure_cache_dir};

/// Renders all diagnostics as the versioned JSON schema.
pub fn render_json(diags: &Diagnostics, map: &SourceMap) -> String {
    diagnostics::to_json(diags, map)
}

/// Returns the canonical explanation for a diagnostic code, if documented.
pub fn explain_code(code: &str) -> Option<&'static explain::ExplainEntry> {
    explain::lookup(code)
}

/// Builds a diagnostic with the standard cause/explanation attached for `code`.
pub fn diagnostic_with_context(code: &'static str, span: source::Span, message: impl Into<String>) -> Diagnostic {
    let mut d = Diagnostic::error(code, message).at(span);
    if let Some(entry) = explain::lookup(code) {
        d = d.with_cause(entry.summary).with_explanation(entry.explanation);
    }
    d
}
