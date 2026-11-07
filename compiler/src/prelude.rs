//! The prelude: functions every UMAY program can call without importing
//! anything.
//!
//! These are ordinary UMAY functions, not compiler built-ins. They are written
//! in source and parsed together with the program, so the checker,
//! borrow checker and backend see exactly what a user would have written. That
//! keeps one rule for how a call behaves and means the prelude cannot drift from
//! the language it is written in.
//!
//! The bodies delegate to the runtime (`runtime/runtime.um`), which is compiled
//! into every program. The two are separate on purpose: this file is the
//! language's surface, the runtime is its implementation, and the runtime can be
//! replaced or extended without changing what programs call.

use crate::ast;
use crate::source::{FileId, SourceMap};

/// The prelude's source text.
///
/// Kept as a string rather than generated code so that a reader can see exactly
/// what every program gets for free. `include_str!` makes it compile-time data,
/// so there is no file to find at run time.
///
/// The standard library's own files follow the prelude, because the prelude's
/// declarations are what they are written against. `concat!` keeps this a single
/// compile-time string: there is no file to find, no import to resolve, and no
/// way for the parts to be built out of step with each other.
pub const PRELUDE_SOURCE: &str = concat!(
    include_str!("../../stdlib/prelude.um"),
    "\n",
    include_str!("../../stdlib/option.um"),
    "\n",
    include_str!("../../stdlib/string.um"),
    "\n",
    include_str!("../../stdlib/vec.um"),
    "\n",
    include_str!("../../stdlib/fs.um"),
    "\n",
    include_str!("../../stdlib/process.um"),
    "\n",
    include_str!("../../stdlib/hashmap.um"),
    "\n",
    include_str!("../../stdlib/bytes.um"),
);

/// The name the prelude's file is registered under in the source map.
pub const PRELUDE_FILE: &str = "<prelude>";

/// Parses the prelude and returns its items.
///
/// A parse error here is a bug in the compiler, not in the user's program: the
/// prelude is part of the toolchain and its tests keep it parsing. The errors are
/// still returned so a caller can surface them rather than silently producing a
/// program with a missing prelude.
pub fn parse_prelude(map: &mut SourceMap) -> (Vec<ast::Item>, crate::diagnostics::Diagnostics) {
    let id = map.add(PRELUDE_FILE, PRELUDE_SOURCE);
    let out = crate::parse_file(map, id);
    (out.program.items, out.diagnostics)
}

/// Appends the prelude's items in front of `program`.
///
/// The prelude goes first so that its names are declared before any user item,
/// which is also the order they are looked up in when a user shadows something.
pub fn with_prelude(
    program: ast::Program,
    map: &mut SourceMap,
) -> (ast::Program, crate::diagnostics::Diagnostics) {
    let (items, diagnostics) = parse_prelude(map);
    let mut all = items;
    all.extend(program.items);
    (ast::Program { items: all, span: program.span }, diagnostics)
}

/// The file id the prelude occupies, for diagnostics that must not be reported
/// against user code.
pub fn prelude_file(map: &SourceMap) -> Option<FileId> {
    (0..map.len())
        .map(|i| FileId(i as u32))
        .find(|&id| map.file(id).name() == PRELUDE_FILE)
}
