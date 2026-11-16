//! Cross-target tests: one source, three systems.
//!
//! The frontend, the MIR, the borrow checker and the optimiser are identical for
//! every target, so what these tests check is the layer underneath: that the
//! symbol spelling, the object-file dressing, the argument registers and the
//! runtime's platform layer all follow the target that was asked for, and that
//! the result assembles into the right *kind* of object.
//!
//! They do not link or run a non-host executable: this machine has no Apple SDK
//! or MinGW runtime, and a test that required one would not run anywhere else.
//! What they do verify is the part that would otherwise fail silently -- that the
//! text the backend emits is accepted by a real assembler for that object format
//! and that the entry symbol is defined where the linker will look for it.
//!
//! Each test is skipped, not failed, when the assembler is missing.

use std::process::Command;

fn compiler() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

/// True when `clang` is present, which is the cross assembler for both
/// non-Linux targets.
fn have_clang() -> bool {
    Command::new("clang")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A program that exercises a call, a stack argument and a string literal, so
/// that the assembly contains something of every shape a target can get wrong.
const PROGRAM: &str = r#"
fn add(a: i32, b: i32) -> i32 {
    return a + b
}

fn main() -> i32 {
    let mut total = 0
    let mut i = 0
    while i < 4 {
        total = add(total, i)
        i = i + 1
    }
    println("cross")
    println_i64(total)
    return 0
}
"#;

/// Writes the program to a temporary directory and returns that directory.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_cross_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    std::fs::write(dir.join("main.um"), PROGRAM).expect("write program");
    dir
}

