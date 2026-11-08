//! Command-line interface for the `umay` compiler driver.
//!
//! Design constraints (from `docs/build-system.md`):
//! * exit codes are stable and documented,
//! * `--format json` produces the versioned diagnostic schema on stdout and
//!   nothing else,
//! * no command ever writes compiler temporaries into the project directory.

use std::io::Write;

use umayc::diagnostics::Diagnostics;
use umayc::source::SourceMap;
use umayc::render_json;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Exit codes are part of the public interface used by editors and AI agents.
pub mod exit {
    pub const OK: u8 = 0;
    pub const COMPILE_ERROR: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const IO_ERROR: u8 = 3;
    pub const NOT_IMPLEMENTED: u8 = 4;
    pub const CODEGEN_ERROR: u8 = 5;
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(args: Vec<String>) -> ExitCode {
    let mut args = args.into_iter().peekable();
    let Some(cmd) = args.next() else {
        print_help();
        return ExitCode::from(exit::USAGE);
    };
    let rest: Vec<String> = args.collect();
    let code = match cmd.as_str() {
        "check" => cmd_check(&rest),
        "explain" => cmd_explain(&rest),
        "ast" => cmd_ast(&rest),
        "metadata" => cmd_metadata(&rest),
        "mir" => cmd_mir(&rest),
        "doctor" => cmd_doctor(&rest),
        "new" => cmd_new(&rest),
        "clean" => cmd_clean(&rest),
        "version" | "--version" | "-V" => {
            println!("umay {VERSION}");
            exit::OK
        }
        "help" | "--help" | "-h" => {
            print_help();
            exit::OK
        }
        "build" => cmd_build(&rest, false),
        "run" => cmd_build(&rest, true),
        "asm" => cmd_asm(&rest),
        "fmt" => cmd_fmt(&rest),
        "lint" => cmd_lint(&rest),
        "lsp" => umayc::lsp::serve() as u8,
        other => {
            eprintln!("umay: unknown command `{other}`");
            eprintln!("run `umay help` for the list of commands");
            exit::USAGE
        }
    };
    ExitCode::from(code)
}

fn not_yet(cmd: &str, phase: &str) -> u8 {
    eprintln!("umay: `{cmd}` is not implemented yet; it is planned for {phase}.");
    eprintln!("Every other command works; see ROADMAP.md for what is left.");
    exit::NOT_IMPLEMENTED
}

fn print_help() {
    println!(
        "\
umay {VERSION} — the UMAY compiler

USAGE:
    umay <COMMAND> [OPTIONS]

COMMANDS:
    check <files...>   Parse and validate UMAY source
    build <file>       Compile to an executable in dist/
    run <file>         Compile and run it, reporting its exit status
    asm <file>         Print the generated assembly
    ast <file>         Print the parsed syntax tree
    mir <file>         Print the lowered MIR (basic blocks)
    fmt <files...>     Rewrite whitespace to the standard style (--check: report only)
    lint <files...>    Report style findings (--rules: list them)
    explain <CODE>     Explain a diagnostic code (e.g. umay explain E0101)
    doctor             Report toolchain and environment information
    new <name>         Create a new UMAY project
    clean              Remove build artifacts (dist/)
    version            Print the compiler version

OPTIONS:
    --format human|json    Diagnostic output format (default: human)
    --color auto|always|never
    --explain              Include the full explanation under each diagnostic
    -O0 .. -O3             Optimisation level for build, run and asm (-O = -O2)

EXIT CODES:
    0 success   1 compile error   2 usage error   3 I/O error
    4 not implemented   5 code generation failed"
    );
}

/// A parsed `--flag` set shared by the file-oriented commands.
struct Options {
    format_json: bool,
    explain: bool,
    color: Option<bool>,
    /// Optimisation level for `mir` and (later) `build`.
    opt_level: umayc::opt::OptLevel,
    files: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            format_json: false,
            explain: false,
            color: None,
            opt_level: umayc::opt::OptLevel::O0,
            files: Vec::new(),
        }
    }
}

