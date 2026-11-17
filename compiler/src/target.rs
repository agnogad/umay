//! The target description: everything that differs between the operating
//! systems UMAY compiles for, in one place.
//!
//! The frontend, the MIR, the borrow checker and the optimiser are the same on
//! every system. What changes is the layer underneath: how a symbol is spelled,
//! which assembler and linker are run and with which flags, which calling
//! convention arguments travel in, and which system-call numbers the runtime
//! uses.
//!
//! Before this module existed those decisions were scattered across `cli.rs`
//! (the literal strings `"as"`, `"cc"`, `"--64"`, `"-no-pie"`), `asm.rs` (symbol
//! spelling, `.globl`/`.size` directives) and `runtime/runtime.um` (the `O_*`
//! table, `__errno_location`). Adding a system meant finding every one of them.
//! Now it means adding a `Target` value here, and the rest of the compiler asks
//! this module rather than assuming.
//!
//! # Why a struct and not `cfg!`
//!
//! A `cfg!(target_os = "windows")` compiles the *host*'s answer into the binary.
//! `umay --target` has to be able to name a system other than the one it is
//! running on, so the target is data, chosen at run time, not a compile-time
//! fact. The one place `cfg!` is still used is the default: which target a bare
//! `umay build` means is the host's own.
//!
//! # What is deliberately not here
//!
//! The instruction selection and register allocation for the Windows x64 ABI
//! differ in more than a table can express — the callee-saved set and the
//! shadow space change the shape of the prologue, not just its constants. Those
//! live in `isel`/`asm`/`regalloc`, which ask this module which ABI to use. The
//! division is: this module answers questions with a single answer; the codegen
//! modules ask the question.

/// The calling convention a target's functions use.
///
/// Only the two 64-bit ones are modelled, because only x86_64 is a backend.
/// They differ in three ways that reach the code generator:
///
/// * **Argument registers.** SysV puts an aggregate's second eightbyte in
///   `rdx`; Win64 has no equivalent guarantee for aggregates and passes them by
///   pointer, so the aggregate cases never arise there.
/// * **Callee-saved registers.** SysV preserves `rbx`, `r12`-`r15`; Win64 also
///   preserves `rsi`, `rdi` and `xmm6`-`xmm15`, so a prologue that saves only
///   the SysV set silently corrupts a caller's `rsi` on Windows.
/// * **Shadow space.** Every Win64 caller must reserve 32 bytes above the
///   return address for the callee to spill into. SysV has no such rule, and
///   omitting it on Windows lets the callee write over the caller's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    /// System V AMD64 psABI: Linux, the BSDs, macOS.
    SysV,
    /// Microsoft x64 calling convention.
    Win64,
}

/// Which assembler syntax a target's `as` accepts.
///
/// All three targets are assembled by GNU `as`-compatible tools, so the
/// instruction text is identical; what differs is the file's declarative
/// dressing — symbol prefixes, section names, and the metadata each object
/// format wants. Keeping the distinction here means `asm.rs` prints one
/// instruction stream and this module says how to wrap it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    /// ELF, as Linux produces.
    Elf,
    /// Mach-O, as macOS produces.
    MachO,
    /// COFF, as Windows produces.
    Coff,
}

impl ObjectFormat {
    /// The format this machine's own linker reads.
    pub fn of_host() -> ObjectFormat {
        if cfg!(target_os = "macos") {
            ObjectFormat::MachO
        } else if cfg!(target_os = "windows") {
            ObjectFormat::Coff
        } else {
            ObjectFormat::Elf
        }
    }

    /// The format a named linker program produces.
    ///
    /// A cross driver is a different program from the host's: `x86_64-w64-mingw32-gcc`
    /// links COFF whatever it runs on, and the host's `cc` links the host's own
    /// format whatever the target says.
    pub fn of_linker(linker: &str) -> ObjectFormat {
        if linker.contains("mingw") || linker.contains("w64") {
            ObjectFormat::Coff
        } else if linker.contains("apple") || linker.contains("darwin") {
            ObjectFormat::MachO
        } else {
            ObjectFormat::of_host()
        }
    }