/// The assembly this compiler emits for one target.
fn assembly_for(dir: &std::path::Path, target: &str) -> String {
    let out = Command::new(compiler())
        .current_dir(dir)
        .arg("asm")
        .arg("--target")
        .arg(target)
        .arg("main.um")
        .output()
        .expect("run umay asm");
    assert!(
        out.status.success(),
        "umay asm --target {target} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn macos_assembly_is_a_valid_macho_object() {
    if !have_clang() {
        eprintln!("skip macos_assembly_is_a_valid_macho_object: clang not found");
        return;
    }
    let dir = scratch("macos");
    let asm = assembly_for(&dir, "x86_64-apple-darwin");

    // The symbol spelling is the visible difference from Linux: every external
    // name carries the Mach-O underscore, including the entry point the linker
    // looks for.
    assert!(asm.contains("_main:"), "macOS assembly must define `_main`:\n{asm}");
    assert!(asm.contains(".globl _main"), "macOS assembly must export `_main`");
    // ELF-only metadata must be absent: Mach-O's assembler rejects `.type` and
    // `.size` rather than ignoring them.
    assert!(!asm.contains(".type "), "Mach-O rejects `.type`");
    assert!(!asm.contains(".size "), "Mach-O rejects `.size`");
    assert!(!asm.contains(".note.GNU-stack"), "the ELF stack note is not Mach-O");
    // Data lives in a named segment/section pair on Mach-O.
    if asm.contains(".asciz") {
        assert!(
            asm.contains(".section __TEXT,__const"),
            "string literals must go in __TEXT,__const on Mach-O"
        );
    }

    // The real check: hand it to an assembler that understands Mach-O.
    let src = dir.join("main.s");
    std::fs::write(&src, &asm).expect("write asm");
    let obj = dir.join("main.o");
    let out = Command::new("clang")
        .arg("--target=x86_64-apple-darwin")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run clang");
    assert!(
        out.status.success(),
        "clang rejected the macOS assembly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&obj).expect("read object");
    // Mach-O 64-bit magic, little-endian: `cffaedfe`.
    assert_eq!(&bytes[..4], &[0xcf, 0xfa, 0xed, 0xfe], "not a Mach-O 64 object");
}

/// A macOS object is linkable even though it is never linked here.
///
/// There is no Apple SDK on this host, so `ld64` cannot run and the usual proof
/// -- build it, run it, read the answer -- is unavailable. What can still be
/// checked is everything the linker will look at: the object is well formed, the
/// entry point it looks for is defined and global, and every name the object
/// leaves undefined is one UMAY's own runtime supplies rather than something
/// Apple was expected to.
///
/// That last part is the one worth having. A misspelt C symbol is invisible until
/// a Mac fails to link, and it is exactly the class of fault that cost four
/// separate bugs on Windows.
#[test]
fn a_macos_object_leaves_only_its_own_runtime_undefined() {
    if !have_clang() || !have_llvm_nm() {
        eprintln!("skip a_macos_object_leaves_only_its_own_runtime_undefined: no toolchain");
        return;
    }
    let dir = scratch("macos-linkable");
    let asm = assembly_for(&dir, "x86_64-apple-darwin");
    let program = match assemble_macho(&dir, &asm, "main") {
        Some(object) => object,
        None => return,
    };
    // The very same program built for the host, so the runtime object exists to
    // compare against.
    let runtime = match runtime_object_for_macos(&dir) {
        Some(object) => object,
        None => return,
    };

    // The entry point must be defined and exported: `ld64` looks for `_main` by
    // name and will not accept a private symbol.
    let defined = defined_symbols(&program);
    assert!(
        defined.contains(&"_main".to_string()),
        "`_main` must be defined and global, found: {defined:?}"
    );

    // Every name the program leaves undefined must be one the runtime defines.
    // A C library name appearing here is either a typo or a binding that no
    // longer exists.
    let runtime_defined = defined_symbols(&runtime);
    for name in undefined_symbols(&program) {
        assert!(
            runtime_defined.contains(&name),
            "`{name}` is undefined in the program object and is not something the \
             runtime defines -- on a Mac this is a link error"
        );
    }

    // The runtime in turn may leave C library calls undefined, but not UMAY's
    // own: a reference there is only satisfied by the standard library, which is
    // compiled into the program, so the program must define it.
    for name in undefined_symbols(&runtime) {
        if name.starts_with("_umay_") {
            assert!(
                defined.contains(&name),
                "the runtime calls `{name}`, which neither it nor the program defines"
            );
        }
    }

    // A C library name in the runtime is checked against the list of names macOS
    // actually has. Without this a misspelt binding -- `wrote` for `write` -- is
    // undefined but looks UMAY's own, and nothing would notice until a Mac failed
    // to link. That is precisely how four faults survived on Windows.
    const LIBC_ON_MACOS: &[&str] = &[
        "__error", "close", "exit", "fork", "free", "getenv", "lseek", "malloc", "mkdir", "open",
        "pipe", "read", "rename", "unlink", "waitpid", "write", "realloc", "memcpy", "memset",
        "strlen", "abort",
    ];
    for name in undefined_symbols(&runtime) {
        if name.starts_with("_umay_") || name.starts_with("_main") {
            continue;
        }
        // Mach-O prefixes C names with an underscore; the list is written bare.
        let bare = name.strip_prefix('_').unwrap_or(&name);
        assert!(
            LIBC_ON_MACOS.contains(&bare),
            "the runtime refers to `{name}`, which is not a C library function macOS \
             provides -- a misspelt binding is a link error and nothing else catches it"
        );
    }
}

/// True when `llvm-nm` is present, which reads a Mach-O object's symbol table.
fn have_llvm_nm() -> bool {
    Command::new("llvm-nm")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Assembles `asm` into a Mach-O object named `stem.o`. Returns its path.
fn assemble_macho(dir: &std::path::Path, asm: &str, stem: &str) -> Option<std::path::PathBuf> {
    let src = dir.join(format!("{stem}.s"));
    std::fs::write(&src, asm).expect("write asm");
    let obj = dir.join(format!("{stem}.o"));
    let out = Command::new("clang")
        .arg("--target=x86_64-apple-darwin")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run clang");
    assert!(
        out.status.success(),
        "clang rejected the macOS assembly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(obj)
}

/// The runtime, assembled for macOS from the source the compiler itself uses.
fn runtime_object_for_macos(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    // The runtime is not a UMAY program -- there is nothing to run -- so it is
    // taken from the compiler's own view of it rather than from a build, which
    // would try to link a binary for a system this host cannot link for.
    let source = umayc::runtime_source_for(umayc::target::Target::MACOS);
    let asm = umayc::codegen_source_without_prelude_for(
        "umay_runtime",
        &source,
        umayc::opt::OptLevel::O0,
        umayc::target::Target::MACOS,
    );
    let Ok(out) = asm else {
        eprintln!("skip: the runtime did not compile for macOS");
        return None;
    };
    assemble_macho(dir, &out.assembly, "runtime")
}

/// Every symbol `llvm-nm` reports as defined in `object`.
fn defined_symbols(object: &std::path::Path) -> Vec<String> {
    let out = Command::new("llvm-nm")
        .arg("-g")
        .arg("--defined-only")
        .arg(object)
        .output()
        .expect("run llvm-nm");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().last().map(str::to_string))
        .collect()
}

/// Every symbol `llvm-nm` reports as undefined in `object`.
///
/// The output shape differs by format: a Mach-O symbol table prints the bare name,
/// while a COFF one prints `U` and a space first. `--just-symbol-name` asks for the
/// name alone, which is what both callers want.
fn undefined_symbols(object: &std::path::Path) -> Vec<String> {
    let out = Command::new("llvm-nm")
        .arg("--undefined-only")
        .arg("--just-symbol-name")
        .arg(object)
        .output()
        .expect("run llvm-nm");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn windows_assembly_is_a_valid_coff_object_using_win64_registers() {
    if !have_clang() {
        eprintln!("skip windows_assembly_is_a_valid_coff_object_using_win64_registers: clang not found");
        return;
    }
    let dir = scratch("windows");
    let asm = assembly_for(&dir, "x86_64-pc-windows-gnu");

    assert!(asm.contains("_main:"), "Windows assembly must define `_main`");
    assert!(!asm.contains(".type "), "COFF rejects `.type`");
    assert!(!asm.contains(".note.GNU-stack"), "the ELF stack note is not COFF");
    // Win64 passes the first argument in `%rcx`. A `%rdi` prologue here means the
    // SysV register list was used, which passes every argument to the wrong
    // place.
    assert!(
        asm.contains("movq %rcx") || asm.contains("movl %ecx") || asm.contains("movw %cx") || asm.contains("movb %cl"),
        "Win64 arguments arrive in `%rcx`; the SysV list would use `%rdi`:\n{}",
        &asm[..asm.len().min(2000)]
    );
    // The `rdi`/`rsi` argument moves of SysV must not appear for incoming
    // arguments. They may still appear as ordinary allocator choices, so the
    // check is on the prologue's argument stores specifically: a Win64 prologue
    // never reads `%rdi` for a parameter.
    assert!(
        !asm.contains("movq %rdi, -8(%rbp)"),
        "a Win64 prologue must not take its first parameter from `%rdi`"
    );

    let src = dir.join("main.s");
    std::fs::write(&src, &asm).expect("write asm");
    let obj = dir.join("main.o");
    let out = Command::new("clang")
        .arg("--target=x86_64-pc-windows-gnu")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run clang");
    assert!(
        out.status.success(),
        "clang rejected the Windows assembly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // COFF on x86-64 starts with the machine type 0x8664 in the first two bytes.
    let bytes = std::fs::read(&obj).expect("read object");
    assert_eq!(&bytes[..2], &[0x64, 0x86], "not an x86-64 COFF object");
}

/// Every symbol a file calls must be spelled exactly as the file that defines it
/// spells it.
///
/// On Mach-O and COFF a definition takes a leading underscore and a reference
/// must take the same one. Only definitions went through the target's mangling,
/// so every call into the runtime and every `%rip`-relative read of a string
/// referenced a symbol that did not exist: the object assembled cleanly and the
/// link failed with a page of `undefined reference` errors. Nothing in a single
/// object could reveal that, because the definition lived in the other one.
#[test]
fn windows_references_match_their_definitions() {
    let dir = scratch("windows-symbols");
    let program = assembly_for(&dir, "x86_64-pc-windows-gnu");
    // `asm` writes only the program; the runtime's own text comes from a build,
    // which also writes `dist/umay_runtime.s` next to it.
    let out = Command::new(compiler())
        .current_dir(&dir)
        .arg("build")
        .arg("--target")
        .arg("x86_64-pc-windows-gnu")
        .arg("main.um")
        .output()
        .expect("run umay build");
    let runtime = std::fs::read_to_string(dir.join("dist/umay_runtime.s")).unwrap_or_else(|e| {
        panic!(
            "no runtime assembly: {e}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });

    let defined = |asm: &str| -> std::collections::HashSet<String> {
        asm.lines()
            .filter_map(|l| l.strip_prefix(".globl "))
            .map(|s| s.trim().to_string())
            .collect()
    };
    let called = |asm: &str| -> std::collections::HashSet<String> {
        asm.lines()
            .filter_map(|l| l.trim().strip_prefix("call "))
            .filter(|s| !s.starts_with('*'))
            .map(|s| s.trim().to_string())
            .collect()
    };

    // UMAY's own functions live in one of the two objects and are spelled with
    // the target's prefix. A reference without it cannot resolve.
    let program_defs = defined(&program);
    let runtime_defs = defined(&runtime);
    let all: std::collections::HashSet<String> =
        program_defs.iter().chain(runtime_defs.iter()).cloned().collect();
    // Only UMAY's own names are UMAY's to spell; the C library's are left exactly
    // as the declaration wrote them, and the symbol table check below covers those.
    for asm in [&program, &runtime] {
        for name in called(asm) {
            if !name.starts_with("_umay") {
                continue;
            }
            assert!(
                all.contains(&name),
                "`call {name}` names a symbol neither object defines"
            );
        }
    }
    // The specific case that broke: a program's call into the runtime. Only a
    // function the program actually reaches is emitted now, so the check is on a
    // name both objects agree about rather than on one particular binding.
    let calls_into_runtime: Vec<String> = called(&program)
        .into_iter()
        .filter(|name| name.starts_with("_umay_"))
        .collect();
    assert!(
        !calls_into_runtime.is_empty(),
        "the program must call into the runtime for this test to mean anything"
    );
    for name in calls_into_runtime {
        assert!(
            all.contains(&name),
            "`call {name}` is spelled differently from its definition"
        );
    }
}

/// A target that cannot be linked here says so, and says where the object is.
///
/// Assembling works across systems -- `clang` emits Mach-O on Linux -- but linking
/// does not: it needs the other system's C library and startup code. Handing the
/// objects to the host linker anyway produces "file format not recognized", which
/// tells the user nothing about the real cause. The compiler must say which
/// format the object is in and where it was written.
///
/// The distinction that matters is the *linker's* format, not the host's: MinGW's
/// driver links COFF from Linux, and an earlier version of this check refused it.
#[test]
fn an_unlinkable_target_reports_where_its_object_went() {
    let dir = scratch("unlinkable-report");
    let built = Command::new(compiler())
        .arg("build")
        .arg("--target")
        .arg("x86_64-apple-darwin")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        !built.status.success(),
        "linking a Mach-O object with the host's linker cannot succeed"
    );
    let stderr = String::from_utf8_lossy(&built.stderr);
    assert!(
        stderr.contains("cannot be linked on this host"),
        "the message must name the real cause:\n{stderr}"
    );
    assert!(
        stderr.contains("Mach-O"),
        "the message must name the format the object is in:\n{stderr}"
    );
    assert!(
        stderr.contains("dist/main.o"),
        "the message must point at the object it produced:\n{stderr}"
    );
    // The object is the useful part, so it must exist despite the failure.
    assert!(
        dir.join("dist/main.o").exists(),
        "the object must be left where the message says it is"
    );

    // Windows links here because its driver produces COFF; if that ever fails the
    // check has become too broad and would hide a real regression.
    assert!(
        umayc::target::Target::WINDOWS.link_unavailable(std::path::Path::new("x.o")).is_none(),
        "a target whose linker produces its format must not be refused"
    );
}

/// Asking for just the object is a success, not half a build.
///
/// Linking across systems is impossible without the other system's C library, but
/// assembling is not -- `clang` emits Mach-O on Linux. A caller who wants that
/// object to carry elsewhere should not have to treat the command as a failure,
/// and `--emit-object-only` is how they say so.
#[test]
fn emitting_an_object_alone_succeeds_for_a_target_that_cannot_be_linked() {
    let dir = scratch("emit-object-only");
    let built = Command::new(compiler())
        .arg("build")
        .arg("--emit-object-only")
        .arg("--target")
        .arg("x86_64-apple-darwin")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        built.status.success(),
        "asking only for objects must not fail over a link that was never asked for:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let stdout = String::from_utf8_lossy(&built.stdout);
    assert!(
        stdout.contains("dist/main.o"),
        "the message must name the program object:\n{stdout}"
    );
    assert!(
        stdout.contains("dist/umay_runtime.o"),
        "the runtime object is produced too and must be named:\n{stdout}"
    );
    // Both objects must actually be there and in the target's format.
    for name in ["dist/main.o", "dist/umay_runtime.o"] {
        let bytes = std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            &bytes[..4],
            &[0xcf, 0xfa, 0xed, 0xfe],
            "{name} must be a Mach-O 64 object"
        );
    }

    // No executable is produced, and nothing pretends there is one.
    assert!(
        !dir.join("dist/main").exists(),
        "no executable can exist after a build that skipped linking"
    );

    // `run` needs an executable, so the combination is a usage error rather than
    // a build that reports success with nothing to run.
    let ran = Command::new(compiler())
        .arg("run")
        .arg("--emit-object-only")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay run");
    assert_eq!(
        ran.status.code(),
        Some(2),
        "`run --emit-object-only` must be a usage error:\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
}

/// The link command `--emit-object-only` prints actually works.
///
/// Leaving the last step to the reader only helps if the instructions are right.
/// The flags differ per system (`-no-pie`, `-mconsole`, `-lws2_32`) and a wrong one
/// is a link error about something else entirely, so the command is not something
/// the reader should have to reconstruct. This takes the printed line, runs it
/// exactly as printed, and runs the result.
#[test]
fn the_printed_link_command_works_as_printed() {
    let dir = scratch("printed-link-command");
    let built = Command::new(compiler())
        .arg("build")
        .arg("--emit-object-only")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let stdout = String::from_utf8_lossy(&built.stdout);

    let command = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("link it with: "))
        .unwrap_or_else(|| panic!("the message must give a link command:\n{stdout}"));
    // A command that names the object by a path the reader is not in, or omits the
    // runtime, links nothing. Both have happened here already.
    assert!(
        command.contains("dist/main.o") && command.contains("dist/umay_runtime.o"),
        "the command must name both objects by path: {command}"
    );

    // Run it as printed. A shell is what a reader has; the quoting is the
    // compiler's job to get right.
    let linked = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&dir)
        .output()
        .expect("run the printed command");
    assert!(
        linked.status.success(),
        "the printed command must link:\n{command}\n{}",
        String::from_utf8_lossy(&linked.stderr)
    );

    // And the executable must be where `build` would have put it, so that a caller
    // who then runs `dist/main` finds it.
    let exe = dir.join("dist/main");
    assert!(
        exe.exists(),
        "the printed command must write the path `build` writes: {command}"
    );
    let ran = Command::new(&exe).output().expect("run the built program");
    assert!(
        String::from_utf8_lossy(&ran.stdout).contains("cross"),
        "the linked program must run"
    );

    // The target it cannot link for says why, and does not pretend the command works
    // unchanged.
    let macos = Command::new(compiler())
        .arg("build")
        .arg("--emit-object-only")
        .arg("--target")
        .arg("x86_64-apple-darwin")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    let macos_out = String::from_utf8_lossy(&macos.stdout);
    if !cfg!(target_os = "macos") {
        assert!(
            macos_out.contains("not Mach-O"),
            "a command that cannot work here must say so:\n{macos_out}"
        );
    }
}

/// The objects `--emit-object-only` leaves are enough to link and run elsewhere.
///
/// This is the claim the flag rests on, and nothing before it checked the claim
/// end to end: the symbol test confirms the names line up and the format test
/// confirms the bytes are COFF, but neither says a real linker accepts them and a
/// real program results. Here MinGW links the two objects by hand -- exactly what
/// a user would do on a machine that has MinGW and not this compiler -- and Wine
/// runs the result.
#[test]
fn an_emitted_windows_object_links_and_runs_elsewhere() {
    if !have_tool("x86_64-w64-mingw32-gcc") {
        eprintln!("skip an_emitted_windows_object_links_and_runs_elsewhere: no MinGW");
        return;
    }
    if !have_tool("wine") {
        eprintln!("skip an_emitted_windows_object_links_and_runs_elsewhere: no wine");
        return;
    }
    let dir = scratch("windows-manual-link");
    let built = Command::new(compiler())
        .arg("build")
        .arg("--emit-object-only")
        .arg("--target")
        .arg("x86_64-pc-windows-gnu")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        built.status.success(),
        "emitting objects must succeed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    // Link by hand, with the flags the compiler would have used. Nothing is passed
    // that a user could not pass.
    let exe = dir.join("manual.exe");
    let link = Command::new("x86_64-w64-mingw32-gcc")
        .current_dir(&dir)
        .arg("-o")
        .arg(&exe)
        .arg("dist/main.o")
        .arg("dist/umay_runtime.o")
        .arg("-mconsole")
        .arg("-lws2_32")
        .output()
        .expect("run MinGW");
    assert!(
        link.status.success(),
        "MinGW must accept the objects and link them:\n{}",
        String::from_utf8_lossy(&link.stderr)
    );

    // And the program must run. Wine is an emulator, so this is the Windows the
    // objects were built for rather than the one they were built on.
    let ran = Command::new("wine")
        .current_dir(&dir)
        .arg(&exe)
        .output()
        .expect("run wine");
    let stdout = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "the manually linked program must exit cleanly:\n{stdout}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    // The program prints "cross" and then 0+1+2+3. Checking the sum rather than
    // the word means a program that starts but computes nothing still fails.
    assert!(
        stdout.contains("cross") && stdout.contains('6'),
        "the program must produce its own output, not merely start:\n{stdout}"
    );
}

/// Whether a program is on `PATH` and runnable.
fn have_tool(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `asm --emit-object-only` prints the text and leaves the object.
///
/// `asm` prints assembly and keeps nothing, which is right for reading it and
/// useless for using it: a caller who wants the object had to run `build`, which
/// links, which a cross target cannot do. Asking for the object from `asm` makes
/// seeing the text and keeping the object one command.
///
/// Without the flag nothing changes -- `asm` still writes nothing -- because a
/// command that silently starts leaving files behind is a worse surprise than one
/// that needs a flag to do it.
#[test]
fn asm_leaves_an_object_only_when_asked() {
    let dir = scratch("asm-object");
    // Without the flag: text only, nothing on disk.
    let printed = Command::new(compiler())
        .arg("asm")
        .arg("--target")
        .arg("x86_64-apple-darwin")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay asm");
    assert!(printed.status.success(), "{}", String::from_utf8_lossy(&printed.stderr));
    assert!(
        !printed.stdout.is_empty(),
        "`asm` prints the assembly; that is its job"
    );
    assert!(
        !dir.join("dist").exists(),
        "`asm` without the flag must not create `dist/`"
    );

    // With the flag: the same text, and the object beside it.
    let built = Command::new(compiler())
        .arg("asm")
        .arg("--emit-object-only")
        .arg("--target")
        .arg("x86_64-apple-darwin")
        .arg("main.um")
        .current_dir(&dir)
        .output()
        .expect("run umay asm");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    // The assembly text is unchanged; the flag only appends the line naming what
    // it wrote. Editing the text when asking for an object would make the two
    // runs disagree about the program.
    let text = String::from_utf8_lossy(&printed.stdout);
    let with_object = String::from_utf8_lossy(&built.stdout);
    assert!(
        with_object.starts_with(text.trim_end()),
        "the flag must not change the assembly it prints"
    );
    assert!(
        with_object.contains("built dist/main.o"),
        "it must say where the object went:\n{with_object}"
    );
    let bytes = std::fs::read(dir.join("dist/main.o"))
        .expect("the object must be written where the message says");
    assert_eq!(
        &bytes[..4],
        &[0xcf, 0xfa, 0xed, 0xfe],
        "the object must be Mach-O for this target"
    );
    assert!(
        dir.join("dist/main.s").exists(),
        "the assembly it printed is kept too, so the object can be reproduced"
    );
    // `asm` links nothing, so there is nothing to run.
    assert!(
        !dir.join("dist/main").exists(),
        "`asm` must not produce an executable"
    );
}

/// A Windows object leaves only names MinGW actually provides undefined.
///
/// The symbol check above works on assembly text, which cannot tell a C library
/// name from a misspelt one -- it has to guess which prefix means "the C library's
/// problem". The real object's symbol table can tell, so this reads it back and
/// holds every C name against the list of things MinGW's CRT and Win32 export.
///
/// A misspelt binding (`wrote` for `write`) is undefined, looks like any other
/// external call, and is nothing but a link error on a machine without MinGW. That
/// is how four separate faults survived on Windows before.
#[test]
fn a_windows_object_leaves_only_what_mingw_provides_undefined() {
    if !have_llvm_nm() {
        eprintln!("skip a_windows_object_leaves_only_what_mingw_provides_undefined: no llvm-nm");
        return;
    }
    let dir = scratch("windows-linkable");
    let program = assembly_for(&dir, "x86_64-pc-windows-gnu");
    let program_obj = match assemble_coff(&dir, &program, "main") {
        Some(object) => object,
        None => return,
    };
    let runtime_obj = match runtime_object_for_windows(&dir) {
        Some(object) => object,
        None => return,
    };

    // The entry point MinGW's CRT calls. It is spelled bare in the object -- the
    // CRT startup code names `WinMain` without COFF's underscore -- and a program
    // object that lacks it links but never runs.
    let program_asm_defined: std::collections::HashSet<String> = program
        .lines()
        .filter_map(|l| l.trim().strip_prefix(".globl "))
        .map(|s| s.trim().to_string())
        .collect();
    assert!(
        program_asm_defined.contains("WinMain"),
        "MinGW's CRT calls `WinMain`; the program object must define it"
    );

    // Every UMAY name one object leaves undefined must be defined by the other.
    let program_defined = defined_symbols(&program_obj);
    let runtime_defined = defined_symbols(&runtime_obj);
    for name in undefined_symbols(&program_obj) {
        if name.starts_with("_umay") {
            assert!(
                runtime_defined.contains(&name) || program_defined.contains(&name),
                "`{name}` is undefined in the program and not defined in the runtime"
            );
        }
    }

    // Every other undefined name is the C library's or Win32's, and must be read
    // from the list of what MinGW really exports.
    for object in [&program_obj, &runtime_obj] {
        for name in undefined_symbols(object) {
            if name.starts_with("_umay") || name == "WinMain" {
                continue;
            }
            assert!(
                provides_mingw(&name),
                "`{name}` is not something MinGW provides -- a misspelt binding is a \
                 link error and nothing else catches it"
            );
        }
    }
}

/// Whether MinGW's CRT or Win32 exports `name`.
///
/// The list is the union of what the runtime and the standard library bind today,
/// written with COFF's leading underscore where the C name needs one. A name that
/// is not here is not a name the target has.
fn provides_mingw(name: &str) -> bool {
    // Win32 API functions have no leading underscore; C library functions do.
    const WIN32: &[&str] = &[
        "CloseHandle", "CreatePipe", "CreateThread", "GetCurrentProcessId",
        "GetExitCodeThread", "MoveFileExA", "WaitForSingleObject", "WSAStartup",
        "WSAGetLastError", "socket", "bind", "listen", "accept", "recv", "send",
        "closesocket",
    ];
    const CRT: &[&str] = &[
        "_close", "_errno", "_exit", "_lseeki64", "_mkdir", "_open", "_read", "_unlink",
        "_write", "free", "malloc", "realloc", "memcpy", "memset", "strlen", "abort",
        "getenv",
    ];
    WIN32.contains(&name) || CRT.contains(&name)
}

/// Assembles `asm` into a COFF object named `stem.o`.
fn assemble_coff(dir: &std::path::Path, asm: &str, stem: &str) -> Option<std::path::PathBuf> {
    let src = dir.join(format!("{stem}.s"));
    std::fs::write(&src, asm).expect("write asm");
    let obj = dir.join(format!("{stem}.o"));
    let out = Command::new("clang")
        .arg("--target=x86_64-w64-windows-gnu")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run clang");
    assert!(
        out.status.success(),
        "clang rejected the Windows assembly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(obj)
}

/// The runtime, assembled for Windows from the source the compiler itself uses.
fn runtime_object_for_windows(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let source = umayc::runtime_source_for(umayc::target::Target::WINDOWS);
    let asm = umayc::codegen_source_without_prelude_for(
        "umay_runtime",
        &source,
        umayc::opt::OptLevel::O0,
        umayc::target::Target::WINDOWS,
    );
    let Ok(out) = asm else {
        eprintln!("skip: the runtime did not compile for Windows");
        return None;
    };
    assemble_coff(dir, &out.assembly, "runtime")
}

/// Only the prelude a program reaches is emitted.
///
/// The prelude is compiled with every program and is much larger than most
/// programs: a four-line "hello" used to emit 160 function bodies, including the
/// JSON parser and the HTTP helpers. Emitting them cost assembly time, object
/// size and link time for code nothing would run -- and it forced every system's
/// bindings into every object, so a program that never opened a socket still
/// carried a POSIX `accept`.
#[test]
fn only_the_prelude_a_program_reaches_is_emitted() {
    let dir = scratch("prelude-pruning");
    let asm = assembly_for(&dir, "x86_64-unknown-linux-gnu");

    // The program uses `println`, so the printing path must be there.
    assert!(
        asm.contains("println"),
        "the printing path the program calls must be emitted"
    );
    // Nothing in this program opens a socket or parses JSON, so none of that
    // surface should survive.
    for unused in ["socket", "json_parse", "hash_key"] {
        assert!(
            !asm.contains(unused),
            "an unreached prelude function ({unused}) was emitted anyway"
        );
    }
    // A pruned object is a small fraction of the whole prelude. The exact count
    // is not worth pinning, but a runaway (no pruning at all) is.
    let bodies = asm.lines().filter(|l| l.starts_with(".globl ")).count();
    assert!(
        bodies < 60,
        "pruning should leave a handful of functions, not {bodies}"
    );
}

/// A small aggregate comes back in a register on Win64, not through a pointer.
///
/// Win64 returns a structure of 1, 2, 4 or 8 bytes in `rax`, and one of 16, 32 or
/// 64 bytes in `rax` and further registers. Only the sizes that are not one of
/// those powers use a hidden pointer. Emitting the pointer form for every
/// aggregate made a one-field structure crash on Windows: the caller read the
/// field from `rax`, where the callee had left the address it was told to write
/// through -- and that address was the only reason the field was correct at all.
///
/// The bug had two halves, in different files: the callee's rule and the caller's
/// rule. One was fixed and the other was not, so the test checks both in the same
/// assembly.
#[test]
fn windows_returns_a_small_aggregate_in_a_register() {
    let dir = scratch("win64-small-aggregate");
    let source = "struct S { v: isize }\nfn make() -> S {\n    return S { v: 7isize }\n}\nfn main() -> i32 {\n    let s = make()\n    println_i64(s.v as i64)\n    return 0\n}\n";
    let asm = assembly_of(&dir, source, "x86_64-pc-windows-gnu");

    let make = function_body(&asm, "_umay_user_make").expect("`make` must be emitted");
    // The callee leaves the value in `rax` and the frame holds no outgoing
    // pointer for it. A `%rcx` store at the top of the body would be the signature
    // of the pointer form.
    assert!(
        !make.contains("movq %rcx,") || !make.lines().take(6).any(|l| l.contains("%rcx")),
        "`make` must not spill a return pointer it was not given:\n{make}"
    );
    assert!(
        make.contains("movq %rax,"),
        "`make` must leave its result in `rax`:\n{make}"
    );

    // The caller must not reserve storage and pass its address.
    let main = function_body(&asm, "_main").expect("`main` must be emitted");
    let call_at = main.find("call _umay_user_make").expect("`main` must call `make`");
    let before = &main[..call_at];
    assert!(
        !before.contains("leaq") || !before.lines().rev().take(5).any(|l| l.contains("%rcx")),
        "`main` must not pass a hidden return pointer for an eight-byte structure:\n{main}"
    );
}

/// The assembly of `source` built for `triple`.
fn assembly_of(dir: &std::path::Path, source: &str, triple: &str) -> String {
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let output = Command::new(compiler())
        .arg("asm")
        .arg("--target")
        .arg(triple)
        .arg(&file)
        .current_dir(dir)
        .output()
        .expect("run umay asm");
    assert!(
        output.status.success(),
        "asm failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The lines of one function's body, label excluded.
fn function_body(asm: &str, label: &str) -> Option<String> {
    let start = asm.find(&format!("{label}:\n"))?;
    let rest = &asm[start + label.len() + 2..];
    let end = rest.find("\n\t.size") .or_else(|| rest.find("\n.globl"))?;
    Some(rest[..end].to_string())
}

/// Every frame re-aligns the stack, so a Windows thread entry works.
///
/// Both ABIs promise a 16-byte stack pointer at a call, and every frame assumes
/// it. `CreateThread` does not `call` its start routine -- the thread simply
/// begins there -- so the pointer is eight bytes off from what the frame expects
/// and the first call the thread makes faults. Rounding down in the prologue
/// costs one instruction and makes the frame correct however it was entered.
#[test]
fn a_frame_does_not_trust_the_alignment_it_was_entered_with() {
    for triple in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-gnu"] {
        let dir = scratch(&format!("frame-align-{triple}"));
        let asm = assembly_for(&dir, triple);
        assert!(
            asm.contains("andq $-16, %rsp"),
            "{triple}: the prologue must round the stack down rather than assume an alignment"
        );
        // The rounding happens after the frame is reserved and before anything
        // reads a slot, so `%rbp` still addresses the frame and `leave` still
        // restores the caller's.
        let prologue = asm.split("subq $").nth(1).unwrap_or("");
        let cut = prologue.find(".L_").unwrap_or(prologue.len());
        assert!(
            prologue[..cut].contains("andq $-16, %rsp"),
            "{triple}: rounding must be part of the prologue, not of the body"
        );
    }
}

/// A generic value can have its address taken.
///
/// `&value as *const A` is the only way to reach a value whose type is a
/// parameter, and it is what `thread_spawn` needs to hand one across a thread
/// boundary. It was refused twice over: the cast itself had no rule, and the
/// parser read `&v as T` as `&(v as T)`, taking the address of the cast instead
/// of casting the address.
#[test]
fn a_generic_value_can_have_its_address_taken() {
    let dir = scratch("generic-address");
    let source = "fn addr_of<A>(value: A) -> usize {\n    return unsafe { &value as *const A as usize }\n}\n\nfn main() -> i32 {\n    let n: i64 = 42i64\n    let p = addr_of::<i64>(n)\n    println_i64(unsafe { *(p as *const i64) })\n    return 0\n}\n";
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let output = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        output.status.success(),
        "a generic value's address must be reachable:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ran = Command::new(dir.join("dist").join("main"))
        .output()
        .expect("run program");
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "42\n");
}

/// A pipe's two ends are stored at full width on every system.
///
/// The portable runtime keeps one body that forks and reads a pipe, and it has to
/// run on Windows too. A POSIX descriptor is a small integer; a Windows handle is
/// pointer-sized, so storing either in four bytes truncates one of them. The
/// platform layer widens the narrow one and every caller reads eight bytes.
///
/// Checked here as a *running* program on this host, because the failure is in
/// what the two ends contain, not in how they are spelled.
#[test]
fn a_pipe_reports_two_usable_ends() {
    let dir = scratch("pipe-ends");
    let source = "fn main() -> i32 {\n    let pipe = umay_alloc(16usize)\n    if umay_pipe(pipe) != 0 {\n        println(\"pipe failed\")\n        return 1\n    }\n    let read_end = umay_read_i64(unsafe { pipe as *const u8 })\n    let write_end = umay_read_i64(unsafe { ((unsafe { pipe as usize }) + 8) as *const u8 })\n    if read_end <= 0i64 || write_end <= 0i64 {\n        println(\"an end that names nothing\")\n        return 2\n    }\n    if read_end == write_end {\n        println(\"the same end twice\")\n        return 3\n    }\n    println(\"two ends\")\n    umay_dealloc(pipe)\n    return 0\n}\n";
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let built = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        built.status.success(),
        "pipe program did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let ran = Command::new(dir.join("dist").join("main"))
        .output()
        .expect("run program");
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "two ends\n",
        "a pipe must report two distinct, usable ends"
    );
}

/// A cross-target assembler is a *driver*, not an assembler.
///
/// `clang -o out.o in.s` reads `in.s` as a program to build and link, not as
/// assembly to assemble, so it needs `-c`. Without it the object was never
/// produced and the failure surfaced as a page of `undefined reference` errors
/// against the other object -- which pointed at symbol mangling rather than at
/// the missing flag. GNU `as` never links on its own, which is why the Linux
/// target worked while both cross targets did not.
#[test]
fn a_cross_target_assembler_is_told_to_assemble_only() {
    for target in umayc::target::Target::ALL {
        match target.format {
            umayc::target::ObjectFormat::Elf => continue,
            umayc::target::ObjectFormat::MachO | umayc::target::ObjectFormat::Coff => {
                assert!(
                    target.assembler_args.contains(&"-c"),
                    "{target:?} uses {} as its assembler, which needs `-c` \
                     or it links instead of assembling",
                    target.assembler
                );
            }
        }
    }
}

/// MinGW's startup code calls `WinMain`, not `main`.
#[test]
fn windows_assembly_provides_the_entry_point_mingw_expects() {
    let dir = scratch("windows-entry");
    let asm = assembly_for(&dir, "x86_64-pc-windows-gnu");
    // The name is bare: the startup object spells it without the COFF prefix,
    // unlike every other symbol in the object.
    assert!(
        asm.contains(".globl WinMain") && asm.contains("WinMain:"),
        "a Windows object must define `WinMain` or the CRT cannot start it"
    );
}

#[test]
fn linux_assembly_is_unchanged_by_the_target_layer() {
    let dir = scratch("linux");
    let asm = assembly_for(&dir, "x86_64-unknown-linux-gnu");
    // The Linux path must keep exactly what it had before the target layer
    // existed: no prefix, ELF metadata, the stack note.
    assert!(asm.contains("\nmain:"), "Linux symbols carry no prefix");
    assert!(asm.contains(".type main, @function"), "ELF keeps `.type`");
    assert!(asm.contains(".note.GNU-stack"), "ELF keeps the stack note");
    assert!(!asm.contains("_main:"), "Linux must not gain a symbol prefix");
}

#[test]
fn an_unsupported_target_is_refused_before_building() {
    let dir = scratch("bad_target");
    let out = Command::new(compiler())
        .current_dir(&dir)
        .arg("build")
        .arg("--target")
        .arg("aarch64-apple-darwin")
        .arg("main.um")
        .output()
        .expect("run umay build");
    assert!(!out.status.success(), "an ARM target must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("only x86_64 has a backend"),
        "the error must say why:\n{stderr}"
    );
}

#[test]
fn the_runtime_compiles_for_every_target() {
    // Each platform layer must be accepted by the checker: the extern
    // declarations, the constant table and the adapters are all source, and a
    // mistake in one is a compile error in the runtime that every program pays
    // for. Building the runtime is enough to catch that without a toolchain for
    // the target.
    for target in ["x86_64-unknown-linux-gnu", "x86_64-apple-darwin", "x86_64-pc-windows-gnu"] {
        let dir = scratch(&target.replace(['.', '-'], "_"));
        let out = Command::new(compiler())
            .current_dir(&dir)
            .arg("build")
            .arg("--target")
            .arg(target)
            .arg("main.um")
            .output()
            .expect("run umay build");
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The runtime failing to compile is reported with this exact phrase and
        // is the only failure that matters here; a link failure (missing SDK) is
        // expected on a machine that is not that system.
        assert!(
            !stderr.contains("the runtime failed to compile"),
            "the runtime did not compile for {target}:\n{stderr}"
        );
    }
}
