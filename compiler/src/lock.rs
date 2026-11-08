//! The lockfile, `umay.lock`.
//!
//! A lockfile records the exact resolved version and content hash of every
//! dependency, so that two checkouts of the same project build the same thing.
//! That is the whole value: a manifest says what a project wants, a lockfile says
//! what it got.
//!
//! There is no registry yet, so `upm` cannot fetch anything. What exists is the
//! format, the resolution of path dependencies, and the verification that a
//! locked dependency still has the content it had when it was locked. When a
//! registry arrives, it slots in where `resolve` looks for a dependency rather
//! than changing the file format.
//!
//! The file is deliberately not TOML. A lockfile is written by a tool and read by
//! a tool; a line-oriented format with fixed field order is simpler to produce
//! deterministically, and determinism is the point.

use std::path::{Path, PathBuf};

use crate::manifest::{self, Manifest};
use crate::sha256;

/// One locked dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locked {
    pub name: String,
    pub version: String,
    /// Where the source came from: a path relative to the project, or a
    /// registry URL once one exists.
    pub source: String,
    /// SHA-256 of the dependency's source file, so a changed dependency is
    /// visible rather than silently adopted.
    pub hash: String,
}

/// The whole lockfile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lockfile {
    /// Sorted by name, so writing twice produces the same bytes.
    pub packages: Vec<Locked>,
    /// The version of the lockfile format itself.
    pub format: u32,
}

/// The lockfile format this compiler writes and reads.
pub const FORMAT: u32 = 1;

/// A lockfile problem.
#[derive(Debug, Clone, PartialEq)]
pub struct LockError {
    pub line: usize,
    pub message: String,
}

impl LockError {
    fn at(line: usize, message: impl Into<String>) -> LockError {
        LockError { line, message: message.into() }
    }
}

impl Lockfile {
    /// An empty lockfile in the current format.
    pub fn new() -> Lockfile {
        Lockfile { packages: Vec::new(), format: FORMAT }
    }

    /// Parses a lockfile.
    pub fn parse(text: &str) -> Result<Lockfile, Vec<LockError>> {
        let mut errors = Vec::new();
        let mut lockfile = Lockfile { packages: Vec::new(), format: 0 };
        let mut current: Option<Locked> = None;

        for (index, raw) in text.lines().enumerate() {
            let line_number = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // `version = 1` before any `[[package]]` is the format version; inside
            // one it is the package's own version. Telling them apart by position
            // is the whole difference between the two.
            if current.is_none() {
                if let Some(rest) = line.strip_prefix("version = ") {
                    let rest = rest.trim().trim_matches('"');
                    match rest.parse::<u32>() {
                        Ok(n) => lockfile.format = n,
                        Err(_) => errors.push(LockError::at(
                            line_number,
                            format!("the lockfile format `{rest}` is not a number"),
                        )),
                    }
                    continue;
                }
            }
            if line == "[[package]]" {
                if let Some(package) = current.take() {
                    lockfile.packages.push(package);
                }
                current = Some(Locked {
                    name: String::new(),
                    version: String::new(),
                    source: String::new(),
                    hash: String::new(),
                });
                continue;
            }
            let Some(package) = current.as_mut() else {
                errors.push(LockError::at(
                    line_number,
                    "this line is outside a `[[package]]` block",
                ));
                continue;
            };
            let Some((key, value)) = line.split_once('=') else {
                errors.push(LockError::at(
                    line_number,
                    format!("expected `key = value`, found `{line}`"),
                ));
                continue;
            };
            let key = key.trim();
            let value = unquote(value.trim());
            match key {
                "name" => package.name = value,
                "version" => package.version = value,
                "source" => package.source = value,
                "hash" => package.hash = value,
                other => errors.push(LockError::at(
                    line_number,
                    format!("unknown lockfile key `{other}`"),
                )),
            }
        }
        if let Some(package) = current.take() {
            lockfile.packages.push(package);
        }

        if lockfile.format == 0 {
            errors.push(LockError::at(0, "the lockfile has no format version"));
        } else if lockfile.format != FORMAT {
            errors.push(LockError::at(
                0,
                format!(
                    "lockfile format {} is not the one this compiler writes ({FORMAT})",
                    lockfile.format
                ),
            ));
        }
        for package in &lockfile.packages {
            if package.name.is_empty() {
                errors.push(LockError::at(0, "a locked package has no name"));
            }
            if package.hash.is_empty() {
                errors.push(LockError::at(
                    0,
                    format!("locked package `{}` has no content hash", package.name),
                ));
            }
        }

        // The packages are sorted on write; a hand-edited file may not be.
        lockfile.packages.sort_by(|a, b| a.name.cmp(&b.name));
        if errors.is_empty() {
            Ok(lockfile)
        } else {
            Err(errors)
        }
    }

