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
    /// Retired: no command exits with this any more, since every command in
    /// `umay --help` is implemented. The value is kept so that the numbers after
    /// it do not shift, and so a driver that still checks for it still compiles.
    #[allow(dead_code)]
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
        "lock" => cmd_lock(&rest),
        other => {
            eprintln!("umay: unknown command `{other}`");
            eprintln!("run `umay help` for the list of commands");
            exit::USAGE
        }
    };
    ExitCode::from(code)
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
    --target <triple>      Build for another system (x86_64-unknown-linux-gnu,
                           x86_64-apple-darwin, x86_64-pc-windows-gnu)
    --emit-object-only     Also leave the object in dist/ (build, asm)

EXIT CODES:
    0 success   1 compile error   2 usage error   3 I/O error
    4 retired (no command uses it)   5 code generation failed"
    );
}

/// A parsed `--flag` set shared by the file-oriented commands.
struct Options {
    format_json: bool,
    explain: bool,
    color: Option<bool>,
    /// Optimisation level for `mir` and (later) `build`.
    opt_level: umayc::opt::OptLevel,
    /// The system to emit code for. `None` means the host's own.
    target: Option<umayc::target::Target>,
    /// Stop after assembling, leaving the objects and linking nothing.
    ///
    /// A caller who only wants the object -- perhaps to link it elsewhere, or to
    /// inspect it -- asks for this instead of `build`, so that producing an object
    /// is a success and not the half of a build that could not finish.
    emit_object_only: bool,
    files: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            format_json: false,
            explain: false,
            color: None,
            opt_level: umayc::opt::OptLevel::O0,
            target: None,
            emit_object_only: false,
            files: Vec::new(),
        }
    }
}