    /// The format's name, for a sentence a person reads.
    pub fn name(&self) -> &'static str {
        match self {
            ObjectFormat::Elf => "ELF",
            ObjectFormat::MachO => "Mach-O",
            ObjectFormat::Coff => "COFF",
        }
    }
}

impl ObjectFormat {
    /// The prefix every external symbol carries in this format's object files.
    ///
    /// Mach-O and COFF reserve a leading underscore for C symbols. A symbol
    /// defined as `umay_write` and referenced by the linker as `_umay_write`
    /// would otherwise be an unresolved external that `cc` reports as an
    /// undefined reference to `_main`, which points at nothing useful.
    ///
    /// Linux has no prefix, so its objects and its assembler source agree, which
    /// is what keeps the existing behaviour byte-identical.
    pub fn symbol_prefix(self) -> &'static str {
        match self {
            ObjectFormat::Elf => "",
            ObjectFormat::MachO | ObjectFormat::Coff => "_",
        }
    }
}

/// One compilation target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The triple recorded in cache keys and project manifests.
    pub triple: &'static str,
    /// The operating system, as `std::env::consts::OS` spells it.
    pub os: &'static str,
    /// The architecture. Only x86_64 has a backend, so this is fixed for now
    /// and is recorded rather than inferred, so that an unsupported triple
    /// cannot be mistaken for a supported one.
    pub arch: &'static str,
    /// The calling convention.
    pub abi: Abi,
    /// The object format the assembler emits.
    pub format: ObjectFormat,
    /// The program that assembles `.s` into an object file.
    pub assembler: &'static str,
    /// The arguments the assembler needs before the object and source paths.
    pub assembler_args: &'static [&'static str],
    /// The extension of the assembled object file, including the dot.
    pub object_extension: &'static str,
    /// The program that links objects into an executable.
    pub linker: &'static str,
}

impl Target {
    /// The target named by `--target`, or the host's own when none is given.
    ///
    /// A triple that is recognised as a system but not as an architecture is an
    /// error rather than a fallback: compiling an ARM triple with an x86_64
    /// backend produces an executable the kernel refuses to load, and the error
    /// message would be about the loader rather than about the compiler.
    pub fn parse(name: &str) -> Result<Target, TargetError> {
        let normalised = name.trim();
        // Accept both a full triple (`x86_64-unknown-linux-gnu`) and the short
        // `arch-os` form the project manifest uses (`x86_64-linux`), so a
        // manifest and a command line can say the same thing two ways.
        let lower = normalised.to_ascii_lowercase();
        if !lower.starts_with("x86_64") && !lower.starts_with("amd64") {
            return Err(TargetError::UnsupportedArch {
                target: normalised.to_string(),
            });
        }
        // Order matters: `apple-darwin` contains no `linux`, but a hypothetical
        // `linux-gnu` vs `musl` pair would both match `linux`, which is correct
        // because they differ only in libc and both take the SysV path.
        if lower.contains("darwin") || lower.contains("macos") || lower.contains("apple") {
            Ok(Target::MACOS)
        } else if lower.contains("windows") || lower.contains("win32") || lower.contains("msvc") {
            Ok(Target::WINDOWS)
        } else if lower.contains("linux") {
            Ok(Target::LINUX)
        } else {
            Err(TargetError::UnsupportedOs {
                target: normalised.to_string(),
            })
        }
    }

    /// The host's own target, for a build that names none.
    ///
    /// The fallback order mirrors what the compiler can actually produce: an
    /// unsupported host still gets the Linux definition, because that is the
    /// only one whose toolchain the repository's own tests exercise. Reporting
    /// a target that cannot work is worse than reporting the one that can.
    pub fn host() -> Target {
        if cfg!(target_os = "macos") {
            Target::MACOS
        } else if cfg!(target_os = "windows") {
            Target::WINDOWS
        } else {
            Target::LINUX
        }
    }

    /// Linux, x86_64, glibc.
    pub const LINUX: Target = Target {
        triple: "x86_64-unknown-linux-gnu",
        os: "linux",
        arch: "x86_64",
        abi: Abi::SysV,
        format: ObjectFormat::Elf,
        assembler: "as",
        assembler_args: &["--64"],
        object_extension: ".o",
        linker: "cc",
    };

