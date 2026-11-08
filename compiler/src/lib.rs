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
pub mod asm;
pub mod build;
pub mod cache;
pub mod codegen;
pub mod borrowck;
pub mod diagnostics;
pub mod drop_elab;
pub mod explain;
pub mod fmt;
pub mod lint;
pub mod hir;
pub mod lexer;
pub mod frame;
pub mod layout;
pub mod lower;
pub mod isel;
pub mod machine;
pub mod lsp;
pub mod lock;
pub mod modules;
pub mod manifest;
pub mod lower_expr;
pub mod lower_flow;
pub mod lower_match;
pub mod metadata;
pub mod mir;
pub mod mir_parse;
pub mod mono;
pub mod regalloc;
pub mod opt;
pub mod ownership;
pub mod mir_print;
pub mod parser;
pub mod prelude;
pub mod sema;
pub mod sha256;
pub mod source;
pub mod token;
pub mod types;

use diagnostics::{Diagnostic, Diagnostics};
use source::{FileId, SourceMap};

/// Everything produced by frontend analysis of a single file (Phases 0-1).
pub struct FrontendOutput {
    pub program: ast::Program,
    pub diagnostics: Diagnostics,
}

/// Runs every lint over one in-memory source file.
///
/// A convenience over `parse_source` and `lint` so that a caller does not have to
/// reconstruct the file id: `parse_source` builds a fresh map, so the file it
/// added is the only one in it.
pub fn lint_source(name: impl Into<String>, text: impl Into<String>) -> (Diagnostics, SourceMap) {
    let (out, map) = parse_source(name, text);
    if out.diagnostics.has_errors() {
        return (out.diagnostics, map);
    }
    let id = FileId(0);
    let diags = lint::lint(&map, id, &out.program);
    (diags, map)
}

