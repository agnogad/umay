//! The project manifest, `umay.toml`.
//!
//! The format is described in `docs/build-system.md` section 3. This module
//! reads it and enforces the rules that document lists.
//!
//! A hand-written TOML reader lives here for the same reason the JSON reader does
//! in the language server: the compiler has no dependencies. It is narrower than
//! TOML -- it handles the sections, strings, integers, booleans, arrays and inline
//! tables that a manifest uses, and refuses anything else rather than guessing.
//! Guessing is the wrong behaviour for a build file: a manifest that half-parses
//! would build the wrong thing quietly.
//!
//! The validation rules matter as much as the parse. The one that earns its keep
//! is `output-dir`: UMAY does not put a `target/` in a project, and a manifest
//! that names one is asking the build to break that promise.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A parsed manifest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Manifest {
    pub package: Package,
    pub build: Build,
    /// Dependency name to constraint, in the order written.
    pub dependencies: Vec<Dependency>,
    /// Native (system) libraries, in the order written. Empty for a v1 manifest.
    pub native: Vec<NativeDependency>,
    /// Mobile app metadata from `[mobile]`. Empty for a v1 manifest.
    pub mobile: Mobile,
}

/// The `[mobile]` section: app metadata for an iOS/Android build (ADR 0021).
///
/// Every field is optional so a v1 manifest stays valid and round-trips. This is
/// configuration only -- signing, Gradle/Xcode projects and packaging are not in
/// this slice: the reader records the declared data, it does not produce an IPA
/// or AAB.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mobile {
    /// Reverse-DNS bundle id, e.g. `com.example.app`.
    pub app_id: String,
    /// Human-readable app name.
    pub display_name: String,
    /// Target triple, e.g. `aarch64-apple-ios`.
    pub target: Option<String>,
    /// Permission names the app declares.
    pub permissions: Vec<String>,
    /// Bundle asset paths, relative to the project.
    pub assets: Vec<String>,
    /// `portrait`, `landscape` or `any`.
    pub orientation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub edition: String,
    pub authors: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Build {
    pub entry: String,
    pub output_dir: String,
    pub opt_level: u8,
    pub debug: bool,
    /// `dynamic` or `static`.
    pub link: String,
    pub target: String,
    /// Root project's build script, as declared data. This reader never runs it.
    pub build_script: Option<String>,
}

impl Default for Build {
    fn default() -> Self {
        Build {
            entry: "src/main.um".to_string(),
            output_dir: "dist".to_string(),
            opt_level: 0,
            debug: true,
            link: "dynamic".to_string(),
            target: "x86_64-linux".to_string(),
            build_script: None,
        }
    }
}

/// One entry from `[dependencies]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Dependency {
    pub name: String,
    /// The version constraint, e.g. `1.0.0`.
    pub version: String,
    /// A local path, for a dependency that is not fetched.
    pub path: Option<String>,
}

/// One `[native.<name>]` table: a system library, not a UPM package.
///
/// All fields default to "not specified" so a v1 manifest stays unchanged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NativeDependency {
    /// The name from the table header, e.g. `sdl3`.
    pub name: String,
    /// pkg-config package name. `None` (an empty TOML value) means no lookup.
    pub pkg_config: Option<String>,
    /// Extra `-I` paths from the manifest, merged with any pkg-config output.
    pub include_paths: Vec<String>,
    /// Extra linker flags from the manifest, merged with any pkg-config output.
    pub link_flags: Vec<String>,
    /// Whether to ask pkg-config for `--static` and link statically.
    pub r#static: bool,
    /// Overrides keyed by exact target triple. A matching one replaces the base.
    pub target_overrides: BTreeMap<String, NativeOverride>,
}

/// A `[native.<name>.target.<triple>]` table.
///
/// A matching override replaces the base entry wholesale; overrides never chain.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NativeOverride {
    pub pkg_config: Option<String>,
    pub include_paths: Vec<String>,
    pub link_flags: Vec<String>,
    pub r#static: bool,
}

/// A manifest problem: what is wrong and where in the file.
#[derive(Debug, Clone, PartialEq)]
pub struct ManifestError {
    pub line: usize,
    pub message: String,
    pub help: Option<String>,
}

