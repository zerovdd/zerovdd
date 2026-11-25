//! Shared repository inspection helpers and Rust source lexer used by compiled static lints and script tests.

/// Rust source lexer for comments and `unsafe` keyword tracking.
pub mod lexer;
/// Repository traversal, manifest discovery, and portable shell lookup helpers.
pub mod repo;

pub use lexer::{lex_rust_source, CommentToken, RustLexResult, UnsafeOccurrence};
pub use repo::{
    collect_crate_manifests, collect_files, collect_rust_files, find_bash_executable,
    relative_display, repo_root,
};
