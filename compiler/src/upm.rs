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

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::lock::source_hash;

/// Where a fetch failed, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchError {
    pub message: String,
}

impl FetchError {
    fn new(message: impl Into<String>) -> FetchError {
        FetchError {
            message: message.into(),
        }
    }
}

/// One entry of `UMAY_REGISTRY`: a local directory or an HTTP(S) base URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryRoot {
    Local(PathBuf),
    Url(String),
}

impl RegistryRoot {
    fn label(&self) -> String {
        match self {
            RegistryRoot::Local(path) => path.display().to_string(),
            RegistryRoot::Url(url) => url.clone(),
        }
    }
}

/// Every registry entry, in order.
///
/// A value containing `://` is split on `;` so a URL keeps its colon; without a
/// scheme the platform path separator is used, preserving the historical
/// directory form. Local and URL entries may be mixed in the `;` form.
pub fn registry_entries() -> Vec<RegistryRoot> {
    match std::env::var("UMAY_REGISTRY") {
        Ok(value) => parse_registry(&value),
        Err(_) => Vec::new(),
    }
}

fn parse_registry(value: &str) -> Vec<RegistryRoot> {
    if value.contains("://") {
        value
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                if part.starts_with("http://") || part.starts_with("https://") {
                    RegistryRoot::Url(part.trim_end_matches('/').to_string())
                } else {
                    RegistryRoot::Local(PathBuf::from(part))
                }
            })
            .collect()
    } else {
        std::env::split_paths(value)
            .map(|part| RegistryRoot::Local(PathBuf::from(part.to_string_lossy().trim())))
            .filter(|root| match root {
                RegistryRoot::Local(path) => !path.as_os_str().is_empty(),
                RegistryRoot::Url(_) => true,
            })
            .collect()
    }
}

