//! Phase 22: mobile targets, without pretending linking works.
//!
//! A mobile triple can be assembled on this host -- clang emits Mach-O for iOS
//! and ELF for Android -- but it cannot be linked into an app here. That needs
//! an SDK, load commands and a signing step, which are not implemented. These
//! tests pin the honest boundary: parsing selects the mobile target rather than
//! a desktop fallback, `--emit-object-only` yields a real object of the right
//! format, and a plain `build` refuses before any linker is invoked.
//!
//! Nothing here attempts a link or claims an IPA/APK. Each test that needs the
//! assembler skips, rather than fails, when clang is absent.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

use umayc::target::{Abi, ObjectFormat, Target};

const IOS_SDK: &str = "Xcode + iOS SDK (codesign required)";
const ANDROID_SDK: &str = "Android NDK sysroot + Bionic";

/// The four triples and what clang must be told to assemble them.
const MOBILE: &[(&str, &str, &[u8], &str)] = &[
    (
        "aarch64-apple-ios",
        "arm64-apple-ios",
        &[0xcf, 0xfa, 0xed, 0xfe],
        IOS_SDK,
    ),
    (
        "aarch64-apple-ios-sim",
        "arm64-apple-ios-simulator",
        &[0xcf, 0xfa, 0xed, 0xfe],
        IOS_SDK,
    ),
    (
        "aarch64-linux-android",
        "aarch64-linux-android",
        &[0x7f, 0x45, 0x4c, 0x46],
        ANDROID_SDK,
    ),
    (
        "x86_64-linux-android",
        "x86_64-linux-android",
        &[0x7f, 0x45, 0x4c, 0x46],
        ANDROID_SDK,
    ),
];

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn have_clang() -> bool {
    Command::new("clang")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase22_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    // A tiny, dependency-free program: no strings, no runtime calls, so the
    // object's shape depends only on the target's format and ABI.
    std::fs::write(dir.join("main.um"), "fn main() -> i32 {\n    return 0\n}\n")
        .expect("write program");
    dir
}

/// Parsing a mobile triple selects its target, never a desktop fallback.
#[test]
fn parse_selects_the_mobile_target_not_a_desktop_fallback() {
    let cases = [
        ("aarch64-apple-ios", Target::AARCH64_APPLE_IOS),
        ("aarch64-apple-ios-sim", Target::AARCH64_APPLE_IOS_SIM),
        ("aarch64-linux-android", Target::AARCH64_LINUX_ANDROID),
        ("x86_64-linux-android", Target::X86_64_LINUX_ANDROID),
    ];
    for (triple, expected) in cases {
        let parsed = Target::parse(triple).unwrap_or_else(|e| panic!("{triple}: {e}"));
        assert_eq!(parsed, expected, "{triple} fell back to a desktop target");
        assert_eq!(parsed.triple, triple);
        assert!(
            Target::ALL.contains(&parsed),
            "{triple} must be listed in ALL"
        );
        for desktop in [
            Target::LINUX,
            Target::MACOS,
            Target::WINDOWS,
            Target::AARCH64_LINUX,
            Target::AARCH64_MACOS,
            Target::AARCH64_WINDOWS,
        ] {
            assert_ne!(parsed, desktop, "{triple} was misclassified as {desktop:?}");
        }
    }
}

/// The arch, os, format, ABI and assembler flags of each mobile target.
#[test]
fn mobile_targets_have_the_right_arch_os_format_and_abi() {
    let expected = [
        (
            Target::AARCH64_APPLE_IOS,
            "aarch64",
            "macos",
            Abi::Aapcs64,
            ObjectFormat::MachO,
            "--target=arm64-apple-ios",
        ),
        (
            Target::AARCH64_APPLE_IOS_SIM,
            "aarch64",
            "macos",
            Abi::Aapcs64,
            ObjectFormat::MachO,
            "--target=arm64-apple-ios-simulator",
        ),
        (
            Target::AARCH64_LINUX_ANDROID,
            "aarch64",
            "linux",
            Abi::Aapcs64,
            ObjectFormat::Elf,
            "--target=aarch64-linux-android",
        ),
        (
            Target::X86_64_LINUX_ANDROID,
            "x86_64",
            "linux",
            Abi::SysV,
            ObjectFormat::Elf,
            "--target=x86_64-linux-android",
        ),
    ];
    for (target, arch, os, abi, format, clang_flag) in expected {
        assert_eq!(target.arch, arch, "{}", target.triple);
        // iOS reuses the macOS runtime/stdlib layer (Darwin); Android reuses the
        // Linux one (POSIX/Bionic). Neither needs a new platform source.
        assert_eq!(target.os, os, "{}", target.triple);
        assert_eq!(target.abi, abi, "{}", target.triple);
        assert_eq!(target.format, format, "{}", target.triple);
        assert!(
            target.assembler_args.contains(&clang_flag),
            "{}",
            target.triple
        );
        assert!(target.assembler_args.contains(&"-c"), "{}", target.triple);
        assert_eq!(target.assembler, "clang", "{}", target.triple);
    }
    // The runtime/stdlib selection follows `os`, so mobile targets get the same
    // platform sources as their desktop siblings by construction.
    assert_eq!(
        Target::AARCH64_APPLE_IOS.runtime_platform_source(),
        Target::AARCH64_MACOS.runtime_platform_source()
    );
    assert_eq!(
        Target::AARCH64_LINUX_ANDROID.stdlib_platform_source(),
        Target::AARCH64_LINUX.stdlib_platform_source()
    );
}