    /// Renders the lockfile.
    ///
    /// Packages are sorted and fields are written in a fixed order, so the same
    /// resolution produces the same bytes. A lockfile that changes when nothing
    /// changed turns every dependency bump into a diff nobody can read.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("# Generated by umay. Do not edit by hand.\n");
        out.push_str(&format!("version = {}\n", self.format));
        let mut packages = self.packages.clone();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        for package in &packages {
            out.push_str("\n[[package]]\n");
            out.push_str(&format!("name = \"{}\"\n", package.name));
            out.push_str(&format!("version = \"{}\"\n", package.version));
            out.push_str(&format!("source = \"{}\"\n", package.source));
            out.push_str(&format!("hash = \"{}\"\n", package.hash));
        }
        out
    }

    /// The locked entry for a dependency, if there is one.
    pub fn get(&self, name: &str) -> Option<&Locked> {
        self.packages.iter().find(|p| p.name == name)
    }
}

fn unquote(text: &str) -> String {
    text.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(text).to_string()
}

/// What resolving a manifest did.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub lockfile: Lockfile,
    /// Dependencies that could not be resolved, with the reason.
    pub unresolved: Vec<(String, String)>,
}

/// Resolves a manifest's dependencies, and theirs.
///
/// The graph is walked breadth-first from the project's own manifest. Until this
/// walked, only the direct dependencies were locked and a dependency's own
/// dependencies were ignored -- so a lockfile said it had pinned a project's
/// dependencies while silently leaving most of them out.
///
/// Only path dependencies can be resolved, because there is no registry. A
/// dependency with no `path` is reported rather than invented: a lockfile that
/// claims to have pinned something it never looked at is worse than none.
pub fn resolve(project_dir: &Path, manifest: &Manifest) -> Resolution {
    let mut lockfile = Lockfile::new();
    let mut unresolved = Vec::new();
    // Directories already visited, so a diamond does not lock a dependency twice
    // and a cycle does not recurse forever.
    let mut visited: Vec<PathBuf> = Vec::new();
    // The chain that led to the current dependency, for a cycle message a reader
    // can act on.
    let mut stack: Vec<String> = Vec::new();
    // What each name was already resolved to, for a version conflict.
    let mut chosen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    let root = canonical_dir(project_dir);
    walk(
        project_dir,
        manifest,
        &root,
        &mut lockfile,
        &mut unresolved,
        &mut visited,
        &mut stack,
        &mut chosen,
    );

    lockfile.packages.sort_by(|a, b| a.name.cmp(&b.name));
    Resolution { lockfile, unresolved }
}

