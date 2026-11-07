//! Command-line interface for the `umay` compiler driver.
//!
//! Design constraints (from `docs/build-system.md`):
//! * exit codes are stable and documented,
//! * `--format json` produces the versioned diagnostic schema on stdout and
//!   nothing else,
//! * no command ever writes compiler temporaries into the project directory.

use std::io::Write;

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
        "build" => not_yet("build", "Phase 5 (custom x86_64 backend)"),
        "run" => not_yet("run", "Phase 5 (custom x86_64 backend)"),
        "fmt" => not_yet("fmt", "Phase 7 (developer experience)"),
        "lint" => not_yet("lint", "Phase 7 (developer experience)"),
        "lsp" => not_yet("lsp", "Phase 7 (developer experience)"),
        other => {
            eprintln!("umay: unknown command `{other}`");
            eprintln!("run `umay help` for the list of commands");
            exit::USAGE
        }
    };
    ExitCode::from(code)
}

fn not_yet(cmd: &str, phase: &str) -> u8 {
    eprintln!("umay: `{cmd}` is not implemented yet (planned for {phase}).");
    eprintln!("This build implements Phase 1: lexing and parsing. See ROADMAP.md.");
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
    ast <file>         Print the parsed syntax tree
    explain <CODE>     Explain a diagnostic code (e.g. umay explain E0101)
    doctor             Report toolchain and environment information
    new <name>         Create a new UMAY project
    clean              Remove build artifacts (dist/)
    version            Print the compiler version

OPTIONS:
    --format human|json    Diagnostic output format (default: human)
    --color auto|always|never
    --explain              Include the full explanation under each diagnostic

PLANNED (not in this build):
    build, run, fmt, lint, lsp

EXIT CODES:
    0 success   1 compile error   2 usage error   3 I/O error   4 not implemented"
    );
}

/// A parsed `--flag` set shared by the file-oriented commands.
#[derive(Default)]
struct Options {
    format_json: bool,
    explain: bool,
    color: Option<bool>,
    files: Vec<String>,
}

fn parse_options(args: &[String]) -> Result<Options, (u8, String)> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
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
        let (diags, map) = umayc::check_source(path.to_string_lossy().as_ref(), &text);
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
