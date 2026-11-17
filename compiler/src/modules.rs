//! Loading modules from files.
//!
//! UMAY writes `mod name;` to mean "the items in `name.um` belong here". Until
//! this module existed, that line parsed to nothing: the name was recorded and
//! no file was read, so a program that used the module reported `unknown name`
//! whether the file was present or not. A syntax that is accepted and does
//! nothing is worse than one that is rejected, because it cannot be debugged.
//!
//! Lookup is `dir/name.um` first, then `dir/name/mod.um`. The first that exists
//! wins; if neither does, the error names both paths, because the author has to
//! know which file the compiler wanted.
//!
//! Modules are resolved after parsing and before the prelude is prepended, so by
//! the time anything downstream looks at the program every module is an ordinary
//! list of items. Semantic analysis, metadata and the language server therefore
//! need no knowledge of files at all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::ast::{Item, ModDecl, Program};
use crate::diagnostics::Diagnostics;
use crate::source::Span;
use crate::{parse_file, SourceMap};

/// Loads every `mod name;` in `program` from the file system.
///
/// `base` is the directory module files are looked up in. `map` gains one entry
/// per file loaded, so a diagnostic inside a module points at that module's own
/// text rather than at the file that declared it.
pub fn load_modules(program: Program, base: &Path, map: &mut SourceMap) -> (Program, Diagnostics) {
    load_modules_full(program, base, map, &[])
}

fn load_modules_full(
    program: Program,
    base: &Path,
    map: &mut SourceMap,
    dependencies: &[DependencyPath],
) -> (Program, Diagnostics) {
    let mut loader = Loader {
        map,
        diagnostics: Diagnostics::new(),
        loading: BTreeSet::new(),
        loaded: BTreeSet::new(),
        dependencies,
    };
    let items = loader.expand(program.items, base);
    (Program { items, span: program.span }, loader.diagnostics)
}

/// Loads modules for a program whose entry point is the file at `entry`.
pub fn load_from_entry(program: Program, entry: &Path, map: &mut SourceMap) -> (Program, Diagnostics) {
    load_from_entry_with_deps(program, entry, map, &[])
}

/// Where a declared dependency's modules live.
///
/// A dependency is not a directory next to the program: it is a project of its
/// own, with a manifest and a `src/`. Naming it in `[dependencies]` is what makes
/// `mod lib;` resolve to `../lib/src/lib.um`, rather than the author having to
/// write a path the manifest already knows.
#[derive(Debug, Clone)]
pub struct DependencyPath {
    /// The name written in `[dependencies]`.
    pub name: String,
    /// The dependency's `src/` directory, resolved against the project.
    pub source_dir: PathBuf,
}

/// The dependencies named by the manifest that governs `source_dir`, as module
/// paths.
///
/// The manifest is not necessarily beside the source being compiled: a project
/// entry point is `src/main.um`, so the directory holding `umay.toml` is its
/// parent. Looking only in the source's own directory finds no manifest, and the
/// dependency silently stops resolving -- which is what the first version of this
/// did, and it looked exactly like "the feature does not work".
///
/// Returns nothing when there is no manifest, which is the ordinary case for a
/// single file: a program with no `umay.toml` has no dependencies to resolve, and
/// demanding one would turn every standalone file into a manifest error.
pub fn dependencies_of_project(source_dir: &Path) -> Vec<DependencyPath> {
    let Some((project_dir, manifest)) = find_manifest(source_dir) else {
        return Vec::new();
    };
    manifest
        .dependencies
        .iter()
        .map(|dependency| {
            // A path dependency lives where the manifest points. A fetched one
            // was unpacked under `deps/<name>`, whose `src/` is its module root,
            // the same shape a path dependency has.
            let source_dir = match &dependency.path {
                Some(path) => project_dir.join(path).join("src"),
                None => project_dir.join("deps").join(&dependency.name).join("src"),
            };
            DependencyPath {
                name: dependency.name.clone(),
                source_dir,
            }
        })
        .collect()
}