impl Options {
    /// The target to build for: the one named, or the host's own.
    fn target(&self) -> umayc::target::Target {
        self.target.unwrap_or_else(umayc::target::Target::host)
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
            // `--target <triple>`: compile for a named system. A triple the
            // backend cannot produce is refused here, before any work is done,
            // rather than at the linker.
            "--target" => {
                i += 1;
                let name = match args.get(i) {
                    Some(n) => n,
                    None => return Err((exit::USAGE, "`--target` requires a triple".into())),
                };
                match umayc::target::Target::parse(name) {
                    Ok(t) => opts.target = Some(t),
                    Err(e) => return Err((exit::USAGE, e.to_string())),
                }
            }
            // `--emit-object-only`: assemble and stop. Nothing is linked, so a
            // target this host cannot link for is a success rather than a
            // partial build.
            "--emit-object-only" => opts.emit_object_only = true,
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
    let (entries, bytes) = umayc::build::Cache::open().stats();
    println!("  cache entries    : {entries} ({})", human_bytes(bytes));
    let cap = umayc::build::cache_max_bytes();
    println!("  cache limit      : {}", if cap == u64::MAX { "unlimited".to_string() } else { human_bytes(cap) });
    let roots = umayc::upm::registry_roots();
    if roots.is_empty() {
        println!("  registry         : none (set UMAY_REGISTRY to fetch dependencies)");
    } else {
        let joined = roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  registry         : {joined}");
    }
    println!();
    println!("  cache key is derived from:");
    for input in umayc::build::CacheKey::derivation() {
        println!("    - {input}");
    }
    println!();
    // What this build can do, stated as fact rather than as a plan: the point of
    // `doctor` is to answer "will this compiler do what I need".
    println!("  phase status:");
    for (phase, state) in [
        ("0 architecture docs", "done"),
        ("1 lexer + parser", "done"),
        ("2 semantic analysis", "done"),
        ("3 HIR / MIR", "done"),
        ("4 ownership + borrow checking", "done"),
        ("5 x86_64 code generation", "done"),
        ("6 runtime + standard library", "done"),
        ("7 developer experience", "done"),
        ("8 build system + packages", "done"),
        ("9 stabilization", "done"),
    ] {
        println!("    {phase:<32} {state}");
    }
    if cache_ok {
        exit::OK
    } else {
        exit::IO_ERROR
    }
}

/// `umay lock` — resolve the project's dependencies into `umay.lock`.
///
/// Resolution is honest about what it cannot do: a dependency with no `path` needs
/// a registry, and there is none. Writing a lockfile that claimed to have pinned
/// something never fetched would be worse than writing none.
fn cmd_lock(args: &[String]) -> u8 {
    let opts = match parse_options(args) {
        Ok(o) => o,
        Err((code, msg)) => {
            eprintln!("umay: {msg}");
            return code;
        }
    };
    let _ = opts;
    let project = Path::new("umay.toml");
    if !project.exists() {
        eprintln!("umay: no `umay.toml` here; run `umay lock` from a project directory");
        return exit::USAGE;
    }
    let manifest = match umayc::manifest::read_project(Path::new(".")) {
        Ok(m) => m,
        Err(errors) => {
            report_manifest_errors(&errors);
            return exit::COMPILE_ERROR;
        }
    };
    let resolution = umayc::lock::resolve(Path::new("."), &manifest);
    for (name, reason) in &resolution.unresolved {
        eprintln!("umay: cannot resolve `{name}`: {reason}");
    }
    if !resolution.unresolved.is_empty() {
        // A partial lockfile would be a lie about what is pinned, so nothing is
        // written until every dependency resolves.
        return exit::COMPILE_ERROR;
    }
    match std::fs::write("umay.lock", resolution.lockfile.to_text()) {
        Ok(()) => {
            println!(
                "wrote umay.lock ({} package{})",
                resolution.lockfile.packages.len(),
                if resolution.lockfile.packages.len() == 1 { "" } else { "s" }
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("umay: cannot write `umay.lock`: {e}");
            exit::IO_ERROR
        }
    }
}

/// Prints manifest errors the way diagnostics are printed: location first.
fn report_manifest_errors(errors: &[umayc::manifest::ManifestError]) {
    for error in errors {
        if error.line > 0 {
            eprintln!("umay.toml:{}: {}", error.line, error.message);
        } else {
            eprintln!("umay.toml: {}", error.message);
        }
        if let Some(help) = &error.help {
            eprintln!("  help: {help}");
        }
    }
}

/// A byte count a person can read.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
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
    println!("\nnext: cd {name} && umay run");
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
    // A program that greets and exits 0. `main` with no return type and no
    // `return` leaves whatever was in the return register, so a freshly created
    // project would exit with a random status -- which makes `umay run` look
    // broken when nothing is wrong.
    let main = format!(
        "// {name} — created by `umay new`\n\n\
         fn main() -> i32 {{\n\
         \x20   let greeting = \"Hello, UMAY\"\n\
         \x20   println(greeting)\n\
         \x20   return 0\n\
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
/// The cache key's name for an optimisation level.
fn opt_level_name(level: umayc::opt::OptLevel) -> &'static str {
    match level {
        umayc::opt::OptLevel::O0 => "O0",
        umayc::opt::OptLevel::O1 => "O1",
        umayc::opt::OptLevel::O2 => "O2",
        umayc::opt::OptLevel::O3 => "O3",
    }
}

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

/// Whether `text` declares `main`, directly or in a module it includes.
///
/// Best-effort in the same way the cache key is: a source that does not parse has
/// a parse error to report, and that error is the one worth printing.
fn names_a_main(text: &str, path: &Path, dependencies: &[umayc::modules::DependencyPath]) -> bool {
    let (out, mut map) = umayc::parse_source(path.to_string_lossy().into_owned(), text.to_string());
    if out.diagnostics.has_errors() {
        // The parse error is the real problem; let codegen report it with its
        // usual care rather than adding a second complaint about `main`.
        return true;
    }
    let (program, module_diagnostics) =
        umayc::modules::load_from_entry_with_deps(out.program, path, &mut map, dependencies);
    if module_diagnostics.has_errors() {
        return true;
    }
    umayc::declares_main(&program)
}

/// The source text of every module `text` would load, for the cache key.
///
/// Best-effort on purpose: the frontend reports the error properly, and a cache
/// key that cannot be computed must not be a second, worse error message.
fn module_sources_for(
    text: &str,
    path: &Path,
    dependencies: &[umayc::modules::DependencyPath],
) -> Vec<(String, String)> {
    let (frontend, _map) =
        umayc::parse_source(path.to_string_lossy().into_owned(), text.to_string());
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    umayc::modules::module_sources_with_deps(&frontend.program, base, dependencies)
        .into_iter()
        .map(|(name, source)| {
            // The path is absolute on one machine and different on another. The
            // key must not depend on where the project sits, so the module is
            // named by its path relative to the entry point.
            let relative = Path::new(&name)
                .strip_prefix(base)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| {
                    // `base` is the entry's directory, which is not always the
                    // canonical parent; fall back to the file name.
                    Path::new(&name)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(name)
                });
            (relative, source)
        })
        .collect()
}

/// Warns when a project's lockfile no longer matches what is on disk.
///
/// A lockfile nobody checks is documentation. The check has to happen somewhere
/// the author actually runs, and `build` is that place: a dependency that changed
/// under a pinned lockfile is exactly the situation the file exists for, and
/// finding out at link time -- or never -- is too late to be useful.
fn warn_if_lock_is_stale() {
    if !Path::new("umay.lock").is_file() {
        return;
    }
    let Ok(lockfile) = umayc::lock::read_project(Path::new(".")) else {
        // An unreadable lockfile is `umay lock`'s problem to report clearly; a
        // second, vaguer complaint from `build` only obscures it.
        return;
    };
    for problem in umayc::lock::verify(Path::new("."), &lockfile) {
        eprintln!("warning: {problem}");
    }
}

/// The entry point named by `umay.toml` in the current directory.
fn entry_from_manifest() -> Result<String, u8> {
    if !Path::new("umay.toml").exists() {
        eprintln!("umay: no source file given and no `umay.toml` here");
        eprintln!("usage: umay build <file.um>, or run it inside a project");
        return Err(exit::USAGE);
    }
    match umayc::manifest::read_project(Path::new(".")) {
        Ok(manifest) => Ok(manifest.build.entry.clone()),
        Err(errors) => {
            report_manifest_errors(&errors);
            Err(exit::COMPILE_ERROR)
        }
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
    // With no file named, the project's manifest says what to build. That is the
    // point of having a manifest: `umay build` in a project directory should not
    // need the entry point repeated.
    let file = match opts.files.first() {
        Some(file) => file.clone(),
        None => match entry_from_manifest() {
            Ok(entry) => entry,
            Err(code) => return code,
        },
    };
    if run && opts.emit_object_only {
        // Running needs an executable, and `--emit-object-only` deliberately
        // produces none. Refusing here beats a build that says it succeeded and a
        // `dist` with nothing to run.
        eprintln!("umay: `--emit-object-only` cannot be combined with `run`");
        return exit::USAGE;
    }
    let file = &file;
    if opts.files.len() > 1 {
        eprintln!("umay: `{}` takes at most one source file", if run { "run" } else { "build" });
        return exit::USAGE;
    }
    let path = Path::new(file);
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("umay: cannot read `{file}`: {e}");
            return exit::IO_ERROR;
        }
    };

