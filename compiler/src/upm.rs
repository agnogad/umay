//! `upm`: fetching a dependency from a registry.
//!
//! Until this existed, a dependency with no `path` was reported and nothing was
//! written, because there was no registry to fetch from. The lockfile argument
//! was sound -- a file that claims to pin something it never looked at is worse
//! than none -- but it left the manifest able to say `lib = "1.0.0"` with no way
//! to honour it.
//!
//! The registry is a directory of packages, addressed by name and version:
//!
//! ```text
//! <registry>/
//!     lib/
//!         1.0.0/
//!             umay.toml
//!             src/lib.um
//!         1.2.0/
//!             ...
//! ```
//!
//! Its location comes from `UMAY_REGISTRY`, an environment variable holding one
//! or more roots separated by the platform's path separator. Each root is a
//! local directory; a URL form is not accepted, because the variable is split on
//! `:` and a scheme would be torn from its path, leaving two roots that exist
//! nowhere. A fetch that silently finds nothing is the very failure this module
//! exists to remove.
//!
//! Fetching copies the package into the project's `deps/<name>/` and returns the
//! directory. The caller hashes it and writes the hash into the lockfile, so a
//! later `build` verifies the content it actually built and not the version
//! someone typed.

use std::path::{Path, PathBuf};

use crate::lock::source_hash;

/// Where a fetch failed, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchError {
    pub message: String,
}

impl FetchError {
    fn new(message: impl Into<String>) -> FetchError {
        FetchError { message: message.into() }
    }
}

/// The registry roots named by `UMAY_REGISTRY`, in order.
///
/// A root prefixed with `file:` is a URL form of a local directory; anything
/// else is read as a plain path. When the variable is unset this is empty, which
/// is what tells a caller there is nowhere to fetch from.
pub fn registry_roots() -> Vec<PathBuf> {
    match std::env::var("UMAY_REGISTRY") {
        Ok(value) => split_roots(&value),
        Err(_) => Vec::new(),
    }
}

/// Splits the registry variable on the platform path separator.
///
/// Each entry is a directory path. A `file:` URL is not accepted: the registry
/// variable is split on `:`, so a scheme would be torn from its path and the
/// result would be two nonsense roots -- a fetch that silently finds nothing is
/// the failure this module exists to remove, so the format is kept to plain
/// directories a reader can recognise.
fn split_roots(value: &str) -> Vec<PathBuf> {
    std::env::split_paths(value)
        .map(|part| PathBuf::from(part.to_string_lossy().trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// Fetches `name` at `version` into `project_dir/deps/<name>`.
///
/// Returns the directory the package was unpacked into. The package must exist
/// at exactly `name/version` in some registry root; a version range is resolved
/// by the caller, not here, because choosing among versions needs the versions a
/// registry actually holds and that is the caller's question.
///
/// A package already present at the destination is replaced, so a re-fetch after
/// a registry update picks up the new content rather than keeping the old.
pub fn fetch(project_dir: &Path, name: &str, version: &str) -> Result<PathBuf, FetchError> {
    if name.is_empty() {
        return Err(FetchError::new("a dependency has no name"));
    }
    if !is_safe_name(name) {
        return Err(FetchError::new(format!(
            "`{name}` is not a usable package name: letters, digits, `-` and `_` only"
        )));
    }

    let roots = registry_roots();
    if roots.is_empty() {
        return Err(FetchError::new(format!(
            "cannot fetch `{name}`: `UMAY_REGISTRY` names no registry to fetch from"
        )));
    }

    // Look for the package in each root, in order. The first one that has it
    // wins, so a project can put a private registry ahead of a shared one.
    let mut found: Option<PathBuf> = None;
    let mut tried: Vec<String> = Vec::new();
    for root in &roots {
        let candidate = root.join(name).join(version);
        if candidate.is_dir() {
            found = Some(candidate);
            break;
        }
        tried.push(candidate.display().to_string());
    }

    let Some(source) = found else {
        return Err(FetchError::new(format!(
            "`{name}` version `{version}` is in no registry (looked in: {})",
            tried.join(", ")
        )));
    };

    // A package without a manifest is not a package. Catching it here names the
    // registry entry rather than failing later inside the compiler with a path
    // the user may never have written.
    if !source.join("umay.toml").is_file() {
        return Err(FetchError::new(format!(
            "`{}` has no `umay.toml`",
            source.display()
        )));
    }

    let dest = crate::lock::dependency_dir(project_dir, name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| {
            FetchError::new(format!("cannot replace `{}`: {e}", dest.display()))
        })?;
    }
    copy_tree(&source, &dest)
        .map_err(|e| FetchError::new(format!("cannot copy `{}`: {e}", source.display())))?;

    Ok(dest)
}

/// Fetches `name` at any version the registry holds, newest first by the
/// registry's own ordering, when the manifest named a package without pinning a
/// version.
///
/// Versions are compared as dot-separated numbers where they are numeric, so
/// `1.10.0` sorts above `1.9.0` rather than below it as a plain string would.
pub fn fetch_latest(project_dir: &Path, name: &str) -> Result<(PathBuf, String), FetchError> {
    if !is_safe_name(name) {
        return Err(FetchError::new(format!(
            "`{name}` is not a usable package name: letters, digits, `-` and `_` only"
        )));
    }
    let roots = registry_roots();
    if roots.is_empty() {
        return Err(FetchError::new(format!(
            "cannot fetch `{name}`: `UMAY_REGISTRY` names no registry to fetch from"
        )));
    }

    let mut versions: Vec<String> = Vec::new();
    for root in &roots {
        let package_dir = root.join(name);
        let Ok(entries) = std::fs::read_dir(&package_dir) else { continue };
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if !versions.iter().any(|v| v == name) {
                        versions.push(name.to_string());
                    }
                }
            }
        }
    }
    if versions.is_empty() {
        return Err(FetchError::new(format!(
            "`{name}` is in no registry named by `UMAY_REGISTRY`"
        )));
    }
    versions.sort_by(|a, b| compare_versions(a, b));
    let version = versions.pop().expect("non-empty");
    let dir = fetch(project_dir, name, &version)?;
    Ok((dir, version))
}

