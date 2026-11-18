//! Resolving `[native]` dependencies for one target.
//!
//! This module reads manifest data and queries `pkg-config` with
//! `std::process::Command`. It never runs a shell and never executes a build
//! script: a build script is arbitrary code, and running it is a separate,
//! trust-boundary slice (`docs/v2/native-dependencies.md` §3 and §7). The
//! `build_script` field in the manifest is exposed here as data only.

use std::process::Command;

use crate::manifest::{Build, Manifest, NativeDependency};

/// A native dependency resolved for one target triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNative {
    /// The `[native.<name>]` key.
    pub name: String,
    /// The exact triple this was resolved for.
    pub target: String,
    /// The pkg-config package queried, if any.
    pub pkg_config: Option<String>,
    /// Include paths from both pkg-config `--cflags` and the manifest.
    pub include_paths: Vec<String>,
    /// Linker flags from both pkg-config `--libs` and the manifest.
    pub link_flags: Vec<String>,
    pub r#static: bool,
    /// The exact commands run, for diagnostics (`umay doctor`) and the cache.
    pub commands: Vec<String>,
}

impl ResolvedNative {
    /// A stable contribution to the build cache key.
    ///
    /// Sorted and deduplicated, so reordering flags in the manifest does not
    /// rebuild. No absolute paths are normalised here: the caller folds this
    /// into `CacheKey::derive_for`'s `extra` inputs and keeps paths relative to
    /// the project root (docs/v2/native-dependencies.md §4).
    pub fn cache_key(&self) -> String {
        let includes = unique_sorted(&self.include_paths);
        let flags = unique_sorted(&self.link_flags);
        format!(
            "native:{}|{}|{}|{}|{}|{}",
            self.name,
            self.target,
            if self.r#static { "static" } else { "dynamic" },
            self.pkg_config.as_deref().unwrap_or(""),
            includes.join(","),
            flags.join(","),
        )
    }
}

/// The base and override fields that actually drive resolution.
struct Effective {
    pkg_config: Option<String>,
    include_paths: Vec<String>,
    link_flags: Vec<String>,
    r#static: bool,
}

fn effective_for(dep: &NativeDependency, target: &str) -> Effective {
    // A matching override replaces the base entry wholesale; overrides do not
    // chain with the base or with each other (design draft §2).
    if let Some(ov) = dep.target_overrides.get(target) {
        Effective {
            pkg_config: ov.pkg_config.clone(),
            include_paths: ov.include_paths.clone(),
            link_flags: ov.link_flags.clone(),
            r#static: ov.r#static,
        }
    } else {
        Effective {
            pkg_config: dep.pkg_config.clone(),
            include_paths: dep.include_paths.clone(),
            link_flags: dep.link_flags.clone(),
            r#static: dep.r#static,
        }
    }
}

/// Resolves one native dependency for `target` (an exact target triple).
///
/// Returns an actionable error string on a missing or failing `pkg-config`:
/// it names the library, the target, the command that failed, and the manifest
/// keys that can satisfy the dependency. Nothing here panics.
pub fn resolve(dep: &NativeDependency, target: &str) -> Result<ResolvedNative, String> {
    let eff = effective_for(dep, target);
    let mut include_paths = Vec::new();
    let mut link_flags = Vec::new();
    let mut commands = Vec::new();

    if let Some(pkg) = eff.pkg_config.as_deref().filter(|p| !p.trim().is_empty()) {
        let cflags = pkg_config_query(pkg, "--cflags", eff.r#static, dep, target, &mut commands)?;
        for token in cflags.split_whitespace() {
            if let Some(path) = token.strip_prefix("-I") {
                push_unique(&mut include_paths, path.to_string());
            } else {
                // The manifest model has no separate preprocessor-flag field,
                // so non-include cflags (e.g. `-D...`) travel with link flags.
                push_unique(&mut link_flags, token.to_string());
            }
        }
        let libs = pkg_config_query(pkg, "--libs", eff.r#static, dep, target, &mut commands)?;
        for token in libs.split_whitespace() {
            push_unique(&mut link_flags, token.to_string());
        }
    }

    // Explicit manifest values are merged last and deduplicated; they are the
    // user's override for a library whose metadata is incomplete or absent.
    for path in &eff.include_paths {
        push_unique(&mut include_paths, path.clone());
    }
    for flag in &eff.link_flags {
        push_unique(&mut link_flags, flag.clone());
    }

    Ok(ResolvedNative {
        name: dep.name.clone(),
        target: target.to_string(),
        pkg_config: eff.pkg_config,
        include_paths,
        link_flags,
        r#static: eff.r#static,
        commands,
    })
}

/// Resolves every declared native dependency in manifest order.
pub fn resolve_all(manifest: &Manifest, target: &str) -> Result<Vec<ResolvedNative>, String> {
    manifest.native.iter().map(|dep| resolve(dep, target)).collect()
}

/// The root project's declared build script, as data.
///
/// This slice deliberately does not execute it, parse its stdout, or forward
/// any environment to it. See `docs/v2/native-dependencies.md` §3 and §7.
pub fn build_script_path(build: &Build) -> Option<&str> {
    build.build_script.as_deref()
}

fn pkg_config_query(
    pkg: &str,
    kind: &str,
    is_static: bool,
    dep: &NativeDependency,
    target: &str,
    commands: &mut Vec<String>,
) -> Result<String, String> {
    let mut args = vec![kind.to_string()];
    if is_static {
        args.push("--static".to_string());
    }
    args.push(pkg.to_string());

    let shown = format!("pkg-config {}", args.join(" "));
    commands.push(shown.clone());

    let output = Command::new("pkg-config")
        .args(&args)
        .output()
        .map_err(|e| missing_error(dep, target, &shown, &format!("cannot run pkg-config: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        };
        return Err(missing_error(dep, target, &shown, &detail));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn missing_error(dep: &NativeDependency, target: &str, command: &str, detail: &str) -> String {
    format!(
        "native dependency `{}` for target `{}`: `{command}` failed ({detail}). \
         Install the development package that provides `{}`, or set \
         `pkg-config = \"\"` with explicit `include-paths` and `link-flags` \
         in [native.{}] (or [native.{}.target.{}] for this target).",
        dep.name, target, dep.name, dep.name, dep.name, target
    )
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

fn unique_sorted(items: &[String]) -> Vec<String> {
    let mut out = items.to_vec();
    out.sort();
    out.dedup();
    out
}
