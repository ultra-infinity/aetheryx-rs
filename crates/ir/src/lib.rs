//! The intermediate representations of the Nix compiler, built on pliron.
//!
//! The pipeline is
//!
//! ```text
//! rnix AST ──ast_to_hir──▶ nix_hir ──hir_to_mir──▶ nix_mir ──▶ LLVM (Phase 3)
//! ```
//!
//! - [`hir`]: `nix_hir`, a desugared, name-resolved form of the source.
//! - [`mir`]: `nix_mir`, the codegen-oriented IR with explicit laziness and
//!   closures.
//! - [`hir_to_mir`]: lowering from `nix_hir` to `nix_mir`.
//! - [`builtins_dialect`]: `nix_builtins`, references to Nix builtins.
//! - [`ast_to_hir`]: lowering from the rnix AST to `nix_hir`.
//! - [`diagnostics`]: source files, IR source locations and error reporting.
//! - [`paths`]: Nix's parse-time path resolution.

pub mod ast_to_hir;
pub mod builtins_dialect;
pub mod diagnostics;
pub mod hir;
pub mod hir_to_mir;
pub mod mir;
pub mod paths;
#[cfg(test)]
mod test_util;
