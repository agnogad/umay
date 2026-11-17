//! `upm`: fetching a dependency from a registry.
//!
//! The registry is a directory of packages addressed by name and version, found
//! through `UMAY_REGISTRY`. These tests build a throwaway registry on disk and
//! drive the fetcher against it, so nothing here depends on the network or on a
//! real registry existing.
//!
//! `UMAY_REGISTRY` is process-global, so every case runs inside one `#[test]`,
//! in order, with the variable set and cleared by hand. Parallel tests sharing
//! one environment variable would race, and a test that passes only when run
//! alone is worse than no test.

use std::path::{Path, PathBuf};

use umayc::manifest;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_upm_it_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn make_package(registry: &Path, name: &str, version: &str, body: &str) {
    let dir = registry.join(name).join(version);
    std::fs::create_dir_all(dir.join("src")).expect("package dir");
    std::fs::write(
        dir.join("umay.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2026\"\n"),
    )
    .expect("manifest");
    std::fs::write(dir.join("src").join(format!("{name}.um")), body).expect("source");
}

fn write_project(dir: &Path, manifest_text: &str, main: &str) {
    std::fs::create_dir_all(dir.join("src")).expect("project src");
    std::fs::write(dir.join("umay.toml"), manifest_text).expect("project manifest");
    std::fs::write(dir.join("src").join("main.um"), main).expect("project main");
}

fn a_dependency_is_fetched_and_locked_by_hash_case() {
    let registry = scratch("registry");
    let project = scratch("project");
    make_package(&registry, "lib", "1.0.0", "fn answer() -> i32 { 42 }\n");
    write_project(
        &project,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlib = \"1.0.0\"\n",
        "fn main() -> i32 { 0 }\n",
    );

    // Point the fetcher at the throwaway registry.
    std::env::set_var("UMAY_REGISTRY", &registry);
    {
        let manifest = manifest::read_project(&project).expect("manifest reads");
        let resolution = umayc::lock::resolve(&project, &manifest);
        assert!(
            resolution.unresolved.is_empty(),
            "a registered dependency must resolve: {:?}",
            resolution.unresolved
        );

        // It was unpacked where the build looks for it.
        let deps_lib = project.join("deps").join("lib");
        assert!(deps_lib.join("umay.toml").is_file(), "the package was not unpacked");
        assert!(
            deps_lib.join("src").join("lib.um").is_file(),
            "the package's source is missing"
        );

        // The lockfile pins a real hash of what was fetched, and verifying it
        // against the project agrees.
        let locked = resolution.lockfile.get("lib").expect("lib is locked");
        assert_eq!(locked.version, "1.0.0");
        assert!(!locked.hash.is_empty(), "the package must be pinned by hash");
        let problems = umayc::lock::verify(&project, &resolution.lockfile);
        assert!(problems.is_empty(), "a fresh fetch must verify: {problems:?}");

        // Changing the fetched content makes verification fail, which is the
        // property that makes the hash worth recording.
        std::fs::write(deps_lib.join("src").join("lib.um"), "fn answer() -> i32 { 43 }\n")
            .expect("edit fetched source");
        let problems = umayc::lock::verify(&project, &resolution.lockfile);
        assert!(
            problems.iter().any(|p| p.contains("lib")),
            "an edited dependency must fail verification: {problems:?}"
        );
    };
    std::env::remove_var("UMAY_REGISTRY");
}

fn a_missing_package_names_the_registry_that_lacked_it_case() {
    let registry = scratch("registry_missing");
    let project = scratch("project_missing");
    write_project(
        &project,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nabsent = \"9.9.9\"\n",
        "fn main() -> i32 { 0 }\n",
    );

    std::env::set_var("UMAY_REGISTRY", &registry);
    {
        let manifest = manifest::read_project(&project).expect("manifest reads");
        let resolution = umayc::lock::resolve(&project, &manifest);
        assert_eq!(resolution.unresolved.len(), 1, "the missing package must be reported");
        let (name, reason) = &resolution.unresolved[0];
        assert_eq!(name, "absent");
        assert!(
            reason.contains("absent") && reason.contains("9.9.9"),
            "the message must name what was looked for: {reason}"
        );
    };
    std::env::remove_var("UMAY_REGISTRY");
}