fn parse_options(args: &[String]) -> Result<Options, (u8, String)> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            // `-O`, `-O2`, `-O3`: optimisation level. `-O` alone means `-O2`,
            // matching the convention every systems compiler shares.
            _ if a == "-O" => {
                opts.opt_level = umayc::opt::OptLevel::O2;
            }
            _ if a.starts_with("-O") => {
                let level = &a[2..];
                match umayc::opt::OptLevel::parse(level) {
                    Some(l) => opts.opt_level = l,
                    None => {
                        return Err((
                            exit::USAGE,
                            format!("invalid optimisation level `{a}` (expected -O, -O0, -O1, -O2 or -O3)"),
                        ))
                    }
                }
            }
            "--format" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("json") => opts.format_json = true,
                    Some("human") => opts.format_json = false,
                    Some(other) => {
                        return Err((exit::USAGE, format!("invalid --format `{other}` (expected `human` or `json`)")))
                    }
                    None => return Err((exit::USAGE, "`--format` requires a value".into())),
                }
            }
            "--color" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("always") => opts.color = Some(true),
                    Some("never") => opts.color = Some(false),
                    Some("auto") => opts.color = None,
                    Some(other) => {
                        return Err((exit::USAGE, format!("invalid --color `{other}` (expected `auto`, `always` or `never`)")))
                    }
                    None => return Err((exit::USAGE, "`--color` requires a value".into())),
                }
            }
            "--explain" => opts.explain = true,
            other if other.starts_with('-') => {
                return Err((exit::USAGE, format!("unknown option `{other}`")));
            }
            other => opts.files.push(other.to_string()),
        }
        i += 1;
    }
    Ok(opts)
}

fn use_color(requested: Option<bool>) -> bool {
    match requested {
        Some(v) => v,
        None => {
            // auto: only when stdout is a terminal and NO_COLOR is unset.
            std::env::var_os("NO_COLOR").is_none() && std::io::IsTerminal::is_terminal(&std::io::stdout())
        }
    }
}

/// `umay fmt`: rewrites the whitespace between tokens and nothing else.
///
/// The formatter is deliberately conservative: it changes no token, and it
/// keeps every comment and every line break the author wrote. A file that is
/// already formatted is not rewritten, so running it twice leaves the same
/// bytes and its exit status tells a script whether anything would have changed.
fn cmd_fmt(args: &[String]) -> u8 {
    let check_only = args.iter().any(|a| a == "--check");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if files.is_empty() {
        eprintln!("umay: `fmt` requires at least one source file");
        return exit::USAGE;
    }
    let mut changed_any = false;
    let mut io_failed = false;
    let mut broken = false;
    for file in files {
        let path = Path::new(file);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("umay: cannot read `{file}`: {e}");
                io_failed = true;
                continue;
            }
        };
        let result = umayc::fmt::format(&text);
        if result.errors > 0 {
            // A file that does not lex has no well-defined gaps, so writing it
            // back would be a guess. Saying so is the honest answer.
            eprintln!("umay: `{file}` has {} lexical errors; not formatted", result.errors);
            broken = true;
            continue;
        }
        if result.text == text {
            continue;
        }
        changed_any = true;
        if check_only {
            println!("{file}");
            continue;
        }
        if let Err(e) = std::fs::write(path, &result.text) {
            eprintln!("umay: cannot write `{file}`: {e}");
            io_failed = true;
        }
    }
    if broken {
        return exit::COMPILE_ERROR;
    }
    if io_failed {
        return exit::IO_ERROR;
    }
    if check_only && changed_any {
        // A script asks "is this tree formatted" by the exit status alone.
        return exit::COMPILE_ERROR;
    }
    exit::OK
}