/// Labels for `umay doctor`.
pub fn registry_labels() -> Vec<String> {
    registry_entries().iter().map(RegistryRoot::label).collect()
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

    let roots = registry_entries();
    if roots.is_empty() {
        return Err(FetchError::new(format!(
            "cannot fetch `{name}`: `UMAY_REGISTRY` names no registry to fetch from"
        )));
    }
    if version.is_empty() || !is_safe_version(version) {
        return Err(FetchError::new(format!(
            "`{version}` is not a usable version"
        )));
    }

    let dest = crate::lock::dependency_dir(project_dir, name);
    let mut tried = Vec::new();
    for root in &roots {
        match root {
            RegistryRoot::Local(path) => {
                let candidate = path.join(name).join(version);
                tried.push(candidate.display().to_string());
                if !candidate.is_dir() {
                    continue;
                }
                if !candidate.join("umay.toml").is_file() {
                    return Err(FetchError::new(format!(
                        "`{}` has no `umay.toml`",
                        candidate.display()
                    )));
                }
                if dest.exists() {
                    std::fs::remove_dir_all(&dest).map_err(|e| {
                        FetchError::new(format!("cannot replace `{}`: {e}", dest.display()))
                    })?;
                }
                copy_tree(&candidate, &dest).map_err(|e| {
                    FetchError::new(format!("cannot copy `{}`: {e}", candidate.display()))
                })?;
                return Ok(dest);
            }
            RegistryRoot::Url(base) => {
                let url = format!("{base}/{name}/{version}.tar");
                tried.push(url.clone());
                let Some(tar) = fetch_bytes(&url)? else {
                    continue;
                };
                let dest = crate::lock::dependency_dir(project_dir, name);
                if dest.exists() {
                    std::fs::remove_dir_all(&dest).map_err(|e| {
                        FetchError::new(format!("cannot replace `{}`: {e}", dest.display()))
                    })?;
                }
                extract_tar(&tar, &dest).map_err(|e| {
                    FetchError::new(format!("cannot unpack `{name}` {version}: {e}"))
                })?;
                if !dest.join("umay.toml").is_file() {
                    return Err(FetchError::new(format!(
                        "`{name}` version `{version}` from {base} has no `umay.toml`"
                    )));
                }
                return Ok(dest);
            }
        }
    }

    Err(FetchError::new(format!(
        "`{name}` version `{version}` is in no registry (looked in: {})",
        tried.join(", ")
    )))
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
    let roots = registry_entries();
    if roots.is_empty() {
        return Err(FetchError::new(format!(
            "cannot fetch `{name}`: `UMAY_REGISTRY` names no registry to fetch from"
        )));
    }

    let mut versions: Vec<String> = Vec::new();
    for root in &roots {
        match root {
            RegistryRoot::Local(path) => {
                let package_dir = path.join(name);
                let Ok(entries) = std::fs::read_dir(&package_dir) else {
                    continue;
                };
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
            RegistryRoot::Url(base) => {
                let url = format!("{base}/{name}/versions.txt");
                if let Some(text) = fetch_text(&url)? {
                    for line in text.lines() {
                        let version = line.trim();
                        if is_safe_version(version) && !versions.iter().any(|v| v == version) {
                            versions.push(version.to_string());
                        }
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

fn is_safe_version(version: &str) -> bool {
    !version.is_empty()
        && version != ".."
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

fn fetch_bytes(url: &str) -> Result<Option<Vec<u8>>, FetchError> {
    let output = Command::new("curl")
        .args(["-sSL", "--max-time", "60", "-w", "\n%{http_code}", url])
        .output()
        .map_err(|error| FetchError::new(format!("cannot run `curl` to fetch {url}: {error}")))?;
    if !output.status.success() {
        return Err(FetchError::new(format!(
            "cannot fetch {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mut body = output.stdout;
    let split = body.iter().rposition(|byte| *byte == b'\n').unwrap_or(0);
    let status = String::from_utf8_lossy(&body[split + 1..])
        .trim()
        .to_string();
    body.truncate(split);
    match status.as_str() {
        "200" => Ok(Some(body)),
        // A package missing from one root lets the next root answer.
        "404" => Ok(None),
        other => Err(FetchError::new(format!("cannot fetch {url}: HTTP {other}"))),
    }
}

fn fetch_text(url: &str) -> Result<Option<String>, FetchError> {
    fetch_bytes(url).map(|bytes| bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

fn tar_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn tar_size(bytes: &[u8]) -> Result<u64, String> {
    let text = tar_string(bytes).trim().to_string();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(&text, 8).map_err(|_| format!("bad tar size `{text}`"))
}

fn safe_tar_path(name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return Err(format!("tar entry escapes the package: `{name}`")),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("tar entry has an empty path".into());
    }
    Ok(out)
}

/// Extracts an uncompressed USTAR archive.
///
/// The registry server emits USTAR, whose names fit the header's fields. Other
/// entry types are skipped rather than followed: a package is data, not a set
/// of links or devices.
fn extract_tar(bytes: &[u8], dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut offset = 0usize;
    while offset + 512 <= bytes.len() {
        let header = &bytes[offset..offset + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let name = tar_string(&header[0..100]);
        let prefix = tar_string(&header[345..500]);
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let size = tar_size(&header[124..136])?;
        let kind = header[156];
        let data_start = offset + 512;
        let data_end = data_start
            .checked_add(size as usize)
            .ok_or_else(|| "tar entry size overflows".to_string())?;
        if data_end > bytes.len() {
            return Err("tar entry is truncated".into());
        }
        if kind == b'5' || full.ends_with('/') {
            std::fs::create_dir_all(dest.join(safe_tar_path(full.trim_end_matches('/'))?))
                .map_err(|e| e.to_string())?;
        } else if kind == b'0' || kind == 0 {
            let path = dest.join(safe_tar_path(&full)?);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, &bytes[data_start..data_end]).map_err(|e| e.to_string())?;
        }
        let padded = (size as usize + 511) & !511;
        offset = data_start + padded;
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
        assert_eq!(
            compare_versions("1.10.0", "1.9.0"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("1.0.0", "1.0.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_versions("2.0.0", "1.99.0"),
            std::cmp::Ordering::Greater
        );
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
        let roots = parse_registry(joined.to_str().expect("roots are UTF-8"));
        assert_eq!(
            roots,
            vec![
                RegistryRoot::Local(PathBuf::from("/a")),
                RegistryRoot::Local(PathBuf::from("/b"))
            ]
        );
        assert_eq!(
            parse_registry("/only"),
            vec![RegistryRoot::Local(PathBuf::from("/only"))]
        );
    }

    #[test]
    fn urls_and_directories_can_share_a_registry_list() {
        let entries = parse_registry("http://example.test/reg;https://secure.test/reg;/tmp/local");
        assert_eq!(
            entries,
            vec![
                RegistryRoot::Url("http://example.test/reg".into()),
                RegistryRoot::Url("https://secure.test/reg".into()),
                RegistryRoot::Local(PathBuf::from("/tmp/local")),
            ]
        );
    }

    #[test]
    fn a_version_that_could_escape_the_path_is_refused() {
        assert!(!is_safe_version("../x"));
        assert!(!is_safe_version("a/b"));
        assert!(is_safe_version("1.10.0-rc1"));
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
