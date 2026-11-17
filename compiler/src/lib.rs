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
//! All phases are implemented: semantic analysis, HIR/MIR, borrow checking,
//! code generation, the runtime and the standard library. `ROADMAP.md` records
//! the phases and what has been verified on each supported system.

pub mod aarch64;
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
pub mod target;
pub mod token;
pub mod types;
pub mod upm;

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
/// The dependencies a source file's project declares, for module resolution.
///
/// Every entry point uses this rather than the caller passing a list, so a
/// single file, a tool and the language server all find a declared dependency by
/// the same rule: the manifest lives beside the source.
fn dependencies_for(name: &str) -> Vec<modules::DependencyPath> {
    let path = std::path::Path::new(name);
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    match dir {
        Some(dir) => modules::dependencies_of_project(dir),
        None => Vec::new(),
    }
}

pub fn metadata_source(
    name: impl Into<String>,
    text: impl Into<String>,
) -> Result<Vec<metadata::Symbol>, (Diagnostics, SourceMap)> {
    let name = name.into();
    let (out, mut map) = parse_source(name.clone(), text.into());
    let mut diagnostics = out.diagnostics;
    if diagnostics.has_errors() {
        return Err((diagnostics, map));
    }
    // Modules are read from disk so that a tool asking what a project declares
    // hears about the items in its module files, not only the entry point.
    let deps = dependencies_for(&name);
    let (program, module_diagnostics) =
        modules::load_from_entry_with_deps(out.program, std::path::Path::new(&name), &mut map, &deps);
    append_all(&mut diagnostics, &module_diagnostics);
    if diagnostics.has_errors() {
        return Err((diagnostics, map));
    }
    let symbols = metadata::symbols(&program, &map);
    Ok(symbols)
}

/// Whether a parsed program declares a function called `main` at the top level.
///
/// A program needs one to be linked, and the linker's way of saying so is
/// "undefined reference to `main`" from `collect2`, several stages after the
/// author could do anything about it. Asking here turns that into a diagnostic
/// about their source.
pub fn declares_main(program: &ast::Program) -> bool {
    program.items.iter().any(|item| match item {
        ast::Item::Fn(f) => f.name == "main",
        _ => false,
    })
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
    let deps = dependencies_for(name);
    let (program, module_diagnostics) =
        modules::load_from_entry_with_deps(out.program, std::path::Path::new(name), &mut map, &deps);
    append_all(&mut diagnostics, &module_diagnostics);
    // Analysis has no target: it is the same program on every system, and the
    // prelude's platform bindings only matter for what gets emitted. The host's
    // are used so the names still resolve.
    let (program, prelude_diagnostics) =
        prelude::with_prelude(program, &mut map, crate::target::Target::host());
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
    let (program, prelude_diagnostics) =
        prelude::with_prelude(out.program, &mut map, crate::target::Target::host());
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
///
/// Only the portable body is here. What actually differs between systems -- the
/// C symbols, the numeric constants, whether `fork` exists -- is in the platform
/// layers below, and [`target::Target::runtime_platform_source`] concatenates the
/// right one after this text.
pub const RUNTIME_SOURCE: &str = include_str!("../../runtime/runtime.um");

/// The Linux layer of the runtime: glibc, POSIX names, `fork`.
pub const RUNTIME_PLATFORM_LINUX: &str = include_str!("../../runtime/platform/linux.um");

/// The macOS layer of the runtime: libSystem, `__error`, Darwin's `O_*` table.
pub const RUNTIME_PLATFORM_MACOS: &str = include_str!("../../runtime/platform/macos.um");

/// The Windows layer of the runtime: the MSVC CRT, and threads where `fork`
/// would be.
/// The standard library's POSIX bindings, shared by Linux and macOS, then the
/// one constant that differs between them.
macro_rules! stdlib_platform_posix {
    ($clock:literal, $ops:literal) => {
        concat!(
            include_str!("../../stdlib/platform/net_posix.um"),
            "\n",
            include_str!("../../stdlib/platform/thread_posix.um"),
            "\n",
            include_str!("../../stdlib/platform/time_posix.um"),
            "\n",
            include_str!("../../stdlib/platform/ops_posix.um"),
            "\n",
            include_str!($clock),
            "\n",
            include_str!($ops),
        )
    };
}

/// The standard library's Linux bindings.
pub const STDLIB_PLATFORM_LINUX: &str = stdlib_platform_posix!(
    "../../stdlib/platform/clock_linux.um",
    "../../stdlib/platform/ops_linux.um"
);

/// The standard library's macOS bindings.
pub const STDLIB_PLATFORM_MACOS: &str = stdlib_platform_posix!(
    "../../stdlib/platform/clock_macos.um",
    "../../stdlib/platform/ops_macos.um"
);

/// The standard library's Windows bindings.
pub const STDLIB_PLATFORM_WINDOWS: &str = concat!(
    include_str!("../../stdlib/platform/net_windows.um"),
    "\n",
    include_str!("../../stdlib/platform/thread_windows.um"),
    "\n",
    include_str!("../../stdlib/platform/time_windows.um"),
    "\n",
    include_str!("../../stdlib/platform/ops_windows.um"),
);

pub const RUNTIME_PLATFORM_WINDOWS: &str = include_str!("../../runtime/platform/windows.um");

/// The whole runtime for one target: the portable body, then its platform
/// layer.
///
/// The order matters. The portable body calls `umay_platform_*` functions that
/// the layer defines, and the layer calls `umay_alloc`/`umay_read_*` that the
/// body defines. UMAY resolves items across a whole compilation unit rather
/// than top-down, so a single file in this order satisfies both directions,
/// and the concatenation is what makes the runtime one unit instead of two
/// mutually dependent ones.
pub fn runtime_source_for(target: target::Target) -> String {
    let mut out = String::with_capacity(RUNTIME_SOURCE.len() + 4096);
    out.push_str(RUNTIME_SOURCE);
    out.push_str("\n\n// ---------------------------------------------------------------------------\n");
    out.push_str("// Platform layer: ");
    out.push_str(target.triple);
    out.push_str("\n// ---------------------------------------------------------------------------\n\n");
    out.push_str(target.runtime_platform_source());
    out
}

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
    codegen_source_for(name, text, level, target::Target::host())
}

/// Compiles a source text for one specific target.
///
/// This is what a cross build uses: the frontend and checker are identical, and
/// only the symbol spelling, the object dressing and the calling convention
/// change.
pub fn codegen_source_for(
    name: &str,
    text: &str,
    level: opt::OptLevel,
    target: target::Target,
) -> Result<CodegenOutput, (Diagnostics, SourceMap)> {
    codegen_source_impl(name, text, level, true, target)
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
    codegen_source_without_prelude_for(name, text, level, target::Target::host())
}

/// Compiles a prelude-free source for one target: the runtime's own path.
pub fn codegen_source_without_prelude_for(
    name: &str,
    text: &str,
    level: opt::OptLevel,
    target: target::Target,
) -> Result<CodegenOutput, (Diagnostics, SourceMap)> {
    codegen_source_impl(name, text, level, false, target)
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
    target: target::Target,
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
        let deps = dependencies_for(name);
        let (program, module_diagnostics) = modules::load_from_entry_with_deps(
            out.program,
            std::path::Path::new(name),
            &mut map,
            &deps,
        );
        append_all(&mut diagnostics, &module_diagnostics);
        let (program, prelude_diagnostics) = prelude::with_prelude(program, &mut map, target);
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

    match codegen::generate(&lowered.program, &mut sema.types, &sema.hir, target, with_prelude) {
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
