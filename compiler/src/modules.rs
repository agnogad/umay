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
    let mut diagnostics = Diagnostics::new();
    let mut loading = BTreeSet::new();
    let mut loaded = BTreeSet::new();
    let items = expand(program.items, base, map, &mut diagnostics, &mut loading, &mut loaded);
    (Program { items, span: program.span }, diagnostics)
}

/// Loads modules for a program whose entry point is the file at `entry`.
pub fn load_from_entry(program: Program, entry: &Path, map: &mut SourceMap) -> (Program, Diagnostics) {
    let base = entry.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    load_modules(program, &base, map)
}

fn expand(
    items: Vec<Item>,
    dir: &Path,
    map: &mut SourceMap,
    diagnostics: &mut Diagnostics,
    loading: &mut BTreeSet<PathBuf>,
    loaded: &mut BTreeSet<PathBuf>,
) -> Vec<Item> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Item::Mod(module) = &item else {
            out.push(item);
            continue;
        };

        // `mod foo { ... }` is already complete, but its contents may still name
        // files, and those resolve relative to this file's directory.
        if let Some(inline) = module.items.clone() {
            let expanded = expand(inline, dir, map, diagnostics, loading, loaded);
            out.push(Item::Mod(ModDecl { items: Some(expanded), ..module.clone() }));
            continue;
        }

        let Some(path) = find_module_file(dir, &module.name) else {
            let first = dir.join(format!("{}.um", module.name));
            diagnostics.push(
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
        if loading.contains(&path) {
            diagnostics.push(
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
        if loaded.contains(&path) {
            continue;
        }
        loaded.insert(path.clone());

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                diagnostics.push(
                    crate::diagnostics::Diagnostic::error(
                        "E0194",
                        format!("cannot read `{}`: {e}", path.display()),
                    )
                    .at(module.name_span),
                );
                continue;
            }
        };
        let id = map.add(path.to_string_lossy().into_owned(), text);
        let parsed = parse_file(map, id);
        for diagnostic in parsed.diagnostics.items() {
            diagnostics.push(diagnostic.clone());
        }

        loading.insert(path.clone());
        // Nested modules resolve against the directory the module file lives in,
        // which is either its parent or, for `a/mod.um`, `a` itself.
        let nested_dir = module_dir(&path);
        let expanded =
            expand(parsed.program.items, &nested_dir, map, diagnostics, loading, loaded);
        loading.remove(&path);

        // Keep the module's own name so `foo::bar` still resolves: the loaded
        // items belong to `mod foo`, not to the file that declared it. The span
        // stays the one in the declaring file, so a name-path error points where
        // the author wrote the name.
        out.push(Item::Mod(ModDecl { items: Some(expanded), ..module.clone() }));
    }
    out
}

/// The directory a module file's own `mod` declarations resolve against.
///
/// `a/foo.um` puts its modules in `a/`; `a/foo/mod.um` is itself the module
/// `foo`, so its children live in `a/foo/`.
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    if file.file_name().is_some_and(|n| n == "mod.um") {
        parent
    } else {
        parent
    }
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
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    collect_sources(&program.items, base, &mut seen, &mut out);
    out
}

fn collect_sources(
    items: &[Item],
    dir: &Path,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<(String, String)>,
) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        if let Some(inline) = &module.items {
            collect_sources(inline, dir, seen, out);
            continue;
        }
        let Some(path) = find_module_file(dir, &module.name) else { continue };
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
        collect_sources(&parsed.program.items, &module_dir(&path), seen, out);
    }
}