impl ManifestError {
    fn at(line: usize, message: impl Into<String>) -> ManifestError {
        ManifestError { line, message: message.into(), help: None }
    }

    fn with_help(mut self, help: impl Into<String>) -> ManifestError {
        self.help = Some(help.into());
        self
    }
}

/// The editions this compiler understands.
///
/// A manifest may name an edition this build does not know, and that is an error
/// rather than a warning: the same source can mean different things in different
/// editions, so compiling it under the wrong one would be pretending.
pub const EDITIONS: [&str; 1] = ["2026"];

/// Which native table a section header names.
enum NativeContext {
    /// `[native.<name>]`
    Entry(String),
    /// `[native.<name>.target.<triple>]`
    Override(String, String),
}

/// Recognises `[native.<name>]` and `[native.<name>.target.<triple>]`.
///
/// Anything else -- a bare `[native]`, a dotted name, a malformed override --
/// is left for the caller to report as an unknown section. Refusing to guess is
/// the point: a typo must not silently drop a library.
fn native_context(section: &str) -> Option<NativeContext> {
    let rest = section.strip_prefix("native.")?;
    if let Some((name, triple)) = rest.split_once(".target.") {
        if !name.is_empty() && !triple.is_empty() && !triple.contains('.') {
            return Some(NativeContext::Override(name.to_string(), triple.to_string()));
        }
        return None;
    }
    if !rest.is_empty() && !rest.contains('.') {
        Some(NativeContext::Entry(rest.to_string()))
    } else {
        None
    }
}

/// Parses and validates a manifest.
pub fn parse(text: &str) -> Result<Manifest, Vec<ManifestError>> {
    let mut errors = Vec::new();
    let mut manifest = Manifest::default();
    let mut section = String::new();

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        // A `#` starts a comment, but not inside a string. Splitting on the first
        // `#` outside quotes keeps `name = "a#b"` working.
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            match section.as_str() {
                "package" | "build" | "dependencies" | "native" | "mobile" => {}
                other if native_context(other).is_some() => {}
                other => errors.push(
                    ManifestError::at(line_number, format!("unknown section `[{other}]`"))
                        .with_help(
                            "known sections are [package], [build], [dependencies], [native.<name>] and [mobile]",
                        ),
                ),
            }
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            errors.push(
                ManifestError::at(line_number, format!("expected `key = value`, found `{line}`"))
                    .with_help("every line in a manifest is an assignment or a section header"),
            );
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match section.as_str() {
            "package" => read_package(&mut manifest.package, key, value, line_number, &mut errors),
            "build" => read_build(&mut manifest.build, key, value, line_number, &mut errors),
            "dependencies" => read_dependency(&mut manifest.dependencies, key, value, line_number, &mut errors),
            "mobile" => read_mobile(&mut manifest.mobile, key, value, line_number, &mut errors),
            // `[native]` on its own declares nothing; a misplaced key is an error
            // rather than a default silently applied to every library.
            "native" => errors.push(
                ManifestError::at(line_number, format!("unknown key `{key}` in [native]"))
                    .with_help("declare a library as [native.<name>]"),
            ),
            _ if section.starts_with("native.") => {
                if let Some(ctx) = native_context(&section) {
                    match ctx {
                        NativeContext::Entry(name) => read_native_entry(
                            &mut manifest.native, &name, key, value, line_number, &mut errors,
                        ),
                        NativeContext::Override(name, triple) => read_native_override(
                            &mut manifest.native, &name, &triple, key, value, line_number, &mut errors,
                        ),
                    }
                }
                // A malformed native header was already reported above.
            }
            "" => errors.push(
                ManifestError::at(line_number, "this line is outside any section")
                    .with_help("start a section first, e.g. `[package]`"),
            ),
            // An unknown section was already reported.
            _ => {}
        }
    }

    validate(&manifest, &mut errors);
    if errors.is_empty() {
        Ok(manifest)
    } else {
        Err(errors)
    }
}

/// Strips a `#` comment, respecting quoted strings.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

