//! `nix_mir`: the mid-level Nix IR, the input to code generation.
//!
//! **Status:** skeleton. The types below fix the core contract; the ops are
//! designed in Phase 2.
//!
//! The HIR ([`crate::hir`]) leaves everything a machine needs implicit. The
//! MIR makes it explicit, so that lowering to the LLVM dialect is a direct
//! translation:
//!
//! | `nix_hir` (implicit)                         | `nix_mir` (explicit)                     |
//! |----------------------------------------------|------------------------------------------|
//! | any value may be unevaluated                 | [`ThunkType`] vs. [`ValueType`], `force` |
//! | `var` / `lookup` resolve names at runtime    | resolved slots; a runtime `with` chain   |
//! | lambda regions capture outer values freely   | closures with explicit captures          |
//! | `let`, `rec`, default formals are recursive  | cells allocated, then filled             |
//! | attribute sets are regions of bindings       | runtime construction calls               |
//!
//! Source locations are copied from the HIR ops they are lowered from.

use pliron::derive::pliron_type;

/// A value that may still be unevaluated (a thunk), as passed around lazily.
#[pliron_type(name = "nix_mir.thunk", format, generate_get = true, verifier = "succ")]
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct ThunkType;

/// A value in weak head normal form: the result of forcing a [`ThunkType`].
#[pliron_type(name = "nix_mir.value", format, generate_get = true, verifier = "succ")]
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct ValueType;

#[cfg(test)]
mod tests {
    use pliron::context::Context;
    use pliron::printable::Printable;

    use super::*;

    #[test]
    fn types_print_with_the_dialect_prefix() {
        let ctx = &Context::new();
        // pliron prints a trailing space after a type without parameters.
        assert_eq!(
            ThunkType::get(ctx).disp(ctx).to_string().trim_end(),
            "nix_mir.thunk"
        );
        assert_eq!(
            ValueType::get(ctx).disp(ctx).to_string().trim_end(),
            "nix_mir.value"
        );
    }
}