/// `umay lint`: style and correctness tips the compiler has no stake in.
///
/// These are separate from `check` because they are opinions: the code compiles
/// and does what it says, and a project should adopt the advice at its own pace
/// rather than find it in every build. `--rules` prints the list so a reader can
/// decide what to adopt.
fn cmd_lint(args: &[String]) -> u8 {
    if args.iter().any(|a| a == "--rules") {
        print!("{}", umayc::lint::describe_rules());
        return exit::OK;
    }
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    if opts.files.is_empty() {
        eprintln!("umay: `lint` requires at least one source file");
        return exit::USAGE;
    }
    let color = use_color(opts.color);
    let mut any = false;
    let mut io_failed = false;
    let mut total = 0usize;
    for file in &opts.files {
        let path = Path::new(file);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("umay: cannot read `{file}`: {e}");
                io_failed = true;
                continue;
            }
        };
        // A file that does not parse has no tree to lint, and the parse errors
        // are what the reader needs to see first. `lint_source` reports those
        // instead of a finding list.
        let (diags, map) = umayc::lint_source(path.to_string_lossy().to_string(), text);
        let count = diags.items().len();
        total += count;
        if count > 0 {
            any = true;
        }
        if opts.format_json {
            println!("{}", render_json(&diags, &map));
        } else if count > 0 {
            print!("{}", umayc::render_all_full(&diags, &map, color, opts.explain));
        }
    }
    if io_failed {
        return exit::IO_ERROR;
    }
    if !opts.format_json {
        eprintln!(
            "{total} finding{}",
            if total == 1 { "" } else { "s" }
        );
    }
    if any {
        return exit::COMPILE_ERROR;
    }
    exit::OK
}

fn cmd_check(args: &[String]) -> u8 {
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    if opts.files.is_empty() {
        eprintln!("umay: `check` requires at least one source file");
        return exit::USAGE;
    }

    let mut any_error = false;
    let mut io_failed = false;
    let color = use_color(opts.color);

    for file in &opts.files {
        let path = Path::new(file);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("umay: cannot read `{file}`: {e}");
                io_failed = true;
                continue;
            }
        };
        let (diags, map) = umayc::analyze_source(path.to_string_lossy().as_ref(), &text);
        if diags.has_errors() {
            any_error = true;
        }
        if opts.format_json {
            println!("{}", render_json(&diags, &map));
        } else {
            let mut rendered = umayc::render_all_full(&diags, &map, color, opts.explain);
            if rendered.is_empty() {
                rendered.push_str(&format!("{}: no diagnostics\n", path.display()));
            }
            print!("{rendered}");
            let (errs, warns) = diags.summary();
            if errs > 0 || warns > 0 {
                eprintln!("{}: {errs} error(s), {warns} warning(s)", path.display());
            }
        }
    }

    if io_failed {
        exit::IO_ERROR
    } else if any_error {
        exit::COMPILE_ERROR
    } else {
        exit::OK
    }
}

/// `umay mir <file>` prints the MIR of a program.
///
/// This is a developer tool: it exists so the lowering can be inspected and so
/// that tests can assert on its shape.
fn cmd_mir(args: &[String]) -> u8 {
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    if opts.files.len() != 1 {
        eprintln!("umay: `mir` takes exactly one source file");
        return exit::USAGE;
    }
    let file = &opts.files[0];
    let path = Path::new(file);
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("umay: cannot read `{file}`: {e}");
            return exit::IO_ERROR;
        }
    };
    match umayc::lower_to_mir_opt(path.to_string_lossy().as_ref(), &text, opts.opt_level) {
        Ok(mir) => {
            print!("{mir}");
            exit::OK
        }
        Err((diags, map)) => {
            let rendered = umayc::render_all_full(&diags, &map, use_color(opts.color), opts.explain);
            eprint!("{rendered}");
            exit::COMPILE_ERROR
        }
    }
}

/// `umay metadata <file>` — what the program declares, as text or JSON.
fn cmd_metadata(args: &[String]) -> u8 {
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    if opts.files.len() != 1 {
        eprintln!("umay: `metadata` takes exactly one source file");
        return exit::USAGE;
    }
    let file = &opts.files[0];
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("umay: cannot read `{file}`: {e}");
            return exit::IO_ERROR;
        }
    };
    match umayc::metadata_source(file.clone(), text) {
        Ok(symbols) => {
            if opts.format_json {
                println!("{}", umayc::metadata::to_json(&symbols));
            } else {
                print!("{}", umayc::metadata::to_text(&symbols));
            }
            exit::OK
        }
        Err((diags, map)) => {
            if opts.format_json {
                println!("{}", render_json(&diags, &map));
            } else {
                let rendered =
                    umayc::render_all_full(&diags, &map, use_color(opts.color), opts.explain);
                eprint!("{rendered}");
            }
            exit::COMPILE_ERROR
        }
    }
}