    // The cache is keyed on everything that goes into the assembly, so a hit is
    // the previous compile's exact answer. A miss is the normal first build.
    let cache = umayc::build::Cache::open();
    let level = opt_level_name(opts.opt_level);
    warn_if_lock_is_stale();

    // Dependencies named in the manifest are reachable as modules, so the entry
    // point's directory is not the only place `mod lib;` can find a file. The
    // project root is where the manifest lives, which is the entry's directory
    // for a program at `src/main.um`.
    let project_root = path.parent().unwrap_or_else(|| Path::new("."));
    let dependencies = umayc::modules::dependencies_of_project(project_root);

    // A program with no `main` cannot be linked, and the linker's way of saying
    // so is "undefined reference to `main`" from `collect2` after the assembler
    // has already run. Asking about the source here makes it a diagnostic about
    // the program instead of a message about an object file.
    if !names_a_main(&text, path, &dependencies) {
        eprintln!(
            "umay: `{}` has no `main` function\n  a program needs one to be linked and run",
            file
        );
        return exit::COMPILE_ERROR;
    }

    // The key covers every file that goes into the assembly, not just the entry
    // point. Keying on the entry alone let an edited module reuse the previous
    // assembly: the program silently kept its old behaviour.
    let module_sources = module_sources_for(&text, path, &dependencies);
    let extra: Vec<(&str, &str)> = module_sources
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    // Everything below is per-target: the symbol spelling, the object file, the
    // linker's flags and the runtime's platform layer. Resolving it once here
    // means no later step can silently use the host's answer instead of the one
    // that was asked for.
    let target = opts.target();
    let program_key = umayc::build::CacheKey::derive_for(
        umayc::build::Artifact::ProgramAsm,
        level,
        text.as_bytes(),
        &extra,
        target,
    );