    /// macOS, x86_64.
    ///
    /// The assembler is `clang`, whose integrated assembler emits Mach-O and
    /// infers everything from `--target`. GNU `as` -- the host's own on a Linux
    /// machine, and what a bare `as` resolves to -- cannot produce a Mach-O
    /// object at all: it rejects `.section __TEXT,__const` and writes ELF. Using
    /// the C driver as the assembler is what makes a cross build work without a
    /// second toolchain installed.
    ///
    /// The linker is the same driver, because it knows where the system
    /// libraries and the entry stub live; `-no-pie` is refused by modern macOS
    /// linkers, which are position-independent by default and are not asked to
    /// change.
    pub const MACOS: Target = Target {
        triple: "x86_64-apple-darwin",
        os: "macos",
        arch: "x86_64",
        abi: Abi::SysV,
        format: ObjectFormat::MachO,
        assembler: "clang",
        // `clang` is a driver, not an assembler: without `-c` it treats the
        // input as a program to link, so a request to produce an object became a
        // link attempt with no objects in it. GNU `as` never links on its own,
        // which is why the Linux target never needed this.
        assembler_args: &["-c", "--target=x86_64-apple-darwin"],
        object_extension: ".o",
        linker: "cc",
    };

    /// Windows, x86_64, MSVC-compatible.
    ///
    /// The assembler is `clang` with a Windows target, whose integrated
    /// assembler emits COFF directly. `ml64` is not usable here because the
    /// instruction text this backend prints is AT&T syntax and `ml64` accepts
    /// only Intel syntax, so choosing it would need a translation step rather
    /// than a different command.
    ///
    /// The linker is the C driver for the same reason as macOS: it locates the
    /// CRT and the linker itself. `-no-pie` is meaningless on PE/COFF and is not
    /// passed; `-mconsole` selects a console subsystem so that a program's
    /// stdout is not sent to nowhere, which is the failure a plain PE link
    /// produces.
    pub const WINDOWS: Target = Target {
        triple: "x86_64-pc-windows-gnu",
        os: "windows",
        arch: "x86_64",
        abi: Abi::Win64,
        format: ObjectFormat::Coff,
        assembler: "clang",
        assembler_args: &["-c", "--target=x86_64-pc-windows-gnu"],
        object_extension: ".o",
        // Not `clang`. Clang's PE linker cannot resolve the `__ImageBase`
        // relocations in MinGW's import libraries and fails with "dangerous
        // relocation" on every CRT archive. The MinGW driver is the one that
        // knows where those libraries are and how to link them, and it finds
        // `_main` and the console CRT itself.
        linker: "x86_64-w64-mingw32-gcc",
    };

    /// Every target this compiler can emit code for.
    pub const ALL: &'static [Target] = &[Target::LINUX, Target::MACOS, Target::WINDOWS];

    /// The name a symbol has in the object file.
    ///
    /// A UMAY-level name (`write_file_bytes`) becomes `umay_write_file_bytes` on
    /// Linux and `_umay_write_file_bytes` on macOS and Windows. Only external
    /// symbols are affected inside the object, but the prefix is applied
    /// uniformly and the C declarations in the runtime use the same spelling, so
    /// there is one rule rather than two.
    pub fn mangle(&self, symbol: &str) -> String {
        let mut out = String::with_capacity(symbol.len() + 1);
        out.push_str(self.format.symbol_prefix());
        out.push_str(symbol);
        out
    }

    /// The file name of a linked executable.
    ///
    /// Windows will not run a file without the extension, and `cc -o dist/main`
    /// there writes `dist/main` (an extensionless PE file) that neither
    /// `CreateProcess` nor the shell will execute by its bare name.
    pub fn executable_name(&self, stem: &str) -> String {
        if self.abi == Abi::Win64 {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    }

    /// Libraries a program's assembly needs beyond the C library, decided by the
    /// external symbols it calls. A program that calls no `sqlite3_*` function
    /// links exactly as before, so a machine without SQLite builds everything
    /// else; one that does gets `-lsqlite3` without a flag or manifest key.
    pub fn system_libraries(assembly: &str) -> Vec<&'static str> {
        // The C name, with and without Mach-O's leading underscore.
        const BY_PREFIX: &[(&str, &str)] = &[("sqlite3_", "-lsqlite3")];
        BY_PREFIX
            .iter()
            .filter(|(prefix, _)| {
                assembly.contains(&format!("call {prefix}")) || assembly.contains(&format!("call _{prefix}"))
            })
            .map(|(_, lib)| *lib)
            .collect()
    }

