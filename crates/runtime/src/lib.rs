//! The Nix runtime: values and the semantics of every `nix_mir` operation.
//!
//! The reference interpreter calls these functions directly. Code generated
//! from the MIR will call them through a C ABI, which is why a [`Value`] is a
//! single pointer.

pub mod error;
pub mod ops;
pub mod paths;
pub mod primops;
pub mod print;
pub mod value;

pub use error::{Error, Result};
pub use primops::Builtins;
pub use value::{Attrs, Data, Lambda, PrimOp, PrimOpApp, PrimOpFn, Value};
