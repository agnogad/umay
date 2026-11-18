//! Phase 6 / ADR 0021: the `[mobile]` section of `umay.toml`.
//!
//! Every field is optional so a v1 manifest stays valid and round-trips to the
//! exact same bytes. When a value *is* given, the shapes ADR 0021 needs are
//! enforced: a reverse-DNS app id, a known orientation, bare permission names and
//! project-relative asset paths. This slice is configuration only -- no signing,
//! Gradle/Xcode or packaging.

use umayc::manifest::{self, Manifest, ManifestError, Mobile};

fn parse(text: &str) -> Result<Manifest, Vec<ManifestError>> {
    manifest::parse(text)
}

fn errors(text: &str) -> Vec<ManifestError> {
    parse(text).expect_err("this manifest should be rejected")
}

fn has(errs: &[ManifestError], needle: &str) -> bool {
    errs.iter().any(|e| e.message.contains(needle))
}

/// A minimal valid manifest with a `[mobile]` section carrying `lines`.
fn with_mobile(lines: &str) -> String {
    format!(
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [build]\noutput-dir = \"dist\"\n\n[mobile]\n{lines}\n"
    )
}

#[test]
fn a_full_mobile_manifest_parses_and_round_trips() {
    let text = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
                [build]\nentry = \"src/main.um\"\noutput-dir = \"dist\"\n\n\
                [mobile]\n\
                app-id = \"com.example.demo\"\n\
                display-name = \"Demo\"\n\
                target = \"aarch64-apple-ios\"\n\
                permissions = [\"camera\", \"android.permission.INTERNET\"]\n\
                assets = [\"assets/icon.png\", \"assets/logo.png\"]\n\
                orientation = \"landscape\"\n";

    let first = parse(text).expect("a full [mobile] manifest is valid");
    assert_eq!(first.mobile.app_id, "com.example.demo");
    assert_eq!(first.mobile.display_name, "Demo");
    assert_eq!(first.mobile.target.as_deref(), Some("aarch64-apple-ios"));
    assert_eq!(first.mobile.permissions, ["camera", "android.permission.INTERNET"]);
    assert_eq!(first.mobile.assets, ["assets/icon.png", "assets/logo.png"]);
    assert_eq!(first.mobile.orientation.as_deref(), Some("landscape"));

    let written = manifest::to_toml(&first);
    let second = parse(&written).expect("the writer must produce something it can read");
    assert_eq!(first, second, "round-trip changed the manifest:\n{written}");
}

#[test]
fn a_v1_manifest_without_mobile_round_trips_byte_identically() {
    let v1 = "[package]\nname = \"demo\"\nversion = \"1.2.3\"\nedition = \"2026\"\n\
              authors = [\"A <a@example.com>\"]\ndescription = \"a demo\"\n\n\
              [build]\nentry = \"src/main.um\"\noutput-dir = \"dist\"\nopt-level = 2\n\
              debug = false\nlink = \"static\"\ntarget = \"x86_64-linux\"\n\n\
              [dependencies]\nlib = \"1.0.0\"\n\
              local = { path = \"../local\", version = \"0.2.0\" }\n";

    let manifest = parse(v1).expect("a v1 manifest is still valid");
    assert_eq!(manifest.mobile, Mobile::default());
    assert_eq!(manifest::to_toml(&manifest), v1, "v1 output must not change");
}

#[test]
fn invalid_mobile_values_are_rejected() {
    // app id: reverse-DNS-ish -- a dot, lowercase, no spaces, no empty part.
    for app in ["demo", "Com.Example", "com example.app", "com..demo", ".demo"] {
        let errs = errors(&with_mobile(&format!("app-id = \"{app}\"\n")));
        assert!(
            has(&errs, "not a valid mobile app id"),
            "`{app}` should be rejected, got {errs:?}"
        );
    }
    assert!(parse(&with_mobile("app-id = \"com.example.demo\"\n")).is_ok());

    // orientation: one of the three declared values.
    let errs = errors(&with_mobile("orientation = \"diagonal\"\n"));
    assert!(has(&errs, "not a valid mobile orientation"), "got {errs:?}");
    for ok in ["portrait", "landscape", "any"] {
        assert!(
            parse(&with_mobile(&format!("orientation = \"{ok}\"\n"))).is_ok(),
            "`{ok}` should be allowed"
        );
    }

    // permissions: bare names, no path separators or whitespace.
    for perm in ["a/b", "a\\b", "a b", "a\tb"] {
        let errs = errors(&with_mobile(&format!("permissions = [\"{perm}\"]\n")));
        assert!(
            has(&errs, "not a valid mobile permission"),
            "`{perm}` should be rejected, got {errs:?}"
        );
    }
    assert!(parse(&with_mobile("permissions = [\"camera\", \"android.permission.INTERNET\"]\n")).is_ok());

    // assets: relative paths, no absolute path and no `..` escape.
    for asset in ["/tmp/icon.png", "../icon.png", "assets/../../icon.png", "a\\..\\b"] {
        let errs = errors(&with_mobile(&format!("assets = [\"{asset}\"]\n")));
        assert!(
            errs.iter().any(|e| e.message.contains("absolute") || e.message.contains("leaves the project")),
            "`{asset}` should be rejected, got {errs:?}"
        );
    }
    assert!(parse(&with_mobile("assets = [\"assets/icon.png\"]\n")).is_ok());
}

#[test]
fn an_empty_mobile_section_is_accepted_and_emits_no_toml() {
    let text = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
                [build]\noutput-dir = \"dist\"\n\n[mobile]\n";

    let first = parse(text).expect("an empty [mobile] section is valid");
    assert_eq!(first.mobile, Mobile::default());

    let written = manifest::to_toml(&first);
    assert!(!written.contains("[mobile]"), "an empty section must not be emitted:\n{written}");
    assert_eq!(parse(&written).expect("round-trip must parse"), first);

    // It is byte-for-byte the output of the same manifest with no [mobile] at all.
    let no_mobile = parse(
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [build]\noutput-dir = \"dist\"\n",
    )
    .expect("valid");
    assert_eq!(written, manifest::to_toml(&no_mobile));
}