/// The recursive half of [`resolve`].
///
/// Parameters rather than a struct because the borrow checker is happier and the
/// call sites read the same either way; the doc above says what each is for.
#[allow(clippy::too_many_arguments)]
fn walk(
    base: &Path,
    manifest: &Manifest,
    root: &Path,
    lockfile: &mut Lockfile,
    unresolved: &mut Vec<(String, String)>,
    visited: &mut Vec<PathBuf>,
    stack: &mut Vec<String>,
    chosen: &mut std::collections::BTreeMap<String, String>,
) {
    for dependency in &manifest.dependencies {
        // A dependency with no `path` is fetched from the registry into
        // `project_dir/deps/<name>`; one with a `path` is used where it lies.
        // Either way `source_dir` ends up being the directory that holds the
        // package's `umay.toml`, and `walk` continues from there.
        let source_dir = match &dependency.path {
            Some(relative) => {
                let dir = base.join(relative);
                if !dir.is_dir() {
                    unresolved.push((
                        dependency.name.clone(),
                        format!("`{}` is not a directory", dir.display()),
                    ));
                    continue;
                }
                dir
            }
            None => {
                // The registry answers a name and a version. A manifest that
                // names a version pins it; one that names none asks for the
                // newest the registry holds, which fetch_latest resolves.
                let fetched = if dependency.version.is_empty() {
                    crate::upm::fetch_latest(root, &dependency.name)
                } else {
                    crate::upm::fetch(root, &dependency.name, &dependency.version)
                        .map(|dir| (dir, dependency.version.clone()))
                };
                match fetched {
                    Ok((dir, _version)) => dir,
                    Err(err) => {
                        unresolved.push((dependency.name.clone(), err.message));
                        continue;
                    }
                }
            }
        };
        let canonical = canonical_dir(&source_dir);

        // A dependency that reaches itself is a mistake in the graph, not a
        // program to compile: say which chain closed the loop.
        if canonical == *root {
            let mut chain = stack.clone();
            chain.push(dependency.name.clone());
            unresolved.push((
                dependency.name.clone(),
                format!(
                    "dependency cycle: {} returns to the project itself",
                    chain.join(" -> ")
                ),
            ));
            continue;
        }
        // A dependency already on the stack closes a loop back to itself. That
        // is a mistake in the graph rather than a package that happens to be
        // reached twice, and the message names the chain so it can be found.
        if let Some(seen_at) = stack.iter().position(|seen| *seen == dependency.name) {
            let mut chain: Vec<String> = stack[seen_at..].to_vec();
            chain.push(dependency.name.clone());
            unresolved.push((
                dependency.name.clone(),
                format!("dependency cycle: {}", chain.join(" -> ")),
            ));
            continue;
        }
        if visited.iter().any(|v| *v == canonical) {
            // Reached through another route. Two routes to the same directory is
            // a diamond, which is fine: it is one package.
            continue;
        }

        let hash = match source_hash(&source_dir) {
            Ok(hash) => hash,
            Err(reason) => {
                unresolved.push((dependency.name.clone(), reason));
                continue;
            }
        };

        // The version the dependency's own manifest states is the truth about
        // it; the constraint in the parent's manifest is a request. A
        // disagreement is reported rather than quietly resolved in either
        // direction, because both answers are wrong: one builds a version the
        // author did not ask for, the other edits a file the author did not
        // write.
        let own = manifest::read_project(&source_dir);
        let own_version = match &own {
            Ok(m) => m.package.version.clone(),
            Err(_) => dependency.version.clone(),
        };
        if !dependency.version.is_empty() && !own_version.is_empty() && own_version != dependency.version
        {
            unresolved.push((
                dependency.name.clone(),
                format!(
                    "`{}` is version {}, but the manifest asks for {}",
                    dependency.name, own_version, dependency.version
                ),
            ));
            continue;
        }

        // Two routes to the same *name* with different versions is a real
        // conflict: a program cannot have two `lib`s.
        if let Some(existing) = chosen.get(&dependency.name) {
            if existing != &own_version {
                unresolved.push((
                    dependency.name.clone(),
                    format!(
                        "`{}` is required at both {} and {} through different paths",
                        dependency.name, existing, own_version
                    ),
                ));
                continue;
            }
            continue;
        }
        chosen.insert(dependency.name.clone(), own_version.clone());

        lockfile.packages.push(Locked {
            name: dependency.name.clone(),
            version: own_version,
            // The path is recorded relative to the project that named it, which
            // is also relative to the project being locked, since every path is
            // written from the directory of the manifest that contains it.
            source: normalise_relative(root, &canonical),
            hash,
        });

        visited.push(canonical.clone());
        stack.push(dependency.name.clone());
        if let Ok(own) = own {
            walk(
                &source_dir,
                &own,
                root,
                lockfile,
                unresolved,
                visited,
                stack,
                chosen,
            );
        }
        stack.pop();
    }
}

