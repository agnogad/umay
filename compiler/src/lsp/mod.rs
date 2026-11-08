//! Language Server Protocol over stdio.
//!
//! An editor speaks JSON-RPC: a `Content-Length` header, two newlines, then a
//! JSON body. There is no separate library here for the same reason the rest of
//! the compiler has no dependencies -- the protocol surface this server needs is
//! small, and writing it out keeps what it does visible.
//!
//! What the server does NOT do is hold a second compiler. Every request runs the
//! same functions the command-line compiler runs: `check_source` for diagnostics,
//! `metadata::symbols` for the names, `fmt::format` for formatting. That is the
//! point of building it last: the phases before this one exist so that an editor
//! talks to the compiler rather than to a copy of it that drifts.
//!
//! Positions are the fiddliest part. LSP counts lines from 0 and columns in UTF-16
//! code units; UMAY counts both from 0 in bytes, or from 1 in characters. Every
//! conversion lives in `position.rs` so that the off-by-one lives in one place.

mod jsonrpc;
mod position;
mod server;

pub use server::serve;