fn read_package(
    package: &mut Package,
    key: &str,
    value: &str,
    line: usize,
    errors: &mut Vec<ManifestError>,
) {
    match key {
        "name" => match string_value(value) {
            Some(text) => package.name = text,
            None => errors.push(bad_value(line, "name", value)),
        },
        "version" => match string_value(value) {
            Some(text) => package.version = text,
            None => errors.push(bad_value(line, "version", value)),
        },
        "edition" => match string_value(value) {
            Some(text) => package.edition = text,
            None => errors.push(bad_value(line, "edition", value)),
        },
        "authors" => match string_array(value) {
            Some(items) => package.authors = items,
            None => errors.push(bad_value(line, "authors", value)),
        },
        "description" => match string_value(value) {
            Some(text) => package.description = Some(text),
            None => errors.push(bad_value(line, "description", value)),
        },
        other => errors.push(
            ManifestError::at(line, format!("unknown key `{other}` in [package]"))
                .with_help("known keys are name, version, edition, authors, description"),
        ),
    }
}

fn read_build(
    build: &mut Build,
    key: &str,
    value: &str,
    line: usize,
    errors: &mut Vec<ManifestError>,
) {
    match key {
        "entry" => match string_value(value) {
            Some(text) => build.entry = text,
            None => errors.push(bad_value(line, "entry", value)),
        },
        "output-dir" => match string_value(value) {
            Some(text) => build.output_dir = text,
            None => errors.push(bad_value(line, "output-dir", value)),
        },
        "opt-level" => match integer_value(value) {
            Some(n) if (0..=3).contains(&n) => build.opt_level = n as u8,
            Some(n) => errors.push(
                ManifestError::at(line, format!("opt-level `{n}` is out of range"))
                    .with_help("opt-level must be 0, 1, 2 or 3"),
            ),
            None => errors.push(bad_value(line, "opt-level", value)),
        },
        "debug" => match bool_value(value) {
            Some(b) => build.debug = b,
            None => errors.push(bad_value(line, "debug", value)),
        },
        "link" => match string_value(value) {
            Some(text) if text == "dynamic" || text == "static" => build.link = text,
            Some(text) => errors.push(
                ManifestError::at(line, format!("link `{text}` is not a link mode"))
                    .with_help("link must be `dynamic` or `static`"),
            ),
            None => errors.push(bad_value(line, "link", value)),
        },
        "target" => match string_value(value) {
            Some(text) => build.target = text,
            None => errors.push(bad_value(line, "target", value)),
        },
        // Data only. The reader records the path; nothing here executes it.
        // Both spellings are accepted: the v2 design writes `build-script`,
        // while callers of the Rust API name the field `build_script`.
        "build-script" | "build_script" => match string_value(value) {
            Some(text) if !text.trim().is_empty() => build.build_script = Some(text),
            Some(_) => {
                errors.push(ManifestError::at(line, "`build-script` is empty").with_help(
                    "give a path such as `scripts/build.sh`, or omit the key",
                ))
            }
            None => errors.push(bad_value(line, "build-script", value)),
        },
        other => errors.push(
            ManifestError::at(line, format!("unknown key `{other}` in [build]"))
                .with_help(
                    "known keys are entry, output-dir, opt-level, debug, link, target, build-script",
                ),
        ),
    }
}

fn read_dependency(
    dependencies: &mut Vec<Dependency>,
    key: &str,
    value: &str,
    line: usize,
    errors: &mut Vec<ManifestError>,
) {
    // `name = "1.0.0"` is the short form.
    if let Some(version) = string_value(value) {
        dependencies.push(Dependency { name: key.to_string(), version, path: None });
        return;
    }
    // `name = { path = "../lib", version = "1.0.0" }` is the table form.
    if let Some(fields) = inline_table(value) {
        let mut version = String::new();
        let mut path = None;
        for (field, field_value) in fields {
            match field.as_str() {
                "version" => version = string_value(&field_value).unwrap_or_default(),
                // An empty path is no path at all: a dependency that names one
                // is not local, so it still has to say what version it wants.
                "path" => {
                    path = string_value(&field_value).filter(|p| !p.trim().is_empty());
                }
                // A dependency is not a place to invent keys: a typo here is a
                // dependency that silently resolves wrong.
                other => errors.push(
                    ManifestError::at(line, format!("unknown dependency key `{other}`"))
                        .with_help("known keys are version and path"),
                ),
            }
        }
        dependencies.push(Dependency { name: key.to_string(), version, path });
        return;
    }
    errors.push(bad_value(line, key, value));
}