    /// The command that links this target's two objects into an executable.
    ///
    /// `--emit-object-only` produces objects and no executable, which leaves the
    /// last step to whoever asked. Guessing it is not reasonable: the flags differ
    /// per system (`-no-pie`, `-mconsole`, `-lws2_32`) and getting one wrong is a
    /// link error with a message about the wrong thing. The compiler already knows
    /// them, so it says them.
    pub fn link_command(&self, program: &std::path::Path, runtime: &std::path::Path, exe: &str) -> String {
        let mut parts = vec![
            self.linker.to_string(),
            "-o".to_string(),
            exe.to_string(),
            program.display().to_string(),
            runtime.display().to_string(),
        ];
        parts.extend(self.linker_flags().iter().map(|f| f.to_string()));
        parts.join(" ")
    }

    /// What stands in the way of that command working here, if anything.
    ///
    /// Same rule as `link_unavailable`: the question is the named linker's format.
    /// The linker program being *present* is not enough -- the host's `cc` is here
    /// and still cannot produce Mach-O -- so the caveat names the format it would
    /// have to write, not the program it would have to find.
    pub fn link_command_caveat(&self) -> Option<String> {
        if ObjectFormat::of_linker(self.linker) == self.format {
            return None;
        }
        Some(format!(
            "`{}` here links {}, not {}; this needs a linker that writes {}",
            self.linker,
            ObjectFormat::of_linker(self.linker).name(),
            self.format.name(),
            self.format.name(),
        ))
    }

