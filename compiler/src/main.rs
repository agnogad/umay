//! `umay` CLI entry point.
//!
//! Implemented commands (Phase 1): `check`, `explain`, `ast`, `doctor`, `new`,
//! `clean`, `version`.
//!
//! Commands belonging to later phases (`build`, `run`, `fmt`, `lint`, `lsp`)
//! are recognised and exit with a clear "not implemented in this phase" message
//! plus the phase in which they will land. They never pretend to succeed.

use std::process::ExitCode;

mod cli;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    cli::run(args)
}
