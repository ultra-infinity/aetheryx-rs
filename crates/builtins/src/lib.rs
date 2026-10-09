//! The Nix builtins: which exist, how they are bound in the global scope,
//! and what kind of value each one is.
//!
//! This crate is pure data with no IR dependency, so both the compiler and
//! a future runtime can use it. The data in `generated.rs` is produced from
//! a real `nix` binary by `gen-registry.sh`.

mod generated;
pub mod registry;

pub use registry::{Builtin, Global, Kind, all, get, lookup_global};
