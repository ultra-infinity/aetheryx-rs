//! The intermediate representations of the Nix compiler, built on pliron.
//!
//! The pipeline is
//!
//! ```text
//! rnix AST ──ast_to_hir──▶ nix_hir ──(Phase 2)──▶ nix_mir ──▶ LLVM
//! ```
//!
//! - [`hir`]: `nix_hir`, a desugared, name-resolved form of the source.
//! - [`mir`]: `nix_mir`, the codegen-oriented IR (being designed).
//! - [`builtins_dialect`]: `nix_builtins`, references to Nix builtins.
//! - [`ast_to_hir`]: lowering from the rnix AST to `nix_hir`.
//! - [`diagnostics`]: source files, IR source locations and error reporting.
//! - [`paths`]: Nix's parse-time path resolution.

pub mod ast_to_hir;
pub mod builtins_dialect;
pub mod diagnostics;
pub mod hir;
pub mod mir;
pub mod paths;