/// A fetched package's content hash, for the lockfile.
pub fn fetched_hash(dir: &Path) -> Result<String, FetchError> {
    source_hash(dir).map_err(FetchError::new)
}

/// Whether a name is safe to use as one path component.
///
/// The name becomes a directory under `deps/`, so a `/` or `..` in it would
/// escape the project. Restricting it to letters, digits, `-` and `_` is a rule
/// short enough to keep in one place.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Copies a directory tree, creating the destination.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Compares two dotted version strings.
///
/// Numeric components compare as numbers (`1.10.0` above `1.9.0`); anything that
/// is not a number falls back to a string comparison for that component, so an
/// odd version cannot panic the sort.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<&str> = a.split('.').collect();
    let pb: Vec<&str> = b.split('.').collect();
    for (x, y) in pa.iter().zip(pb.iter()) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) => nx.cmp(&ny),
            _ => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    pa.len().cmp(&pb.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("umay_upm_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_version_compares_by_number_not_string() {
        assert_eq!(compare_versions("1.10.0", "1.9.0"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("2.0.0", "1.99.0"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn an_unsafe_name_is_refused() {
        assert!(!is_safe_name("../escape"));
        assert!(!is_safe_name("a/b"));
        assert!(!is_safe_name(""));
        assert!(is_safe_name("net-util_2"));
    }

    #[test]
    fn roots_are_split_on_the_path_separator() {
        // `split_paths` uses `;` on Windows and `:` elsewhere, so the joined
        // form comes from the same API the reader uses.
        let joined = std::env::join_paths([Path::new("/a"), Path::new("/b")]).expect("join roots");
        let roots = split_roots(joined.to_str().expect("roots are UTF-8"));
        assert_eq!(roots, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(split_roots("/only"), vec![PathBuf::from("/only")]);
    }

    #[test]
    fn fetching_without_a_registry_is_an_error() {
        // No registry named, so this must say so rather than return silently.
        let project = scratch("noreg");
        // Guard: only run the assertion when the environment is genuinely unset.
        if std::env::var("UMAY_REGISTRY").is_err() {
            let err = fetch(&project, "lib", "1.0.0").unwrap_err();
            assert!(err.message.contains("UMAY_REGISTRY"), "{}", err.message);
        }
    }
}