fn cmd_ast(args: &[String]) -> u8 {
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    let Some(file) = opts.files.first() else {
        eprintln!("umay: `ast` requires a source file");
        return exit::USAGE;
    };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("umay: cannot read `{file}`: {e}");
            return exit::IO_ERROR;
        }
    };
    let (out, map) = umayc::parse_source(file.clone(), text);
    let json = umayc::ast_json(&out.program, &map);
    println!("{json}");
    if out.diagnostics.has_errors() {
        for d in out.diagnostics.items() {
            eprintln!("{}", umayc::diagnostics::render(d, &map, Default::default()).trim_end());
        }
        return exit::COMPILE_ERROR;
    }
    exit::OK
}

fn cmd_explain(args: &[String]) -> u8 {
    let Some(code) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("umay: `explain` requires a diagnostic code, e.g. `umay explain E0101`");
        eprintln!("available codes: {}", umayc::explain::all_codes().join(", "));
        return exit::USAGE;
    };
    match umayc::explain_code(code) {
        Some(entry) => {
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{}: {}\n", entry.code, entry.summary);
            let _ = writeln!(stdout, "{}", wrap(entry.explanation, 78));
            if let Some(example) = entry.example {
                let _ = writeln!(stdout, "\nexample:\n");
                for line in example.lines() {
                    let _ = writeln!(stdout, "    {line}");
                }
            }
            exit::OK
        }
        None => {
            eprintln!("umay: no explanation for code `{code}`");
            eprintln!("available codes: {}", umayc::explain::all_codes().join(", "));
            exit::USAGE
        }
    }
}

fn cmd_doctor(_args: &[String]) -> u8 {
    println!("umay doctor");
    println!("  compiler version : {VERSION}");
    println!("  target           : {}-{}",
        std::env::consts::ARCH,
        std::env::consts::OS);
    println!("  cache directory  : {}", umayc::cache_dir().display());
    let cache_ok = umayc::ensure_cache_dir().is_ok();
    println!("  cache writable   : {}", if cache_ok { "yes" } else { "NO (check permissions)" });
    println!();
    println!("  phase status:");
    for (phase, state) in [
        ("0 architecture docs", "complete"),
        ("1 lexer + parser", "complete"),
        ("2 semantic analysis", "not started"),
        ("3 HIR / MIR", "not started"),
        ("4 ownership + borrow checking", "not started"),
        ("5 x86_64 code generation", "not started"),
        ("6 runtime + standard library", "not started"),
        ("7 developer experience", "not started"),
        ("8 build system + packages", "not started"),
        ("9 stabilization", "not started"),
    ] {
        println!("    {phase:<32} {state}");
    }
    if cache_ok {
        exit::OK
    } else {
        exit::IO_ERROR
    }
}

fn cmd_new(args: &[String]) -> u8 {
    let Some(name) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("umay: `new` requires a project name");
        return exit::USAGE;
    };
    let dir: PathBuf = PathBuf::from(name);
    if dir.exists() {
        eprintln!("umay: `{}` already exists", dir.display());
        return exit::USAGE;
    }
    if let Err(e) = create_project(&dir, name) {
        eprintln!("umay: failed to create project: {e}");
        return exit::IO_ERROR;
    }
    println!("created project `{name}`");
    println!("  umay.toml");
    println!("  src/main.um");
    println!("\nnext: cd {name} && umay check src/main.um");
    exit::OK
}

fn create_project(dir: &Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::create_dir_all(dir.join("dist"))?;
    let manifest = format!(
        "# UMAY project manifest\n\
         # See docs/build-system.md for the full reference.\n\n\
         [package]\n\
         name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2026\"\n\n\
         [build]\n\
         entry = \"src/main.um\"\n\
         output-dir = \"dist\"\n\
         opt-level = 0\n"
    );
    std::fs::write(dir.join("umay.toml"), manifest)?;
    let main = format!(
        "// {name} — created by `umay new`\n\n\
         fn main() {{\n\
         \x20   let greeting = \"Hello, UMAY\"\n\
         \x20   print(greeting)\n\
         }}\n"
    );
    std::fs::write(dir.join("src/main.um"), main)?;
    // Keep dist/ tracked but empty, so the layout is visible from the start.
    std::fs::write(dir.join("dist/.gitkeep"), "")?;
    Ok(())
}