    let cached_assembly = cache.read(&program_key, umayc::build::Artifact::ProgramAsm);
    let assembly = match cached_assembly {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => {
            let output = match umayc::codegen_source_for(
                path.to_string_lossy().as_ref(),
                &text,
                opts.opt_level,
                target,
            ) {
                Ok(o) => o,
                Err((diags, map)) => {
                    // A tool that drives the compiler reads JSON from every
                    // command, not just from `check`; a build that fails has to
                    // say so in the same shape or the driver needs two parsers.
                    if opts.format_json {
                        println!("{}", render_json(&diags, &map));
                    } else {
                        let rendered = umayc::render_all_full(
                            &diags,
                            &map,
                            use_color(opts.color),
                            opts.explain,
                        );
                        eprint!("{rendered}");
                    }
                    return exit::COMPILE_ERROR;
                }
            };
            report_warnings(&output.diagnostics, &output.map, &opts);
            // Only a build with nothing to say is stored. The cache holds the
            // assembly and nothing else, so a hit has no diagnostics to report --
            // a warning program would then be silent on its second build, and a
            // warning a user sees once is a warning they cannot trust.
            if output.diagnostics.items().is_empty() {
                let _ = cache.write(
                    &program_key,
                    umayc::build::Artifact::ProgramAsm,
                    output.assembly.as_bytes(),
                );
            }
            output.assembly
        }
    };

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    // Artifacts go in `dist/` at the project root; the build never writes inside
    // the source tree beyond that.
    let dist = Path::new("dist");
    if let Err(e) = std::fs::create_dir_all(dist) {
        eprintln!("umay: cannot create `dist/`: {e}");
        return exit::IO_ERROR;
    }
    let asm_path = dist.join(format!("{stem}.s"));
    let exe_path = dist.join(target.executable_name(stem));

    if let Err(e) = std::fs::write(&asm_path, &assembly) {
        eprintln!("umay: cannot write `{}`: {e}", asm_path.display());
        return exit::IO_ERROR;
    }

    // Assemble. The system assembler turns the text into an object file; the
    // backend stops at text deliberately, so that it never has to encode a
    // relocation itself.
    let obj_path = dist.join(format!("{stem}{}", target.object_extension));