/// The nearest manifest, walking up from `dir`.
///
/// Bounded to a few levels: a stray `umay.toml` in an ancestor directory should
/// not silently govern an unrelated file, and a program deep in a home directory
/// should not pick up whatever happens to be there.
fn find_manifest(dir: &Path) -> Option<(PathBuf, crate::manifest::Manifest)> {
    let mut current = Some(dir.to_path_buf());
    for _ in 0..3 {
        let directory = current?;
        if let Ok(manifest) = crate::manifest::read_project(&directory) {
            return Some((directory, manifest));
        }
        current = directory.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Loads modules, resolving `mod name;` against declared dependencies too.
pub fn load_from_entry_with_deps(
    program: Program,
    entry: &Path,
    map: &mut SourceMap,
    dependencies: &[DependencyPath],
) -> (Program, Diagnostics) {
    let base = entry.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    // A program at `src/main.um` resolves `mod util;` to `src/util.um`. A
    // dependency named `lib` resolves to `../lib/src/lib.um`; its own modules
    // then come from `../lib/src/`.
    let deps: Vec<DependencyPath> = dependencies.to_vec();
    load_modules_full(program, &base, map, &deps)
}

/// The state one load carries through the graph.
///
/// A struct rather than seven parameters: every recursive call passed the same
/// list through, and adding the dependency list to it made the parameter list
/// longer than the function's first screen.
struct Loader<'a> {
    map: &'a mut SourceMap,
    diagnostics: Diagnostics,
    loading: BTreeSet<PathBuf>,
    loaded: BTreeSet<PathBuf>,
    /// Declared dependencies, so `mod lib;` finds `../lib/src/lib.um`.
    dependencies: &'a [DependencyPath],
}

impl Loader<'_> {
fn expand(&mut self, items: Vec<Item>, dir: &Path) -> Vec<Item> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Item::Mod(module) = &item else {
            out.push(item);
            continue;
        };

        // `mod foo { ... }` is already complete, but its contents may still name
        // files, and those resolve relative to this file's directory.
        if let Some(inline) = module.items.clone() {
            let expanded = self.expand(inline, dir);
            out.push(Item::Mod(ModDecl { items: Some(expanded), ..module.clone() }));
            continue;
        }

        let found = find_module_file(dir, &module.name)
            .or_else(|| self.find_in_dependencies(dir, &module.name));
        let Some(path) = found else {
            let first = dir.join(format!("{}.um", module.name));
            self.diagnostics.push(
                crate::diagnostics::Diagnostic::error(
                    "E0193",
                    format!("module `{}` has no file", module.name),
                )
                .at(module.name_span)
                .with_cause(format!(
                    "looked for `{}` and `{}`; create one, or write `mod {}` with a body to keep its items in this file",
                    first.display(),
                    dir.join(&module.name).join("mod.um").display(),
                    module.name
                )),
            );
            continue;
        };

        // A module that includes itself, directly or through another, would
        // otherwise expand until the stack runs out.
        if self.loading.contains(&path) {
            self.diagnostics.push(
                crate::diagnostics::Diagnostic::error(
                    "E0192",
                    format!("module `{}` includes itself", module.name),
                )
                .at(module.name_span),
            );
            continue;
        }
        // The same file named twice is loaded once: two copies of the same items
        // would be duplicate definitions of everything in it.
        if self.loaded.contains(&path) {
            continue;
        }
        self.loaded.insert(path.clone());

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                self.diagnostics.push(
                    crate::diagnostics::Diagnostic::error(
                        "E0194",
                        format!("cannot read `{}`: {e}", path.display()),
                    )
                    .at(module.name_span),
                );
                continue;
            }
        };
        let id = self.map.add(path.to_string_lossy().into_owned(), text);
        let parsed = parse_file(self.map, id);
        for diagnostic in parsed.diagnostics.items() {
            self.diagnostics.push(diagnostic.clone());
        }

        self.loading.insert(path.clone());
        // Nested modules resolve against the directory the module file lives in,
        // which is either its parent or, for `a/mod.um`, `a` itself.
        let nested_dir = module_dir(&path);
        let expanded = self.expand(parsed.program.items, &nested_dir);
        self.loading.remove(&path);

        // Keep the module's own name so `foo::bar` still resolves: the loaded
        // items belong to `mod foo`, not to the file that declared it. The span
        // stays the one in the declaring file, so a name-path error points where
        // the author wrote the name.
        out.push(Item::Mod(ModDecl { items: Some(expanded), ..module.clone() }));
    }
    out
}

    /// A declared dependency's source, when a local file did not match.
    ///
    /// The author named the dependency in `umay.toml`; making them repeat its
    /// path in `mod lib;` would be asking for information the manifest already
    /// has. The name has to match exactly, so a typo fails rather than quietly
    /// reaching some other package.
    fn find_in_dependencies(&self, dir: &Path, name: &str) -> Option<PathBuf> {
        let dependency = self.dependencies.iter().find(|dep| dep.name == name)?;
        // A dependency is a project: its modules are in its own `src/`, which is
        // where its manifest's entry point and library both live.
        find_module_file(&dependency.source_dir, name).or_else(|| {
            // A dependency may name its library file something else, so the last
            // resort is any single source file the manifest points at.
            let _ = dir;
            None
        })
    }
}