fn a_transitive_dependency_is_fetched_too_case() {
    let registry = scratch("registry_transitive");
    let project = scratch("project_transitive");
    // `outer` depends on `inner`, both from the registry.
    make_package(&registry, "inner", "1.0.0", "fn inner() -> i32 { 1 }\n");
    let outer_dir = registry.join("outer").join("1.0.0");
    std::fs::create_dir_all(outer_dir.join("src")).expect("outer dir");
    std::fs::write(
        outer_dir.join("umay.toml"),
        "[package]\nname = \"outer\"\nversion = \"1.0.0\"\nedition = \"2026\"\n\n[dependencies]\ninner = \"1.0.0\"\n",
    )
    .expect("outer manifest");
    std::fs::write(outer_dir.join("src").join("outer.um"), "fn outer() -> i32 { 2 }\n")
        .expect("outer source");

    write_project(
        &project,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nouter = \"1.0.0\"\n",
        "fn main() -> i32 { 0 }\n",
    );

    std::env::set_var("UMAY_REGISTRY", &registry);
    {
        let manifest = manifest::read_project(&project).expect("manifest reads");
        let resolution = umayc::lock::resolve(&project, &manifest);
        assert!(
            resolution.unresolved.is_empty(),
            "a transitive graph must resolve: {:?}",
            resolution.unresolved
        );
        assert!(resolution.lockfile.get("outer").is_some(), "outer is locked");
        assert!(
            resolution.lockfile.get("inner").is_some(),
            "a dependency's own dependency must be locked too"
        );
    };
    std::env::remove_var("UMAY_REGISTRY");
}

fn a_package_without_a_version_gets_the_newest_case() {
    let registry = scratch("registry_latest");
    let project = scratch("project_latest");
    make_package(&registry, "lib", "1.0.0", "fn answer() -> i32 { 1 }\n");
    make_package(&registry, "lib", "1.9.0", "fn answer() -> i32 { 9 }\n");
    make_package(&registry, "lib", "1.10.0", "fn answer() -> i32 { 10 }\n");
    write_project(
        &project,
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlib = \"\"\n",
        "fn main() -> i32 { 0 }\n",
    );

    std::env::set_var("UMAY_REGISTRY", &registry);
    {
        let manifest = manifest::read_project(&project).expect("manifest reads");
        let resolution = umayc::lock::resolve(&project, &manifest);
        assert!(resolution.unresolved.is_empty(), "{:?}", resolution.unresolved);
        let locked = resolution.lockfile.get("lib").expect("lib is locked");
        assert_eq!(
            locked.version, "1.10.0",
            "1.10.0 is newer than 1.9.0, which string ordering gets wrong"
        );
        // And the version that was fetched is the one whose body we got.
        let body = std::fs::read_to_string(project.join("deps/lib/src/lib.um"))
            .expect("fetched source");
        assert!(body.contains("10"), "the fetched body must be 1.10.0's: {body}");
    };
    std::env::remove_var("UMAY_REGISTRY");
}

/// Runs every case in order, in one test.
///
/// `UMAY_REGISTRY` is process-global, so cases that set it cannot run in
/// parallel: two tests mutating it at once would see each other's registry and
/// fail at random. One test with ordered cases is slower and correct.
#[test]
fn registry_cases() {
    a_dependency_is_fetched_and_locked_by_hash_case();
    a_missing_package_names_the_registry_that_lacked_it_case();
    a_transitive_dependency_is_fetched_too_case();
    a_package_without_a_version_gets_the_newest_case();
}
