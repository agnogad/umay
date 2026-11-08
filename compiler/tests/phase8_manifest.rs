//! The project manifest, `umay.toml`.
//!
//! A build file is read before anything is compiled, so a manifest that half-parses
//! builds the wrong thing quietly. These tests are mostly about refusal: the
//! validator must say no to the cases the build-system document lists, and say
//! why.
//!
//! The one rule that is not negotiable is `output-dir`. UMAY does not put a
//! `target/` in a project, so a manifest that names one has to be rejected rather
//! than quietly obeyed.

use umayc::manifest::{self, Manifest, ManifestError};

fn parse(text: &str) -> Result<Manifest, Vec<ManifestError>> {
    manifest::parse(text)
}

fn errors(text: &str) -> Vec<ManifestError> {
    parse(text).expect_err("this manifest should be rejected")
}

fn good() -> &'static str {
    "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[build]\nentry = \"src/main.um\"\noutput-dir = \"dist\"\nopt-level = 0\n"
}

fn has(errors: &[ManifestError], needle: &str) -> bool {
    errors.iter().any(|e| e.message.contains(needle))
}

#[test]
fn the_manifest_umay_new_writes_is_accepted() {
    let text = "# UMAY project manifest\n# See docs/build-system.md for the full reference.\n\n[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[build]\nentry = \"src/main.um\"\noutput-dir = \"dist\"\nopt-level = 0\n";
    let manifest = parse(text).expect("the generated manifest must be valid");
    assert_eq!(manifest.package.name, "demo");
    assert_eq!(manifest.package.edition, "2026");
    assert_eq!(manifest.build.output_dir, "dist");
}

#[test]
fn defaults_fill_in_what_the_manifest_omits() {
    let manifest = parse("[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n")
        .expect("a manifest with no [build] section is valid");
    assert_eq!(manifest.build.entry, "src/main.um");
    assert_eq!(manifest.build.output_dir, "dist");
    assert_eq!(manifest.build.opt_level, 0);
    assert_eq!(manifest.build.link, "dynamic");
}

#[test]
fn a_manifest_without_a_name_is_rejected() {
    let errs = errors("[package]\nversion = \"0.1.0\"\nedition = \"2026\"\n");
    assert!(has(&errs, "no package name"), "got {errs:?}");
}

#[test]
fn a_name_that_is_not_an_identifier_is_rejected() {
    for bad in ["Demo", "1demo", "de mo", "de.mo", ""] {
        let text = format!(
            "[package]\nname = \"{bad}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n"
        );
        let errs = errors(&text);
        assert!(
            errs.iter().any(|e| e.message.contains("valid package name") || e.message.contains("no package name")),
            "`{bad}` should be rejected, got {errs:?}"
        );
    }
    // The characters that are allowed.
    for good in ["demo", "my-project", "my_project", "lib2"] {
        let text = format!(
            "[package]\nname = \"{good}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n"
        );
        assert!(parse(&text).is_ok(), "`{good}` should be allowed");
    }
}

#[test]
fn a_version_that_is_not_semantic_is_rejected() {
    for bad in ["1", "1.0", "1.0.0.0", "v1.0.0", "one"] {
        let text = format!(
            "[package]\nname = \"demo\"\nversion = \"{bad}\"\nedition = \"2026\"\n"
        );
        let errs = errors(&text);
        assert!(has(&errs, "semantic version"), "`{bad}` should be rejected, got {errs:?}");
    }
    for good in ["0.1.0", "10.20.30", "1.0.0-alpha", "1.0.0+build"] {
        let text = format!(
            "[package]\nname = \"demo\"\nversion = \"{good}\"\nedition = \"2026\"\n"
        );
        assert!(parse(&text).is_ok(), "`{good}` should be allowed");
    }
}

#[test]
fn an_unknown_edition_is_rejected() {
    let errs = errors("[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"1999\"\n");
    assert!(has(&errs, "unknown edition"), "got {errs:?}");
    assert!(
        errs.iter().any(|e| e.help.as_deref().is_some_and(|h| h.contains("2026"))),
        "the error should name the editions that exist: {errs:?}"
    );
}

#[test]
fn an_output_directory_named_target_is_rejected() {
    // The rule that does not relax. Every spelling of a path that would put a
    // `target` in the project has to be caught, or the promise is only kept for
    // people who write it the same way we imagined.
    for dir in ["target", "target/", "./target", "build/target", "build/target/out", "TARGET", "target/debug"] {
        let text = format!(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[build]\noutput-dir = \"{dir}\"\n"
        );
        // `TARGET` is a different name; only the lowercase spelling is the rule.
        if dir == "TARGET" {
            assert!(parse(&text).is_ok(), "`{dir}` is not `target` and should be allowed");
            continue;
        }
        let errs = errors(&text);
        assert!(
            has(&errs, "target"),
            "`{dir}` should be rejected as an output directory, got {errs:?}"
        );
    }
}