/// Collects the symbols a program declares, for `umay metadata`.
///
/// Parsing is enough: what a program declares is a property of its syntax, and
/// asking for it should not require the program to also type-check. A program
/// with a type error still has functions and structs, and an editor asking what
/// they are should get them.
pub fn metadata_source(
    name: impl Into<String>,
    text: impl Into<String>,
) -> Result<Vec<metadata::Symbol>, (Diagnostics, SourceMap)> {
    let (out, map) = parse_source(name, text);
    if out.diagnostics.has_errors() {
        return Err((out.diagnostics, map));
    }
    let symbols = metadata::symbols(&out.program, &map);
    Ok(symbols)
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

/// Convenience for tests: runs the frontend only and returns the diagnostics.
///
/// This stops after parsing. Use [`analyze_source`] to include semantic
/// analysis, which is what `umay check` runs.
pub fn check_source(name: &str, text: &str) -> (Diagnostics, SourceMap) {
    let (out, map) = parse_source(name.to_string(), text.to_string());
    (out.diagnostics, map)
}

/// Runs the full frontend: parsing, semantic analysis, lowering, and ownership
/// and borrow checking.
///
/// Each phase runs only if the previous one reported no errors. Reporting
/// ownership errors for a program whose types do not check would bury the real
/// problem under consequences of it.
pub fn analyze_source(name: &str, text: &str) -> (Diagnostics, SourceMap) {
    let mut map = SourceMap::new();
    let id = map.add(name, text);
    let out = parse_file(&map, id);
    let mut diagnostics = out.diagnostics;
    if diagnostics.has_errors() {
        return (diagnostics, map);
    }
    // Modules are read from disk before the prelude is prepended, so everything
    // downstream sees one flat list of items and needs no idea of files.
    let (program, module_diagnostics) =
        modules::load_from_entry(out.program, std::path::Path::new(name), &mut map);
    append_all(&mut diagnostics, &module_diagnostics);
    let (program, prelude_diagnostics) = prelude::with_prelude(program, &mut map);
    append_all(&mut diagnostics, &prelude_diagnostics);

    let mut sema = sema::analyze(&program, &map);
    append_all(&mut diagnostics, &sema.diagnostics);
    drop_prelude_warnings(&mut diagnostics, &map);
    diagnostics.sort();
    if diagnostics.has_errors() {
        return (diagnostics, map);
    }

    // Ownership checking runs on MIR, never on source text, so its results do
    // not depend on formatting or expression nesting.
    let mut lowered = lower::lower(&mut sema.hir, &mut sema.types);
    for (message, span) in &lowered.errors {
        // These are codegen-shaped problems the checker could not see, so they
        // carry the backend's code rather than a front-end one.
        diagnostics.push(Diagnostic::error("E3001", message.clone()).at(*span));
    }
    drop_elab::elaborate(&mut lowered.program, &sema.types);
    let ownership = ownership::check_program(&lowered.program, &sema.types);
    append_all(&mut diagnostics, &ownership);
    drop_prelude_warnings(&mut diagnostics, &map);
    diagnostics.sort();
    (diagnostics, map)
}

/// Appends diagnostics, filling in the explanation text from the database.
fn append_all(diagnostics: &mut Diagnostics, extra: &Diagnostics) {
    for d in extra.items() {
        let mut d = d.clone();
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
}

/// Runs the pipeline as far as MIR and returns the printed MIR.
///
/// This is the `umay mir` entry point and the basis of the Phase 3 tests: every
/// program that type-checked must produce MIR.
pub fn lower_to_mir(name: &str, text: &str) -> Result<String, (Diagnostics, SourceMap)> {
    lower_to_mir_opt(name, text, crate::opt::OptLevel::O0)
}

/// Like [`lower_to_mir`], but runs the optimisation pipeline first.
///
/// `O0` prints exactly what lowering produced, which is what `umay mir` has
/// always shown; a higher level shows what the backend will actually receive.
pub fn lower_to_mir_opt(
    name: &str,
    text: &str,
    level: crate::opt::OptLevel,
) -> Result<String, (Diagnostics, SourceMap)> {
    let mut map = SourceMap::new();
    let id = map.add(name, text);
    let out = parse_file(&map, id);
    let mut diagnostics = out.diagnostics;
    if diagnostics.has_errors() {
        return Err((diagnostics, map));
    }
    let (program, prelude_diagnostics) = prelude::with_prelude(out.program, &mut map);
    append_all(&mut diagnostics, &prelude_diagnostics);
    let mut sema = sema::analyze(&program, &map);
    for d in sema.diagnostics.items() {
        let mut d = d.clone();
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
    if diagnostics.has_errors() {
        return Err((diagnostics, map));
    }
    let mut lowered = lower::lower(&mut sema.hir, &mut sema.types);
    for (message, span) in &lowered.errors {
        // These are codegen-shaped problems the checker could not see, so they
        // carry the backend's code rather than a front-end one.
        diagnostics.push(Diagnostic::error("E3001", message.clone()).at(*span));
    }
    drop_elab::elaborate(&mut lowered.program, &sema.types);
    crate::opt::optimize(&mut lowered.program, &sema.types, level);
    // `umay mir` shows the lowering, so ownership errors are not fatal here:
    // being able to see the MIR of a program that fails borrow checking is
    // exactly when it is most useful.
    Ok(mir_print::print_program(&lowered.program, &mut sema.types))
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

/// The UMAY runtime, compiled with every program.
///
/// It is embedded in the binary rather than read from disk so that an installed
/// compiler cannot be broken by a missing or edited runtime file: the runtime and
/// the compiler that accepts it always ship together.
pub const RUNTIME_SOURCE: &str = include_str!("../../runtime/runtime.um");

/// Everything needed to generate code, or the diagnostics that prevent it.
pub struct CodegenOutput {
    pub assembly: String,
    pub program: mir::MirProgram,
    pub types: types::TyCtxt,
    /// Warnings that survived the build. A build is not a check, but a warning
    /// it saw and threw away is one the user never gets to read, so they are
    /// carried out with the assembly rather than dropped on success.
    pub diagnostics: Diagnostics,
    pub map: SourceMap,
}

/// Runs the whole pipeline from source text to assembly.
///
/// This is the entry point `umay build` uses. Diagnostics from every phase are
/// returned together so that a failing build reports all of its problems at once
/// rather than one per attempt.
pub fn codegen_source(
    name: &str,
    text: &str,
    level: opt::OptLevel,
) -> Result<CodegenOutput, (Diagnostics, SourceMap)> {
    codegen_source_impl(name, text, level, true)
}

/// Compiles a source text without the prelude.
///
/// The runtime is written in UMAY and needs the language, but it must not
/// receive the prelude: the prelude binds the runtime's own symbols, and
/// including it would declare `umay_alloc` twice — once as the definition being
/// compiled and once as an `extern`.
pub fn codegen_source_without_prelude(
    name: &str,
    text: &str,
    level: opt::OptLevel,
) -> Result<CodegenOutput, (Diagnostics, SourceMap)> {
    codegen_source_impl(name, text, level, false)
}

/// Drops warnings that point into the prelude.
///
/// The prelude is the compiler's own source: it is prepended to every program,
/// the user cannot edit it, and nothing it does is a decision the user made.
/// Warnings about it are therefore not advice, they are noise -- thirty-seven of
/// them on a four-line program, which buried the one that was about the user's
/// code. Errors are left alone: a broken prelude means the compiler is broken,
/// and that must be visible.
fn drop_prelude_warnings(diagnostics: &mut Diagnostics, map: &SourceMap) {
    let Some(prelude) = crate::prelude::prelude_file(map) else {
        return;
    };
    let is_noise = |d: &crate::diagnostics::Diagnostic| {
        d.severity != crate::diagnostics::Severity::Error
            && d.span.is_some_and(|s| s.file == prelude)
    };
    diagnostics.retain(|d| !is_noise(d));
}

fn codegen_source_impl(
    name: &str,
    text: &str,
    level: opt::OptLevel,
    with_prelude: bool,
) -> Result<CodegenOutput, (Diagnostics, SourceMap)> {
    let mut map = SourceMap::new();
    let id = map.add(name, text);
    let out = parse_file(&map, id);
    let mut diagnostics = out.diagnostics;
    if diagnostics.has_errors() {
        return Err((diagnostics, map));
    }
    // Modules load before the prelude, so the prelude's items sit in front of a
    // program that is already complete. The runtime compiles without the prelude
    // and has no modules of its own, so it takes the plain path.
    let program = if with_prelude {
        let (program, module_diagnostics) =
            modules::load_from_entry(out.program, std::path::Path::new(name), &mut map);
        append_all(&mut diagnostics, &module_diagnostics);
        let (program, prelude_diagnostics) = prelude::with_prelude(program, &mut map);
        append_all(&mut diagnostics, &prelude_diagnostics);
        program
    } else {
        out.program
    };

    let mut sema = sema::analyze(&program, &map);
    append_all(&mut diagnostics, &sema.diagnostics);
    drop_prelude_warnings(&mut diagnostics, &map);
    diagnostics.sort();
    if diagnostics.has_errors() {
        return Err((diagnostics, map));
    }

    let mut lowered = lower::lower(&mut sema.hir, &mut sema.types);
    for (message, span) in &lowered.errors {
        // These are codegen-shaped problems the checker could not see, so they
        // carry the backend's code rather than a front-end one.
        diagnostics.push(Diagnostic::error("E3001", message.clone()).at(*span));
    }
    drop_elab::elaborate(&mut lowered.program, &sema.types);
    let ownership = ownership::check_program(&lowered.program, &sema.types);
    append_all(&mut diagnostics, &ownership);
    drop_prelude_warnings(&mut diagnostics, &map);
    diagnostics.sort();
    if diagnostics.has_errors() {
        return Err((diagnostics, map));
    }

    opt::optimize(&mut lowered.program, &sema.types, level);

    match codegen::generate(&lowered.program, &mut sema.types, &sema.hir) {
        Ok(assembly) => Ok(CodegenOutput {
            assembly,
            program: lowered.program,
            types: sema.types,
            diagnostics,
            map,
        }),
        Err(errors) => {
            for error in errors {
                diagnostics.push(
                    Diagnostic::error(
                        "E3001",
                        format!("cannot generate code for `{}`: {}", error.function, error.message),
                    )
                    .with_cause("this construct is not supported by the code generator yet")
                    .with_explanation(
                        "UMAY's backend is built incrementally. A construct that type-checks \
                         but has no code generation yet is reported here rather than silently \
                         producing wrong code.",
                    ),
                );
            }
            Err((diagnostics, map))
        }
    }
}