/// A dependency's path as written from the project being locked.
///
/// The lockfile records where a package came from, and a reader expects a path
/// that means something from the project root -- `../lib`, not
/// `/home/someone/projects/lib`, which would make the lockfile depend on the
/// machine it was written on and break reproducibility.
fn normalise_relative(root: &Path, target: &Path) -> String {
    let from: Vec<std::path::Component> = root.components().collect();
    let to: Vec<std::path::Component> = target.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_string());
    }
    for component in &to[common..] {
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn canonical_dir(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The content hash of a dependency directory.
///
/// Every file is hashed in sorted path order, with its name and length, so that
/// adding, removing or renaming a file changes the hash. This is how a path
/// dependency's change becomes visible: the lockfile notices rather than quietly
/// using whatever is on disk.
pub fn source_hash(dir: &Path) -> Result<String, String> {
    if !dir.is_dir() {
        return Err(format!("`{}` is not a directory", dir.display()));
    }
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();

    let mut blob = Vec::new();
    for relative in &files {
        let bytes = std::fs::read(dir.join(relative)).map_err(|e| {
            format!("cannot read `{}`: {e}", dir.join(relative).display())
        })?;
        blob.extend_from_slice(relative.as_bytes());
        blob.push(0);
        blob.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        blob.extend_from_slice(&bytes);
    }
    Ok(sha256::hex(&blob))
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // A dependency's own `dist/` is output, not source.
            if path.file_name().is_some_and(|n| n == "dist" || n == ".git") {
                continue;
            }
            collect(root, &path, out)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Reads a project's lockfile, or an empty one if it has none.
pub fn read_project(project_dir: &Path) -> Result<Lockfile, Vec<LockError>> {
    let path = project_dir.join("umay.lock");
    match std::fs::read_to_string(&path) {
        Ok(text) => Lockfile::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lockfile::new()),
        Err(e) => Err(vec![LockError::at(
            0,
            format!("cannot read `{}`: {e}", path.display()),
        )]),
    }
}

/// Checks a lockfile against the project on disk.
///
/// Reports a dependency whose content no longer matches what was locked. That is
/// the check that makes a lockfile worth having: without it the file records a
/// promise nobody verifies.
pub fn verify(project_dir: &Path, lockfile: &Lockfile) -> Vec<String> {
    let mut problems = Vec::new();
    for package in &lockfile.packages {
        let source_dir = project_dir.join(&package.source);
        match source_hash(&source_dir) {
            Ok(hash) if hash == package.hash => {}
            Ok(_) => problems.push(format!(
                "`{}` no longer matches `umay.lock` -- run `umay lock` to update it",
                package.name
            )),
            Err(reason) => {
                problems.push(format!("`{}`: {reason}", package.name));
            }
        }
    }
    problems
}

/// The manifest and lockfile of a project, read together.
pub fn load_project(project_dir: &Path) -> Result<(Manifest, Lockfile), String> {
    let manifest = manifest::read_project(project_dir).map_err(|errors| {
        errors
            .iter()
            .map(|e| {
                if e.line > 0 {
                    format!("umay.toml:{}: {}", e.line, e.message)
                } else {
                    format!("umay.toml: {}", e.message)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let lockfile = read_project(project_dir).map_err(|errors| {
        errors
            .iter()
            .map(|e| {
                if e.line > 0 {
                    format!("umay.lock:{}: {}", e.line, e.message)
                } else {
                    format!("umay.lock: {}", e.message)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    Ok((manifest, lockfile))
}

/// The directory where a fetched dependency would be unpacked.
pub fn dependency_dir(project_dir: &Path, name: &str) -> PathBuf {
    project_dir.join("deps").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "version = 1\n\n[[package]]\nname = \"lib\"\nversion = \"1.2.3\"\nsource = \"./lib\"\nhash = \"deadbeef\"\n";

    #[test]
    fn a_package_version_is_not_the_format_version() {
        // Both lines say `version =`. Reading the first one anywhere let a
        // package's own version be parsed as the format and rejected.
        let lockfile = Lockfile::parse(ONE).expect("should parse");
        assert_eq!(lockfile.format, 1);
        assert_eq!(lockfile.packages[0].version, "1.2.3");
    }

    #[test]
    fn the_lockfile_round_trips() {
        let lockfile = Lockfile::parse(ONE).expect("should parse");
        let text = lockfile.to_text();
        let again = Lockfile::parse(&text).expect("should parse its own output");
        assert_eq!(lockfile, again);
    }

    #[test]
    fn packages_are_written_in_a_fixed_order() {
        let a = Lockfile {
            format: 1,
            packages: vec![
                Locked { name: "b".into(), version: "1".into(), source: "./b".into(), hash: "h".into() },
                Locked { name: "a".into(), version: "1".into(), source: "./a".into(), hash: "h".into() },
            ],
        };
        let b = Lockfile {
            format: 1,
            packages: vec![
                Locked { name: "a".into(), version: "1".into(), source: "./a".into(), hash: "h".into() },
                Locked { name: "b".into(), version: "1".into(), source: "./b".into(), hash: "h".into() },
            ],
        };
        assert_eq!(a.to_text(), b.to_text(), "the same set must write the same bytes");
    }

    #[test]
    fn an_empty_lockfile_says_so() {
        let lockfile = Lockfile::new();
        assert!(Lockfile::parse(&lockfile.to_text()).is_ok());
        assert!(lockfile.packages.is_empty());
    }

    #[test]
    fn a_package_without_a_hash_is_refused() {
        let text = "version = 1\n\n[[package]]\nname = \"lib\"\nversion = \"1.0.0\"\nsource = \"./lib\"\n";
        let errors = Lockfile::parse(text).expect_err("a package must record what it pinned");
        assert!(errors.iter().any(|e| e.message.contains("no content hash")), "got {errors:?}");
    }

    #[test]
    fn a_hand_edited_order_is_accepted_and_normalised() {
        let text = "version = 1\n\n[[package]]\nname = \"z\"\nversion = \"1\"\nsource = \"./z\"\nhash = \"h\"\n\n[[package]]\nname = \"a\"\nversion = \"1\"\nsource = \"./a\"\nhash = \"h\"\n";
        let lockfile = Lockfile::parse(text).expect("order is not a problem");
        assert_eq!(lockfile.packages[0].name, "a", "packages should be sorted on read");
    }
}