    /// The linker arguments that produce a runnable executable.
    ///
    /// Kept as a method rather than data because the flags are not independent:
    /// `-no-pie` is a Linux ELF concern, `-arch x86_64` tells a Mach-O linker on
    /// an arm64 host to accept this x86_64-only backend's objects, and
    /// `-mconsole` is a PE concern. A table of flags with a `cfg` per row is what
    /// this replaces.
    pub fn linker_flags(&self) -> &'static [&'static str] {
        match self.format {
            // Position-independent executables are the default and `-no-pie`
            // asks the linker for a fixed load address so that the runtime's
            // absolute addresses hold. It is only meaningful for ELF.
            ObjectFormat::Elf => &["-no-pie"],
            ObjectFormat::MachO => &["-arch", "x86_64"],
            // A console subsystem, so that the process gets the standard handles
            // a program expects to write to, and Winsock's import library: the
            // standard library's socket bindings live in `ws2_32`, which is not
            // part of the default set any more than `-lpthread` is on Linux.
            ObjectFormat::Coff => &["-mconsole", "-lws2_32"],
        }
    }

    /// The directive that opens the read-only data section.
    ///
    /// ELF and COFF both accept `.section .rodata`; Mach-O addresses data by
    /// segment *and* section, so it needs `.section __TEXT,__const`. A literal
    /// in a writable section would be a string a program could modify, which
    /// the language forbids, so the distinction is not cosmetic.
    pub fn rodata_directive(&self) -> &'static str {
        match self.format {
            ObjectFormat::Elf | ObjectFormat::Coff => "\n.section .rodata\n",
            ObjectFormat::MachO => "\n.section __TEXT,__const\n",
        }
    }

    /// The directives that close an assembly file.
    ///
    /// ELF gets the non-executable-stack note that modern toolchains expect.
    /// COFF wants nothing: `as` emits the correct object header on its own, and
    /// a `.note.GNU-stack` there is an assembler error rather than a harmless
    /// extra.
    ///
    /// Mach-O also needs nothing here: `as` records the default segment layout
    /// and the linker derives the entry point from `_main`, which is why the
    /// symbol prefix matters for more than the linker's name resolution.
    pub fn object_trailer(&self) -> &'static str {
        match self.format {
            ObjectFormat::Elf => "\n.section .note.GNU-stack,\"\",@progbits\n",
            ObjectFormat::MachO => "",
            // MinGW's startup code does not read `main` on x86-64: it asks the
            // program for `WinMain` and calls that. Linking an object that
            // defines only `main` therefore fails with
            // `undefined reference to WinMain`, whatever subsystem the driver
            // was told to use. The shim is what makes a console program
            // linkable; it is not a subsystem choice, it is the CRT's entry
            // point.
            //
            // `WinMain` is spelled without the target's prefix here. Every
            // other symbol in a COFF object takes one, but this name has to
            // match the startup object, which spells it bare.
            ObjectFormat::Coff => {
                "\n.section .text\n                 .globl WinMain\n                 WinMain:\n\
                 \x20   jmp _main\n"
            }
        }
    }

    /// The directive that declares a symbol global.
    ///
    /// `.globl` is understood by all three assemblers, so this looks like it
    /// could be a constant. It is a method because Mach-O also needs the symbol
    /// to be marked as live for the linker's dead-code pass; a data or function
    /// symbol that is not referenced inside its own object is otherwise
    /// stripped, which for the runtime's exported functions would mean a link
    /// failure reported against the *caller*.
    pub fn globl_directive(&self) -> &'static str {
        ".globl"
    }

    /// The runtime's operating-system layer, as UMAY source.
    ///
    /// This is the only part of the runtime that is per-system: the extern
    /// declarations it can reach, the numeric constants those calls take, and
    /// the functions that do not exist on every platform (`fork`). The rest of
    /// `runtime/runtime.um` above this layer is portable and is compiled
    /// identically for every target.
    pub fn runtime_platform_source(&self) -> &'static str {
        match self.os {
            "macos" => crate::RUNTIME_PLATFORM_MACOS,
            "windows" => crate::RUNTIME_PLATFORM_WINDOWS,
            _ => crate::RUNTIME_PLATFORM_LINUX,
        }
    }

    /// The standard library's per-system bindings, as UMAY source.
    ///
    /// Networking and threads are the only parts of the standard library whose
    /// *names* are not portable: POSIX spells them `socket`/`accept`/
    /// `pthread_create`, Windows spells them `socket`/`accept`/`CreateThread`
    /// with a `WSAStartup` before the first call and a 64-bit handle. Keeping
    /// them out of the shared prelude is what lets a program build on a system
    /// it was not written on.
    ///
    /// macOS and Linux share the POSIX file; they differ in the C library the
    /// linker finds, not in the names these bindings use.
    /// Why this target cannot be linked from the host, if it cannot.
    ///
    /// Assembling is always possible: `clang` can assemble for a system it cannot
    /// link for, and so can GNU `as` for its own format. Linking is not -- it needs
    /// that system's C library and startup code, which a host does not have for
    /// another system. Handing the objects to the host linker anyway produces a
    /// message about unrecognised file formats, which says nothing about the real
    /// cause.
    ///
    /// Returns a sentence to show the user, or `None` when linking should be
    /// attempted. `object` is the path the object was written to.
    pub fn link_unavailable(&self, object: &std::path::Path) -> Option<String> {
        // What decides this is the linker the target names, not the host it runs
        // on. A cross driver such as MinGW's is a different program from the
        // host's `cc` and produces a different format with it; the host's own
        // linker produces only its own.
        if ObjectFormat::of_linker(self.linker) == self.format {
            return None;
        }
        Some(format!(
            "`{}` cannot be linked on this host: it emits {} and the linker here \
             reads {}. The object was written to `{}` and is usable with a linker \
             that understands {}.",
            self.triple,
            self.format.name(),
            ObjectFormat::of_linker(self.linker).name(),
            object.display(),
            self.format.name(),
        ))
    }

    pub fn stdlib_platform_source(&self) -> &'static str {
        match self.os {
            "windows" => crate::STDLIB_PLATFORM_WINDOWS,
            "macos" => crate::STDLIB_PLATFORM_MACOS,
            _ => crate::STDLIB_PLATFORM_LINUX,
        }
    }
}

/// Why a requested target cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// The triple names an architecture with no backend.
    UnsupportedArch { target: String },
    /// The triple names no system this compiler knows.
    UnsupportedOs { target: String },
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetError::UnsupportedArch { target } => write!(
                f,
                "`{target}` is not a supported architecture\n  \
                 only x86_64 has a backend; supported targets: {}",
                target_list()
            ),
            TargetError::UnsupportedOs { target } => write!(
                f,
                "`{target}` is not a supported target\n  \
                 supported targets: {}",
                target_list()
            ),
        }
    }
}