fn cmd_clean(args: &[String]) -> u8 {
    let mut ok = true;
    // Clean the project `dist/` if a manifest is present, and always the cache.
    let manifest = Path::new("umay.toml");
    if manifest.exists() {
        let dist = Path::new("dist");
        if dist.exists() {
            match std::fs::remove_dir_all(dist) {
                Ok(()) => println!("removed dist/"),
                Err(e) => {
                    eprintln!("umay: cannot remove dist/: {e}");
                    ok = false;
                }
            }
        }
        if let Err(e) = std::fs::create_dir_all("dist") {
            eprintln!("umay: cannot recreate dist/: {e}");
            ok = false;
        }
    } else if args.iter().any(|a| !a.starts_with('-')) {
        eprintln!("umay: no umay.toml found here; nothing to clean in the project");
    }
    match umayc::clear_cache() {
        Ok(n) => println!("cleared {n} cache entr{} from {}", if n == 1 { "y" } else { "ies" }, umayc::cache_dir().display()),
        Err(e) => {
            eprintln!("umay: cannot clear cache: {e}");
            ok = false;
        }
    }
    if ok {
        exit::OK
    } else {
        exit::IO_ERROR
    }
}

fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    for para in text.split('\n') {
        if para.trim().is_empty() {
            out.push('\n');
            continue;
        }
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > width {
                out.push_str(&line);
                out.push('\n');
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// build / run / asm
// ---------------------------------------------------------------------------

/// `umay build [options] <file>` — compile to an executable in `dist/`.
///
/// `run` compiles the same way and then executes the result.
/// Prints the warnings a successful build carried out with it.
///
/// A build that succeeds is not silent about a warning it saw: `check` reports
/// these, and a user who runs `build` instead should not get a different story.
fn report_warnings(diags: &Diagnostics, map: &SourceMap, opts: &Options) {
    if diags.items().is_empty() {
        return;
    }
    if opts.format_json {
        println!("{}", render_json(diags, map));
    } else {
        // `check` writes rendered diagnostics to stdout, so `build` does too: a
        // wrapper that captures one command's report has to capture the other's.
        print!("{}", umayc::render_all_full(diags, map, use_color(opts.color), opts.explain));
    }
}

fn cmd_build(args: &[String], run: bool) -> u8 {
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    if opts.files.len() != 1 {
        eprintln!("umay: `{}` takes exactly one source file", if run { "run" } else { "build" });
        return exit::USAGE;
    }
    let file = &opts.files[0];
    let path = Path::new(file);
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("umay: cannot read `{file}`: {e}");
            return exit::IO_ERROR;
        }
    };

    let output = match umayc::codegen_source(path.to_string_lossy().as_ref(), &text, opts.opt_level) {
        Ok(o) => o,
        Err((diags, map)) => {
            // A tool that drives the compiler reads JSON from every command, not
            // just from `check`; a build that fails has to say so in the same
            // shape or the driver needs two parsers.
            if opts.format_json {
                println!("{}", render_json(&diags, &map));
            } else {
                let rendered =
                    umayc::render_all_full(&diags, &map, use_color(opts.color), opts.explain);
                eprint!("{rendered}");
            }
            return exit::COMPILE_ERROR;
        }
    };

    report_warnings(&output.diagnostics, &output.map, &opts);

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    // Artifacts go in `dist/` at the project root; the build never writes inside
    // the source tree beyond that.
    let dist = Path::new("dist");
    if let Err(e) = std::fs::create_dir_all(dist) {
        eprintln!("umay: cannot create `dist/`: {e}");
        return exit::IO_ERROR;
    }
    let asm_path = dist.join(format!("{stem}.s"));
    let exe_path = dist.join(stem);

    if let Err(e) = std::fs::write(&asm_path, &output.assembly) {
        eprintln!("umay: cannot write `{}`: {e}", asm_path.display());
        return exit::IO_ERROR;
    }

    // Assemble. The system assembler turns the text into an object file; the
    // backend stops at text deliberately, so that it never has to encode a
    // relocation itself.
    let obj_path = dist.join(format!("{stem}.o"));
    if let Err(code) = assemble(&asm_path, &obj_path) {
        return code;
    }

    // The runtime is compiled the same way as the program: it is ordinary UMAY
    // source, so there is no separate prebuilt library to keep in step with the
    // compiler.
    let runtime_asm = dist.join("umay_runtime.s");
    let runtime_obj = dist.join("umay_runtime.o");
    let runtime = match umayc::codegen_source_without_prelude("umay_runtime", umayc::RUNTIME_SOURCE, opts.opt_level) {
        Ok(o) => o,
        Err((diags, map)) => {
            // A failure here is a compiler bug, not a problem with the user's
            // program, so it says so instead of blaming the source.
            let rendered = umayc::render_all_full(&diags, &map, use_color(opts.color), opts.explain);
            eprint!("{rendered}");
            eprintln!("umay: internal error: the runtime failed to compile");
            return exit::CODEGEN_ERROR;
        }
    };
    if let Err(e) = std::fs::write(&runtime_asm, &runtime.assembly) {
        eprintln!("umay: cannot write `{}`: {e}", runtime_asm.display());
        return exit::IO_ERROR;
    }
    if let Err(code) = assemble(&runtime_asm, &runtime_obj) {
        return code;
    }

    // Link with the C compiler driver, which knows where the C library and crt
    // objects live. Nothing else from C is used.
    let status = std::process::Command::new("cc")
        .arg("-o")
        .arg(&exe_path)
        .arg(&obj_path)
        .arg(&runtime_obj)
        .arg("-no-pie")
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("umay: linking failed with {s}");
            return exit::CODEGEN_ERROR;
        }
        Err(e) => {
            eprintln!("umay: cannot run `cc`: {e}");
            return exit::IO_ERROR;
        }
    }

    println!("built {}", exe_path.display());

    if run {
        let status = std::process::Command::new(&exe_path).status();
        match status {
            Ok(s) if s.success() => exit::OK,
            Ok(s) => {
                // A non-zero status is the program's own result, not a compiler
                // failure, so it is reported and passed through unchanged.
                if let Some(code) = s.code() {
                    eprintln!("umay: program exited with status {code}");
                    code.clamp(0, 255) as u8
                } else {
                    eprintln!("umay: the program was terminated by a signal");
                    exit::COMPILE_ERROR
                }
            }
            Err(e) => {
                eprintln!("umay: cannot run `{}`: {e}", exe_path.display());
                exit::IO_ERROR
            }
        }
    } else {
        exit::OK
    }
}