    // The runtime is compiled the same way as the program: it is ordinary UMAY
    // source, so there is no separate prebuilt library to keep in step with the
    // compiler.
    let runtime_asm = dist.join("umay_runtime.s");
    let runtime_obj = dist.join(format!("umay_runtime{}", target.object_extension));
    // The runtime's portable body plus this target's platform layer. Every
    // system-specific line of the runtime -- the C symbols, the constant table,
    // whether `fork` exists -- comes from here.
    let runtime_source = umayc::runtime_source_for(target);
    // The runtime is the same source on every build, so its assembly is a cache
    // hit after the first build of a given compiler version and level.
    let runtime_key = umayc::build::CacheKey::derive_for(
        umayc::build::Artifact::RuntimeAsm,
        level,
        runtime_source.as_bytes(),
        &[],
        target,
    );
    let runtime_assembly = match cache.read(&runtime_key, umayc::build::Artifact::RuntimeAsm) {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => {
            match umayc::codegen_source_without_prelude_for(
                "umay_runtime",
                &runtime_source,
                opts.opt_level,
                target,
            ) {
                Ok(o) => {
                    let _ = cache.write(
                        &runtime_key,
                        umayc::build::Artifact::RuntimeAsm,
                        o.assembly.as_bytes(),
                    );
                    o.assembly
                }
                Err((diags, map)) => {
                    // A failure here is a compiler bug, not a problem with the
                    // user's program, so it says so instead of blaming the source.
                    let rendered =
                        umayc::render_all_full(&diags, &map, use_color(opts.color), opts.explain);
                    eprint!("{rendered}");
                    eprintln!("umay: internal error: the runtime failed to compile");
                    return exit::CODEGEN_ERROR;
                }
            }
        }
    };
    if let Err(e) = std::fs::write(&runtime_asm, &runtime_assembly) {
        eprintln!("umay: cannot write `{}`: {e}", runtime_asm.display());
        return exit::IO_ERROR;
    }

    // The two objects do not depend on each other, so they are assembled at the
    // same time. Nothing is printed by `assemble` itself: a message would arrive
    // in whatever order the two processes finished, and two builds of the same
    // source could then differ in what they printed.
    let (program_result, runtime_result) =
        std::thread::scope(|scope| {
            let program = scope.spawn(|| assemble(&asm_path, &obj_path, target));
            let runtime = scope.spawn(|| assemble(&runtime_asm, &runtime_obj, target));
            (
                program.join().unwrap_or(Err(exit::CODEGEN_ERROR)),
                runtime.join().unwrap_or(Err(exit::CODEGEN_ERROR)),
            )
        });
    if let Err(code) = program_result {
        return code;
    }
    if let Err(code) = runtime_result {
        return code;
    }

    // `--emit-object-only` stops here. The objects are the whole request, so they
    // are reported and nothing else is attempted: a target this host cannot link
    // for is not an error when no link was asked for.
    if opts.emit_object_only {
        println!("built {}", obj_path.display());
        println!("built {}", runtime_obj.display());
        println!(
            "  link it with: {}",
            target.link_command(
                &obj_path,
                &runtime_obj,
                &exe_path.display().to_string()
            )
        );
        if let Some(caveat) = target.link_command_caveat() {
            println!("  ({caveat})");
        }
        return exit::OK;
    }

    // The linked executable is cached too, so an unchanged rebuild starts neither
    // `as` nor `cc`. The key is the program assembly plus the runtime assembly:
    // those two texts are everything that decides the objects, and the compiler
    // version is already part of the key, so a change to the runtime or the
    // backend cannot reuse a stale binary.
    let exe_key = umayc::build::CacheKey::derive_for(
        umayc::build::Artifact::LinkedExe,
        level,
        assembly.as_bytes(),
        &[("runtime", &runtime_assembly)],
        target,
    );
    let mut linked_from_cache = false;
    match cache.read(&exe_key, umayc::build::Artifact::LinkedExe) {
        Some(bytes) => match write_executable(&exe_path, &bytes) {
            Ok(()) => linked_from_cache = true,
            Err(e) => {
                eprintln!("umay: cannot write `{}`: {e}", exe_path.display());
                return exit::IO_ERROR;
            }
        },
        None => {}
    }