impl std::error::Error for TargetError {}

/// The supported targets, comma separated, for an error message.
fn target_list() -> String {
    Target::ALL
        .iter()
        .map(|t| t.triple)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_triples() {
        assert_eq!(Target::parse("x86_64-unknown-linux-gnu").unwrap().os, "linux");
        assert_eq!(Target::parse("x86_64-apple-darwin").unwrap().os, "macos");
        assert_eq!(Target::parse("x86_64-pc-windows-gnu").unwrap().os, "windows");
    }

    #[test]
    fn parses_short_manifest_forms() {
        assert_eq!(Target::parse("x86_64-linux").unwrap().os, "linux");
        assert_eq!(Target::parse("x86_64-macos").unwrap().os, "macos");
        assert_eq!(Target::parse("x86_64-windows").unwrap().os, "windows");
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(Target::parse("X86_64-APPLE-DARWIN").unwrap().os, "macos");
    }

    #[test]
    fn rejects_other_architectures() {
        // A plausible-looking triple with no backend must not fall back to
        // x86_64: that produces a binary the kernel will not load.
        let err = Target::parse("aarch64-apple-darwin").unwrap_err();
        assert!(matches!(err, TargetError::UnsupportedArch { .. }));
        assert!(err.to_string().contains("only x86_64 has a backend"));
    }

    #[test]
    fn rejects_unknown_systems() {
        let err = Target::parse("x86_64-unknown-freebsd").unwrap_err();
        assert!(matches!(err, TargetError::UnsupportedOs { .. }));
    }

    #[test]
    fn symbol_prefix_follows_the_object_format() {
        assert_eq!(Target::LINUX.mangle("main"), "main");
        assert_eq!(Target::MACOS.mangle("main"), "_main");
        assert_eq!(Target::WINDOWS.mangle("main"), "_main");
    }

    #[test]
    fn only_windows_adds_an_exe_extension() {
        assert_eq!(Target::LINUX.executable_name("main"), "main");
        assert_eq!(Target::MACOS.executable_name("main"), "main");
        assert_eq!(Target::WINDOWS.executable_name("main"), "main.exe");
    }

    #[test]
    fn sqlite_is_linked_only_when_called() {
        assert_eq!(Target::system_libraries("    call sqlite3_open_v2\n"), vec!["-lsqlite3"]);
        assert_eq!(Target::system_libraries("    call _sqlite3_step\n"), vec!["-lsqlite3"]);
        assert!(Target::system_libraries("    call umay_println\n").is_empty());
    }

    #[test]
    fn linker_flags_match_the_object_format() {
        // `-no-pie` is an ELF flag; Mach-O needs the object architecture
        // because `macos-latest` runners are arm64 while this backend emits
        // x86_64 objects; `-mconsole` is a PE concern.
        assert_eq!(Target::LINUX.linker_flags(), &["-no-pie"]);
        assert_eq!(Target::MACOS.linker_flags(), &["-arch", "x86_64"]);
        assert_eq!(Target::WINDOWS.linker_flags(), &["-mconsole", "-lws2_32"]);
    }

    #[test]
    fn windows_uses_the_win64_abi_and_others_use_sysv() {
        assert_eq!(Target::WINDOWS.abi, Abi::Win64);
        assert_eq!(Target::LINUX.abi, Abi::SysV);
        assert_eq!(Target::MACOS.abi, Abi::SysV);
    }

    #[test]
    fn each_target_has_its_own_runtime_layer() {
        let linux = Target::LINUX.runtime_platform_source();
        let macos = Target::MACOS.runtime_platform_source();
        let windows = Target::WINDOWS.runtime_platform_source();
        assert_ne!(linux, macos);
        assert_ne!(linux, windows);
        assert_ne!(macos, windows);
        // Each layer must define the switch point the portable runtime calls.
        for src in [linux, macos, windows] {
            assert!(src.contains("umay_platform_errno"), "layer lacks the errno accessor");
        }
    }
}