/// Reads one key from `[mobile]`. Every value is optional; validation checks the
/// shapes once the whole section is read.
fn read_mobile(
    mobile: &mut Mobile,
    key: &str,
    value: &str,
    line: usize,
    errors: &mut Vec<ManifestError>,
) {
    match key {
        "app-id" => match string_value(value) {
            Some(text) => mobile.app_id = text,
            None => errors.push(bad_value(line, key, value)),
        },
        "display-name" => match string_value(value) {
            Some(text) => mobile.display_name = text,
            None => errors.push(bad_value(line, key, value)),
        },
        "target" => match string_value(value) {
            Some(text) => mobile.target = Some(text),
            None => errors.push(bad_value(line, key, value)),
        },
        "permissions" => match string_array(value) {
            Some(items) => mobile.permissions = items,
            None => errors.push(bad_value(line, key, value)),
        },
        "assets" => match string_array(value) {
            Some(items) => mobile.assets = items,
            None => errors.push(bad_value(line, key, value)),
        },
        "orientation" => match string_value(value) {
            Some(text) => mobile.orientation = Some(text),
            None => errors.push(bad_value(line, key, value)),
        },
        other => errors.push(
            ManifestError::at(line, format!("unknown key `{other}` in [mobile]"))
                .with_help(
                    "known keys are app-id, display-name, target, permissions, assets, orientation",
                ),
        ),
    }
}

/// Finds a native entry by name, creating it on first use.
///
/// Creating on demand makes tables order-independent: a target override may
/// precede its base table without losing either one.
fn ensure_native<'a>(natives: &'a mut Vec<NativeDependency>, name: &str) -> &'a mut NativeDependency {
    if let Some(index) = natives.iter().position(|d| d.name == name) {
        return &mut natives[index];
    }
    let index = natives.len();
    natives.push(NativeDependency { name: name.to_string(), ..Default::default() });
    &mut natives[index]
}

fn read_native_entry(
    natives: &mut Vec<NativeDependency>,
    name: &str,
    key: &str,
    value: &str,
    line: usize,
    errors: &mut Vec<ManifestError>,
) {
    let dep = ensure_native(natives, name);
    match key {
        // An empty value means "do not query pkg-config"; it is how a platform
        // with no pkg-config data turns the lookup off (see the design draft).
        "pkg-config" => match string_value(value) {
            Some(text) => dep.pkg_config = (!text.trim().is_empty()).then_some(text),
            None => errors.push(bad_value(line, key, value)),
        },
        "include-paths" => match string_array(value) {
            Some(items) => dep.include_paths = items,
            None => errors.push(bad_value(line, key, value)),
        },
        "link-flags" => match string_array(value) {
            Some(items) => dep.link_flags = items,
            None => errors.push(bad_value(line, key, value)),
        },
        "static" => match bool_value(value) {
            Some(b) => dep.r#static = b,
            None => errors.push(bad_value(line, key, value)),
        },
        other => errors.push(
            ManifestError::at(line, format!("unknown key `{other}` in [native.{name}]"))
                .with_help("known keys are pkg-config, include-paths, link-flags, static"),
        ),
    }
}

fn read_native_override(
    natives: &mut Vec<NativeDependency>,
    name: &str,
    triple: &str,
    key: &str,
    value: &str,
    line: usize,
    errors: &mut Vec<ManifestError>,
) {
    let dep = ensure_native(natives, name);
    let ov = dep.target_overrides.entry(triple.to_string()).or_default();
    match key {
        "pkg-config" => match string_value(value) {
            Some(text) => ov.pkg_config = (!text.trim().is_empty()).then_some(text),
            None => errors.push(bad_value(line, key, value)),
        },
        "include-paths" => match string_array(value) {
            Some(items) => ov.include_paths = items,
            None => errors.push(bad_value(line, key, value)),
        },
        "link-flags" => match string_array(value) {
            Some(items) => ov.link_flags = items,
            None => errors.push(bad_value(line, key, value)),
        },
        "static" => match bool_value(value) {
            Some(b) => ov.r#static = b,
            None => errors.push(bad_value(line, key, value)),
        },
        other => errors.push(
            ManifestError::at(
                line,
                format!("unknown key `{other}` in [native.{name}.target.{triple}]"),
            )
            .with_help("known keys are pkg-config, include-paths, link-flags, static"),
        ),
    }
}