/// `umay asm [options] <file>` — print the generated assembly without building.
fn cmd_asm(args: &[String]) -> u8 {
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    if opts.files.len() != 1 {
        eprintln!("umay: `asm` takes exactly one source file");
        return exit::USAGE;
    }
    let file = &opts.files[0];
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("umay: cannot read `{file}`: {e}");
            return exit::IO_ERROR;
        }
    };
    match umayc::codegen_source(file.as_str(), &text, opts.opt_level) {
        Ok(o) => {
            print!("{}", o.assembly);
            exit::OK
        }
        Err((diags, map)) => {
            let rendered = umayc::render_all_full(&diags, &map, use_color(opts.color), opts.explain);
            eprint!("{rendered}");
            exit::COMPILE_ERROR
        }
    }
}

/// Runs the assembler on one file, reporting a failure as an exit code.
fn assemble(source: &Path, object: &Path) -> Result<(), u8> {
    let status = std::process::Command::new("as")
        .arg("--64")
        .arg("-o")
        .arg(object)
        .arg(source)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            eprintln!(
                "umay: the assembler failed with {s}\n  generated assembly: {}",
                source.display()
            );
            Err(exit::CODEGEN_ERROR)
        }
        Err(e) => {
            eprintln!("umay: cannot run `as`: {e}");
            Err(exit::IO_ERROR)
        }
    }
}