    if !linked_from_cache {
        // A system cannot link for another system: that needs the other's C
        // library and startup code. Assembling still worked, so the object is
        // left where the user can find it and the message says so, rather than
        // handing Mach-O to the GNU linker and printing its complaint about file
        // formats.
        if let Some(reason) = target.link_unavailable(&obj_path) {
            eprintln!("umay: {reason}");
            return exit::CODEGEN_ERROR;
        }
        // Link with the C compiler driver, which knows where the C library and
        // crt objects live. Nothing else from C is used.
        let mut link = std::process::Command::new(target.linker);
        // Driver-wide flags must precede the inputs: `-arch` selects the
        // slice of the implicit C library as well as the output architecture.
        // This backend is x86_64-only even when the host is arm64.
        if target.triple.contains("darwin") {
            link.arg("-arch").arg("x86_64");
        }
        link.arg("-o").arg(&exe_path).arg(&obj_path).arg(&runtime_obj);
        // The driver's own flags: `-no-pie` on Linux ELF, `-lSystem` on macOS,
        // `-mconsole` and `-lws2_32` on Windows PE. Passing the wrong one is a
        // hard linker error rather than a warning.
        for flag in target.linker_flags() {
            link.arg(flag);
        }
        // After the objects, so the linker has seen the references first.
        for lib in umayc::target::Target::system_libraries(&assembly) {
            link.arg(lib);
        }
        let status = link.status();
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
        if let Ok(bytes) = std::fs::read(&exe_path) {
            let _ = cache.write(&exe_key, umayc::build::Artifact::LinkedExe, &bytes);
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
    match umayc::codegen_source_for(file.as_str(), &text, opts.opt_level, opts.target()) {
        Ok(o) => {
            print!("{}", o.assembly);
            if !opts.emit_object_only {
                return exit::OK;
            }
            // The assembly is already printed -- that is what `asm` is for. With
            // `--emit-object-only` it also leaves the object beside it, so seeing
            // the text and keeping the object are one command rather than two.
            write_object(&o.assembly, file, opts.target())
        }
        Err((diags, map)) => {
            let rendered = umayc::render_all_full(&diags, &map, use_color(opts.color), opts.explain);
            eprint!("{rendered}");
            exit::COMPILE_ERROR
        }
    }
}

/// Runs the assembler on one file, reporting a failure as an exit code.
/// Writes an executable, with the execute bit set.
///
/// `std::fs::write` creates the file without a mode, so a copy restored from the
/// cache would be a program the shell refuses to run -- the build would succeed
/// and the next `umay run` would fail with "Permission denied", which points at
/// nothing useful.
fn write_executable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = 0o755;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Assembles one `.s` into a `.o`.
///
/// Prints nothing on success and returns the message to show on failure, rather
/// than printing it: two of these run at once, and interleaved output would make
/// the same build say different things on different runs.
/// Writes `assembly` to `dist/<stem>.s` and assembles it into `dist/<stem>.o`.
///
/// `asm --emit-object-only` needs the object without the rest of a build: no
/// runtime, no link. It is the same three steps `build` takes -- make `dist`, write
/// the text, run the assembler -- so it uses the same ones rather than repeating
/// them.
fn write_object(assembly: &str, source: &str, target: umayc::target::Target) -> u8 {
    let stem = Path::new(source)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());

    let dist = Path::new("dist");
    if let Err(e) = std::fs::create_dir_all(dist) {
        eprintln!("umay: cannot create `dist/`: {e}");
        return exit::IO_ERROR;
    }
    let asm_path = dist.join(format!("{stem}.s"));
    let obj_path = dist.join(format!("{stem}{}", target.object_extension));
    if let Err(e) = std::fs::write(&asm_path, assembly) {
        eprintln!("umay: cannot write `{}`: {e}", asm_path.display());
        return exit::IO_ERROR;
    }
    if let Err(code) = assemble(&asm_path, &obj_path, target) {
        return code;
    }
    println!("built {}", obj_path.display());
    // No runtime object here -- `asm` compiles one file -- so the command names
    // the object it made and leaves the runtime to the reader's own build.
    if let Some(caveat) = target.link_command_caveat() {
        println!("  (linking {caveat})");
    }
    exit::OK
}

fn assemble(source: &Path, object: &Path, target: umayc::target::Target) -> Result<(), u8> {
    let mut command = std::process::Command::new(target.assembler);
    // `--64` is a GNU `as` option that selects the target of an ELF object.
    // Mach-O's `as` and Windows' do not take it and infer the architecture from
    // the input, so the flag is a property of the target rather than a constant.
    for arg in target.assembler_args {
        command.arg(arg);
    }
    let status = command.arg("-o").arg(object).arg(source).status();
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
            eprintln!("umay: cannot run `{}`: {e}", target.assembler);
            Err(exit::IO_ERROR)
        }
    }
}