fn validate(manifest: &Manifest, errors: &mut Vec<ManifestError>) {
    if manifest.package.name.is_empty() {
        errors.push(
            ManifestError::at(0, "the manifest has no package name")
                .with_help("add `name = \"my-project\"` under [package]"),
        );
    } else if !is_valid_name(&manifest.package.name) {
        errors.push(
            ManifestError::at(
                0,
                format!("`{}` is not a valid package name", manifest.package.name),
            )
            .with_help("a package name uses lowercase letters, digits, `-` and `_`"),
        );
    }
    if manifest.package.version.is_empty() {
        errors.push(
            ManifestError::at(0, "the manifest has no package version")
                .with_help("add `version = \"0.1.0\"` under [package]"),
        );
    } else if !is_semver(&manifest.package.version) {
        errors.push(
            ManifestError::at(
                0,
                format!("`{}` is not a semantic version", manifest.package.version),
            )
            .with_help("a version looks like `1.2.3`"),
        );
    }
    if manifest.package.edition.is_empty() {
        errors.push(
            ManifestError::at(0, "the manifest has no edition")
                .with_help(format!("add `edition = \"{}\"` under [package]", EDITIONS[0])),
        );
    } else if !EDITIONS.contains(&manifest.package.edition.as_str()) {
        errors.push(
            ManifestError::at(
                0,
                format!("unknown edition `{}`", manifest.package.edition),
            )
            .with_help(format!("this compiler knows editions: {}", EDITIONS.join(", "))),
        );
    }
    // The rule that does not relax: no `target/` in a project.
    if let Some(reason) = output_dir_problem(&manifest.build.output_dir) {
        errors.push(ManifestError::at(0, reason).with_help("use `dist`"));
    }
    if manifest.build.entry.is_empty() {
        errors.push(
            ManifestError::at(0, "the manifest has no entry point")
                .with_help("add `entry = \"src/main.um\"` under [build]"),
        );
    }
    for dependency in &manifest.dependencies {
        if !is_valid_name(&dependency.name) {
            errors.push(ManifestError::at(
                0,
                format!("`{}` is not a valid dependency name", dependency.name),
            ));
        }
        // A path dependency needs no version. A registry dependency may name one
        // to pin it, or leave it empty to ask for the newest the registry holds --
        // an empty version is a request, not a mistake, now that a registry
        // exists to answer it.
    }
    for native in &manifest.native {
        if !is_valid_name(&native.name) {
            errors.push(ManifestError::at(
                0,
                format!("`{}` is not a valid native dependency name", native.name),
            ));
        }
    }
    validate_mobile(&manifest.mobile, errors);
}

/// The `[mobile]` fields are all optional, so only values that are present and
/// non-empty are checked. A v1 manifest has an empty `Mobile` and passes cleanly.
fn validate_mobile(mobile: &Mobile, errors: &mut Vec<ManifestError>) {
    if !mobile.app_id.is_empty() && !is_valid_app_id(&mobile.app_id) {
        errors.push(
            ManifestError::at(
                0,
                format!("`{}` is not a valid mobile app id", mobile.app_id),
            )
            .with_help(
                "an app id is reverse-DNS like `com.example.app`: lowercase, at least one dot, no spaces",
            ),
        );
    }
    if let Some(orientation) = &mobile.orientation {
        if orientation != "portrait" && orientation != "landscape" && orientation != "any" {
            errors.push(
                ManifestError::at(
                    0,
                    format!("`{orientation}` is not a valid mobile orientation"),
                )
                .with_help("orientation must be `portrait`, `landscape` or `any`"),
            );
        }
    }
    for permission in &mobile.permissions {
        if permission.chars().any(|c| c.is_whitespace() || c == '/' || c == '\\') {
            errors.push(
                ManifestError::at(
                    0,
                    format!("`{permission}` is not a valid mobile permission"),
                )
                .with_help("a permission name has no path separators or whitespace"),
            );
        }
    }
    for asset in &mobile.assets {
        if let Some(reason) = asset_path_problem(asset) {
            errors.push(ManifestError::at(0, reason).with_help(
                "use a path relative to the project, e.g. `assets/icon.png`",
            ));
        }
    }
}

