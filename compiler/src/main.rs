//! `umay` CLI entry point.
//!
//! Implemented commands: `check`, `explain`, `ast`, `expand`, `doctor`, `new`,
//! `clean`, `version`, and `bindgen` (the narrow C-header slice from Phase 3).
//!
//! Commands belonging to later phases (`build`, `run`, `fmt`, `lint`, `lsp`)
//! are recognised and exit with a clear "not implemented in this phase" message
//! plus the phase in which they will land. They never pretend to succeed.

use std::process::ExitCode;

mod bindgen;
mod cli;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    cli::run(args)
}