/// The SDK hint is present for mobile targets and absent for desktop ones.
#[test]
fn mobile_sdk_hint_names_the_sdk_and_spares_desktop() {
    for (triple, _, _, sdk) in MOBILE {
        assert_eq!(
            Target::parse(triple).unwrap().mobile_sdk_hint(),
            Some(*sdk),
            "{triple}"
        );
    }
    for target in [
        Target::LINUX,
        Target::MACOS,
        Target::WINDOWS,
        Target::AARCH64_LINUX,
        Target::AARCH64_MACOS,
        Target::AARCH64_WINDOWS,
    ] {
        assert_eq!(target.mobile_sdk_hint(), None, "{}", target.triple);
    }
}

/// `asm --emit-object-only` produces a real object in the target's format.
#[test]
fn emit_object_only_produces_the_right_magic() {
    if !have_clang() {
        eprintln!("skip emit_object_only_produces_the_right_magic: clang not found");
        return;
    }
    for (triple, clang_target, magic, sdk) in MOBILE {
        let dir = scratch(&format!("obj-{}", triple.replace('-', "_")));
        let out = Command::new(compiler())
            .current_dir(&dir)
            .args(["asm", "--emit-object-only", "--target", triple, "main.um"])
            .output()
            .expect("run umay asm --emit-object-only");
        assert!(
            out.status.success(),
            "{triple}: object assembly failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("built dist/main.o"), "{triple}: {stdout}");
        // The honest note must travel with the object, so nobody mistakes the
        // command for one that produced an app.
        assert!(stdout.contains(sdk), "{triple}: missing SDK note: {stdout}");

        let bytes = std::fs::read(dir.join("dist/main.o")).expect("read object");
        assert_eq!(&bytes[..4], *magic, "{triple}: wrong object magic");

        // The same assembly also assembles under the exact clang target flag the
        // compiler is configured with, i.e. the flag really names this format.
        let direct = Command::new("clang")
            .current_dir(&dir)
            .args([
                "-target",
                clang_target,
                "-c",
                "dist/main.s",
                "-o",
                "dist/direct.o",
            ])
            .output()
            .expect("run clang");
        assert!(
            direct.status.success(),
            "{triple}: clang {clang_target} rejected the assembly:\n{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        let direct_bytes = std::fs::read(dir.join("dist/direct.o")).expect("read direct object");
        assert_eq!(
            &direct_bytes[..4],
            *magic,
            "{triple}: clang made the wrong format"
        );
    }
}

/// A plain `build` is refused with the SDK hint and never invokes a linker.
#[test]
fn build_without_emit_object_only_refuses_before_linking() {
    if !have_clang() {
        // The guard is deliberately after the compile-only path, so without an
        // assembler the build fails earlier and this test would prove nothing.
        eprintln!("skip build_without_emit_object_only_refuses_before_linking: clang not found");
        return;
    }
    for (triple, _clang_target, _magic, sdk) in MOBILE {
        let dir = scratch(&format!("build-{}", triple.replace('-', "_")));
        let out = Command::new(compiler())
            .current_dir(&dir)
            .args(["build", "--target", triple, "main.um"])
            .output()
            .expect("run umay build");
        assert_eq!(
            out.status.code(),
            Some(5),
            "{triple}: expected CODEGEN_ERROR (5):\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(triple),
            "{triple}: triple not named: {stderr}"
        );
        assert!(stderr.contains(sdk), "{triple}: SDK not named: {stderr}");
        assert!(
            stderr.contains("--emit-object-only"),
            "{triple}: must point at the supported path: {stderr}"
        );
        // No linker ran: a linker failure has different wording, and no
        // executable (or app bundle) is left behind. The objects are still there
        // because the compile-only path ran before the guard.
        assert!(!stderr.contains("linking failed"), "{triple}: {stderr}");
        assert!(!stderr.contains("cannot run `"), "{triple}: {stderr}");
        assert!(
            !dir.join("dist/main").exists(),
            "{triple}: no executable may exist"
        );
        assert!(
            dir.join("dist/main.o").exists(),
            "{triple}: object should remain"
        );
    }
}