/// A reverse-DNS-ish app id: at least one dot, lowercase, no whitespace and no
/// empty components. Uppercase and spaces are the mistakes this catches.
fn is_valid_app_id(id: &str) -> bool {
    id.contains('.')
        && !id.chars().any(|c| c.is_whitespace() || c.is_uppercase())
        && id.split('.').all(|part| !part.is_empty())
}

/// Why an asset path is not allowed, if it is not. Assets ship in the bundle and
/// are resolved relative to the project, so absolute paths and `..` escapes are
/// refused -- the same spirit as `output_dir_problem`.
fn asset_path_problem(path: &str) -> Option<String> {
    if path.is_empty() {
        return Some("a mobile asset path is empty".to_string());
    }
    if Path::new(path).is_absolute() {
        return Some(format!("the mobile asset `{path}` is an absolute path"));
    }
    if path.replace('\\', "/").split('/').any(|c| c == "..") {
        return Some(format!("the mobile asset `{path}` leaves the project"));
    }
    None
}

/// Why an output directory is not allowed, if it is not.
///
/// Kept as a function of its own so that the reason is testable without building
/// a whole manifest around it.
pub fn output_dir_problem(dir: &str) -> Option<String> {
    if dir.is_empty() {
        return Some("the output directory is empty".to_string());
    }
    // `target` as any component, in any spelling of a path.
    let components: Vec<String> = dir
        .replace('\\', "/")
        .split('/')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty() && c != ".")
        .collect();
    if components.iter().any(|c| c == "target") {
        return Some(format!("the output directory `{dir}` contains `target`"));
    }
    if Path::new(dir).is_absolute() {
        return Some(format!("the output directory `{dir}` is an absolute path"));
    }
    if components.iter().any(|c| c == "..") {
        return Some(format!("the output directory `{dir}` leaves the project"));
    }
    None
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A semantic version: three dot-separated numbers, optionally with a
/// pre-release or build suffix.
fn is_semver(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn bad_value(line: usize, key: &str, value: &str) -> ManifestError {
    ManifestError::at(line, format!("`{key}` has an unexpected value: {value}"))
}

fn string_value(value: &str) -> Option<String> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    // A manifest string has no escapes: a path with a backslash is a path.
    Some(inner.to_string())
}

fn integer_value(value: &str) -> Option<i64> {
    value.parse().ok()
}

fn bool_value(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn string_array(value: &str) -> Option<Vec<String>> {
    let inner = value.strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    for item in inner.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        out.push(string_value(item)?);
    }
    Some(out)
}

/// `{ key = value, key = value }` into pairs, with the values left as text.
fn inline_table(value: &str) -> Option<Vec<(String, String)>> {
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    let mut fields = Vec::new();
    for part in split_top_level(inner) {
        let (key, field_value) = part.split_once('=')?;
        fields.push((key.trim().to_string(), field_value.trim().to_string()));
    }
    Some(fields)
}

/// Splits on commas that are not inside a string.
fn split_top_level(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_string = !in_string;
                current.push(c);
            }
            ',' if !in_string => {
                out.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// Reads `umay.toml` from a project directory.
pub fn read_project(dir: &Path) -> Result<Manifest, Vec<ManifestError>> {
    let path = dir.join("umay.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            return Err(vec![ManifestError::at(
                0,
                format!("cannot read `{}`: {e}", path.display()),
            )
            .with_help("run `umay new <name>` to create a project")])
        }
    };
    parse(&text)
}