/// The directory a module file's own `mod` declarations resolve against.
///
/// `a/foo.um` puts its modules in `a/`; `a/foo/mod.um` is itself the module
/// `foo`, so its children live in `a/foo/`.
fn module_dir(file: &Path) -> PathBuf {
    file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
}

/// The file a `mod name;` refers to, in the order the language looks.
pub fn find_module_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let flat = dir.join(format!("{name}.um"));
    if flat.is_file() {
        return Some(canonical(&flat));
    }
    let nested = dir.join(name).join("mod.um");
    if nested.is_file() {
        return Some(canonical(&nested));
    }
    None
}

/// The canonical form of a path, so `a/../a/x.um` and `a/x.um` are recognised as
/// the same module however the include was written.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A span that points at nothing, for a diagnostic that has no better place.
pub fn no_span() -> Span {
    Span::new(crate::source::FileId::DUMMY, 0, 0)
}

/// Every module file `program` would load, with its text.
///
/// The build cache has to be keyed on the files that actually went into the
/// assembly, not just the entry file. Without this, editing a module left the
/// previous assembly in the cache and the next build reused it -- the program
/// silently kept its old behaviour, which is the worst kind of stale.
///
/// The traversal mirrors [`load_modules`] deliberately: it walks the parsed
/// items, not the loaded program, so it needs no `SourceMap` and cannot fail.
pub fn module_sources(program: &Program, base: &Path) -> Vec<(String, String)> {
    module_sources_with_deps(program, base, &[])
}

/// Like [`module_sources`], but also collects files a `mod` resolves to through
/// a declared dependency.
///
/// Without this a dependency's edit would not change the cache key, and the build
/// would happily reuse assembly compiled against the old one.
pub fn module_sources_with_deps(
    program: &Program,
    base: &Path,
    dependencies: &[DependencyPath],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    collect_sources(&program.items, base, dependencies, &mut seen, &mut out);
    out
}

fn collect_sources(
    items: &[Item],
    dir: &Path,
    dependencies: &[DependencyPath],
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<(String, String)>,
) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        if let Some(inline) = &module.items {
            collect_sources(inline, dir, dependencies, seen, out);
            continue;
        }
        let found = find_module_file(dir, &module.name).or_else(|| {
            let dependency = dependencies.iter().find(|d| d.name == module.name)?;
            find_module_file(&dependency.source_dir, &module.name)
        });
        let Some(path) = found else { continue };
        if !seen.insert(path.clone()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // A nested module is resolved against its own file's directory, the same
        // way the loader does it, so the key covers the whole tree.
        let mut map = SourceMap::new();
        let id = map.add(path.to_string_lossy().into_owned(), text.clone());
        let parsed = parse_file(&map, id);
        out.push((path.to_string_lossy().into_owned(), text));
        collect_sources(&parsed.program.items, &module_dir(&path), dependencies, seen, out);
    }
}