#[test]
fn an_output_directory_outside_the_project_is_rejected() {
    for dir in ["../elsewhere", "/tmp/out", "/abs"] {
        let text = format!(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[build]\noutput-dir = \"{dir}\"\n"
        );
        let errs = errors(&text);
        assert!(
            errs.iter().any(|e| e.message.contains("absolute") || e.message.contains("leaves the project")),
            "`{dir}` should be rejected, got {errs:?}"
        );
    }
}

#[test]
fn an_unknown_key_is_rejected_rather_than_ignored() {
    // A typo in a build file that is silently ignored builds the wrong thing.
    let errs = errors(
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\nauthros = [\"a\"]\n",
    );
    assert!(has(&errs, "unknown key `authros`"), "got {errs:?}");
}

#[test]
fn an_unknown_section_is_rejected() {
    let errs = errors(
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[features]\nx = 1\n",
    );
    assert!(has(&errs, "unknown section"), "got {errs:?}");
}

#[test]
fn a_line_outside_a_section_is_rejected() {
    let errs = errors("name = \"demo\"\n");
    assert!(has(&errs, "outside any section"), "got {errs:?}");
}

#[test]
fn an_out_of_range_opt_level_is_rejected() {
    for level in ["4", "9", "-1"] {
        let text = format!(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[build]\nopt-level = {level}\n"
        );
        assert!(parse(&text).is_err(), "opt-level {level} should be rejected");
    }
    for level in ["0", "1", "2", "3"] {
        let text = format!(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[build]\nopt-level = {level}\n"
        );
        assert!(parse(&text).is_ok(), "opt-level {level} should be allowed");
    }
}

#[test]
fn an_invalid_link_mode_is_rejected() {
    let errs = errors(
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[build]\nlink = \"magic\"\n",
    );
    assert!(has(&errs, "not a link mode"), "got {errs:?}");
}

#[test]
fn dependencies_are_read_in_both_forms() {
    let text = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\nlib = \"1.0.0\"\nlocal = { path = \"../local\", version = \"0.2.0\" }\n";
    let manifest = parse(text).expect("both dependency forms are valid");
    assert_eq!(manifest.dependencies.len(), 2);
    assert_eq!(manifest.dependencies[0].name, "lib");
    assert_eq!(manifest.dependencies[0].version, "1.0.0");
    assert_eq!(manifest.dependencies[0].path, None);
    assert_eq!(manifest.dependencies[1].name, "local");
    assert_eq!(manifest.dependencies[1].path.as_deref(), Some("../local"));
}

#[test]
fn a_registry_dependency_with_no_version_asks_for_the_newest() {
    // An empty version is now a request for the newest the registry holds, not
    // a mistake: there is a registry to answer it. The dependency is accepted
    // with an empty version, which `umay lock` resolves to a concrete one.
    let manifest = parse(
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n[dependencies]\nlib = \"\"\n",
    )
    .expect("an empty version is a valid request for the newest release");
    assert_eq!(manifest.dependencies[0].name, "lib");
    assert!(manifest.dependencies[0].version.is_empty());
    assert!(manifest.dependencies[0].path.is_none());
}

#[test]
fn a_comment_is_not_part_of_a_value() {
    let text = "[package]\nname = \"demo\" # the project name\nversion = \"0.1.0\"\nedition = \"2026\"\n";
    let manifest = parse(text).expect("a trailing comment is allowed");
    assert_eq!(manifest.package.name, "demo");
}

#[test]
fn a_hash_inside_a_string_is_not_a_comment() {
    let text = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\ndescription = \"a # sign\"\n";
    let manifest = parse(text).expect("a `#` inside quotes is part of the value");
    assert_eq!(manifest.package.description.as_deref(), Some("a # sign"));
}

#[test]
fn a_manifest_round_trips_through_its_own_writer() {
    let text = "[package]\nname = \"demo\"\nversion = \"1.2.3\"\nedition = \"2026\"\nauthors = [\"A <a@example.com>\"]\ndescription = \"a demo\"\n\n[build]\nentry = \"src/main.um\"\noutput-dir = \"dist\"\nopt-level = 2\ndebug = false\nlink = \"static\"\ntarget = \"x86_64-linux\"\n\n[dependencies]\nlib = \"1.0.0\"\nlocal = { path = \"../local\", version = \"0.2.0\" }\n";
    let first = parse(text).expect("valid");
    let written = manifest::to_toml(&first);
    let second = parse(&written).expect("the writer must produce something it can read");
    assert_eq!(first, second, "round-trip changed the manifest:\n{written}");
}

#[test]
fn several_problems_are_reported_together() {
    // Reporting one problem per run makes a broken manifest a guessing game.
    let errs = errors("[package]\nname = \"Demo\"\nversion = \"one\"\nedition = \"1999\"\n[build]\noutput-dir = \"target\"\n");
    assert!(errs.len() >= 3, "expected several errors, got {errs:?}");
}

#[test]
fn a_valid_manifest_produces_no_errors() {
    assert!(parse(good()).is_ok());
}