/// Renders a manifest back to text, for `upm` to write.
pub fn to_toml(manifest: &Manifest) -> String {
    let mut out = String::new();
    out.push_str("[package]\n");
    out.push_str(&format!("name = \"{}\"\n", manifest.package.name));
    out.push_str(&format!("version = \"{}\"\n", manifest.package.version));
    out.push_str(&format!("edition = \"{}\"\n", manifest.package.edition));
    if !manifest.package.authors.is_empty() {
        let listed: Vec<String> =
            manifest.package.authors.iter().map(|a| format!("\"{a}\"")).collect();
        out.push_str(&format!("authors = [{}]\n", listed.join(", ")));
    }
    if let Some(description) = &manifest.package.description {
        out.push_str(&format!("description = \"{description}\"\n"));
    }
    out.push_str("\n[build]\n");
    out.push_str(&format!("entry = \"{}\"\n", manifest.build.entry));
    out.push_str(&format!("output-dir = \"{}\"\n", manifest.build.output_dir));
    out.push_str(&format!("opt-level = {}\n", manifest.build.opt_level));
    out.push_str(&format!("debug = {}\n", manifest.build.debug));
    out.push_str(&format!("link = \"{}\"\n", manifest.build.link));
    out.push_str(&format!("target = \"{}\"\n", manifest.build.target));
    if let Some(script) = &manifest.build.build_script {
        out.push_str(&format!("build-script = \"{script}\"\n"));
    }
    if !manifest.dependencies.is_empty() {
        out.push_str("\n[dependencies]\n");
        for dependency in &manifest.dependencies {
            match &dependency.path {
                Some(path) => out.push_str(&format!(
                    "{} = {{ path = \"{path}\", version = \"{}\" }}\n",
                    dependency.name, dependency.version
                )),
                None => out.push_str(&format!(
                    "{} = \"{}\"\n",
                    dependency.name, dependency.version
                )),
            }
        }
    }
    for native in &manifest.native {
        out.push_str(&format!("\n[native.{}]\n", native.name));
        write_native_fields(
            &mut out,
            native.pkg_config.as_deref(),
            &native.include_paths,
            &native.link_flags,
            native.r#static,
        );
        for (triple, ov) in &native.target_overrides {
            out.push_str(&format!("\n[native.{}.target.{}]\n", native.name, triple));
            write_native_fields(
                &mut out,
                ov.pkg_config.as_deref(),
                &ov.include_paths,
                &ov.link_flags,
                ov.r#static,
            );
        }
    }
    // An empty `[mobile]` writes no section, so a v1 manifest round-trips to the
    // exact same bytes. Fields are emitted in a fixed order, not map order.
    if manifest.mobile != Mobile::default() {
        out.push_str("\n[mobile]\n");
        if !manifest.mobile.app_id.is_empty() {
            out.push_str(&format!("app-id = \"{}\"\n", manifest.mobile.app_id));
        }
        if !manifest.mobile.display_name.is_empty() {
            out.push_str(&format!("display-name = \"{}\"\n", manifest.mobile.display_name));
        }
        if let Some(target) = &manifest.mobile.target {
            out.push_str(&format!("target = \"{target}\"\n"));
        }
        if !manifest.mobile.permissions.is_empty() {
            let listed: Vec<String> =
                manifest.mobile.permissions.iter().map(|p| format!("\"{p}\"")).collect();
            out.push_str(&format!("permissions = [{}]\n", listed.join(", ")));
        }
        if !manifest.mobile.assets.is_empty() {
            let listed: Vec<String> =
                manifest.mobile.assets.iter().map(|a| format!("\"{a}\"")).collect();
            out.push_str(&format!("assets = [{}]\n", listed.join(", ")));
        }
        if let Some(orientation) = &manifest.mobile.orientation {
            out.push_str(&format!("orientation = \"{orientation}\"\n"));
        }
    }
    out
}

fn write_native_fields(
    out: &mut String,
    pkg_config: Option<&str>,
    include_paths: &[String],
    link_flags: &[String],
    is_static: bool,
) {
    if let Some(pkg) = pkg_config {
        out.push_str(&format!("pkg-config = \"{pkg}\"\n"));
    }
    if !include_paths.is_empty() {
        let listed: Vec<String> = include_paths.iter().map(|p| format!("\"{p}\"")).collect();
        out.push_str(&format!("include-paths = [{}]\n", listed.join(", ")));
    }
    if !link_flags.is_empty() {
        let listed: Vec<String> = link_flags.iter().map(|f| format!("\"{f}\"")).collect();
        out.push_str(&format!("link-flags = [{}]\n", listed.join(", ")));
    }
    out.push_str(&format!("static = {is_static}\n"));
}

/// The project's dependency map, for a resolver.
pub fn dependency_map(manifest: &Manifest) -> BTreeMap<String, Dependency> {
    manifest.dependencies.iter().map(|d| (d.name.clone(), d.clone())).collect()
}

/// The entry point of a project, resolved against the project directory.
pub fn entry_path(dir: &Path, manifest: &Manifest) -> PathBuf {
    dir.join(&manifest.build.entry)
}
