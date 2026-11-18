//! Phase 18: the native dependency / build-script data model.
//!
//! `native_dep` is not wired into `lib.rs` yet (that is a later slice), so this
//! test includes both source modules directly. Both are written to resolve
//! `crate::manifest`, which is the real module once `lib.rs` declares it and
//! the one declared below in this crate.

// The whole manifest module is compiled into this test crate; helpers this
// slice does not exercise are dead here but live in the library crate.
#![allow(dead_code)]

#[path = "../src/manifest.rs"]
mod manifest;
#[path = "../src/native_dep.rs"]
mod native_dep;

use manifest::{parse, Manifest, NativeDependency};
use native_dep::{build_script_path, resolve, resolve_all, ResolvedNative};

const HOST: &str = "x86_64-unknown-linux-gnu";

const WITH_NATIVE: &str = r#"
[package]
name = "demo"
version = "0.1.0"
edition = "2026"

[build]
entry = "src/main.um"
output-dir = "dist"
build-script = "scripts/build.sh"

[native.foo]
pkg-config = "sqlite3"
include-paths = ["vendor/include"]
link-flags = ["-lfoo"]
static = false

[native.foo.target.x86_64-unknown-linux-gnu]
pkg-config = "sqlite3"
include-paths = ["vendor/linux"]
link-flags = ["-lfoo_linux"]
static = true

[native.bar]
pkg-config = ""
include-paths = ["bar/include"]
link-flags = ["-lbar"]
"#;

