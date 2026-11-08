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
                "package" | "build" | "dependencies" => {}
                other => errors.push(
                    ManifestError::at(line_number, format!("unknown section `[{other}]`"))
                        .with_help("known sections are [package], [build] and [dependencies]"),
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
        other => errors.push(
            ManifestError::at(line, format!("unknown key `{other}` in [build]"))
                .with_help("known keys are entry, output-dir, opt-level, debug, link, target"),
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
        // A path dependency needs no version, but a registry one does.
        if dependency.path.is_none() && dependency.version.is_empty() {
            errors.push(
                ManifestError::at(
                    0,
                    format!("dependency `{}` has no version", dependency.name),
                )
                .with_help("add a version, or a `path`"),
            );
        }
    }
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
    out
}

/// The project's dependency map, for a resolver.
pub fn dependency_map(manifest: &Manifest) -> BTreeMap<String, Dependency> {
    manifest.dependencies.iter().map(|d| (d.name.clone(), d.clone())).collect()
}

/// The entry point of a project, resolved against the project directory.
pub fn entry_path(dir: &Path, manifest: &Manifest) -> PathBuf {
    dir.join(&manifest.build.entry)
}