fn pkg_config_exists() -> bool {
    std::process::Command::new("pkg-config")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_package(pkg: &str) -> bool {
    std::process::Command::new("pkg-config")
        .args(["--exists", pkg])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn base_manifest() -> &'static str {
    "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n"
}

#[test]
fn two_native_entries_parse_with_a_target_override() {
    let m = parse(WITH_NATIVE).expect("a native manifest must parse");
    assert_eq!(m.native.len(), 2);

    let foo = &m.native[0];
    assert_eq!(foo.name, "foo");
    assert_eq!(foo.pkg_config.as_deref(), Some("sqlite3"));
    assert_eq!(foo.include_paths, ["vendor/include"]);
    assert_eq!(foo.link_flags, ["-lfoo"]);
    assert!(!foo.r#static);
    let ov = foo.target_overrides.get(HOST).expect("the linux override");
    assert_eq!(ov.pkg_config.as_deref(), Some("sqlite3"));
    assert_eq!(ov.include_paths, ["vendor/linux"]);
    assert_eq!(ov.link_flags, ["-lfoo_linux"]);
    assert!(ov.r#static);

    let bar = &m.native[1];
    assert_eq!(bar.name, "bar");
    assert_eq!(bar.pkg_config, None, "an empty pkg-config value means no lookup");
    assert_eq!(bar.link_flags, ["-lbar"]);
    assert!(bar.target_overrides.is_empty());
}

#[test]
fn build_script_is_parsed_as_data_only() {
    let m = parse(WITH_NATIVE).expect("valid");
    assert_eq!(build_script_path(&m.build), Some("scripts/build.sh"));
    // The reader records the path; it does not run, validate for execution, or
    // parse any directive from it in this slice.
    let plain: Manifest = parse(base_manifest()).expect("valid");
    assert_eq!(build_script_path(&plain.build), None);
}

#[test]
fn a_v1_manifest_still_has_no_native_dependencies() {
    let m = parse(base_manifest()).expect("a v1 manifest is still valid");
    assert!(m.native.is_empty());
    assert!(m.build.build_script.is_none());
}

#[test]
fn an_unknown_native_key_is_rejected() {
    let text = format!(
        "{}\n[native.foo]\nlink-flgas = [\"-lfoo\"]\n",
        base_manifest()
    );
    let errs = parse(&text).expect_err("a typo must not be silently dropped");
    assert!(
        errs.iter().any(|e| e.message.contains("unknown key `link-flgas`")),
        "got {errs:?}"
    );
}

#[test]
fn a_target_override_replaces_the_base_entry() {
    // No pkg-config here: this checks selection and merging without host state.
    let mut dep = NativeDependency {
        name: "x".to_string(),
        include_paths: vec!["base_inc".to_string()],
        link_flags: vec!["-lbase".to_string()],
        ..Default::default()
    };
    dep.target_overrides.insert(
        HOST.to_string(),
        manifest::NativeOverride {
            include_paths: vec!["ov_inc".to_string()],
            link_flags: vec!["-loverride".to_string()],
            r#static: true,
            ..Default::default()
        },
    );

    let overridden = resolve(&dep, HOST).expect("no pkg-config to query");
    assert_eq!(overridden.link_flags, ["-loverride"]);
    assert_eq!(overridden.include_paths, ["ov_inc"]);
    assert!(overridden.r#static);

    let base = resolve(&dep, "wasm32-unknown-unknown").expect("base entry resolves");
    assert_eq!(base.link_flags, ["-lbase"]);
    assert_eq!(base.include_paths, ["base_inc"]);
    assert!(!base.r#static);
}

#[test]
fn sqlite3_is_resolved_on_this_host() {
    if !pkg_config_exists() || !has_package("sqlite3") {
        return; // pkg-config or the library is unavailable on this host.
    }
    let dep = NativeDependency {
        name: "sqlite3".to_string(),
        pkg_config: Some("sqlite3".to_string()),
        ..Default::default()
    };
    let resolved = resolve(&dep, HOST).expect("sqlite3 is present");
    assert!(
        resolved.link_flags.iter().any(|f| f == "-lsqlite3"),
        "expected -lsqlite3, got {:?}",
        resolved.link_flags
    );
    assert!(resolved.commands.iter().any(|c| c.contains("sqlite3")));
}

#[test]
fn a_missing_package_produces_an_actionable_error() {
    if !pkg_config_exists() {
        return;
    }
    let missing = "umay-phase18-no-such-package-7b1";
    let dep = NativeDependency {
        name: missing.to_string(),
        pkg_config: Some(missing.to_string()),
        ..Default::default()
    };
    let err = resolve(&dep, HOST).expect_err("a missing package must not resolve");
    assert!(err.contains(missing), "error must name the library: {err}");
    assert!(err.contains("pkg-config"), "error must name the command: {err}");
    assert!(err.contains(HOST), "error must name the target: {err}");
    assert!(err.contains("[native."), "error must name the manifest key: {err}");
}

#[test]
fn cache_keys_track_flags_static_and_target() {
    let base = ResolvedNative {
        name: "x".to_string(),
        target: "t1".to_string(),
        pkg_config: None,
        include_paths: Vec::new(),
        link_flags: vec!["-lx".to_string()],
        r#static: false,
        commands: Vec::new(),
    };

    let mut flags = base.clone();
    flags.link_flags.push("-ly".to_string());
    let mut static_link = base.clone();
    static_link.r#static = true;
    let mut target = base.clone();
    target.target = "t2".to_string();
    let mut includes = base.clone();
    includes.include_paths.push("inc".to_string());

    assert_ne!(base.cache_key(), flags.cache_key());
    assert_ne!(base.cache_key(), static_link.cache_key());
    assert_ne!(base.cache_key(), target.cache_key());
    assert_ne!(base.cache_key(), includes.cache_key());

    // Reordering and duplicate flags must not change the key.
    let mut reordered = base.clone();
    reordered.link_flags = vec!["-lx".to_string(), "-lx".to_string()];
    assert_eq!(base.cache_key(), reordered.cache_key());
}

#[test]
fn resolve_all_keeps_manifest_order() {
    let m: Manifest = parse(WITH_NATIVE).expect("valid");
    // "foo" needs sqlite3; skip the whole test if this host cannot resolve it.
    if !pkg_config_exists() || !has_package("sqlite3") {
        return;
    }
    let all = resolve_all(&m, HOST).expect("both entries resolve");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].name, "foo");
    assert_eq!(all[1].name, "bar");
    assert!(all[1].link_flags.iter().any(|f| f == "-lbar"));
}

#[test]
fn native_sections_round_trip_through_the_writer() {
    let first = parse(WITH_NATIVE).expect("valid");
    let written = manifest::to_toml(&first);
    let second = parse(&written).expect("the writer must produce something it can read");
    assert_eq!(first, second, "round-trip changed the manifest:\n{written}");
}
