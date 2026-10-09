//! The `nix_builtins` dialect: references to Nix builtins.
//!
//! Builtins are first-class values in Nix (`map` can be passed around and
//! partially applied, and `builtins` is an attribute set), so they are
//! represented as values: `nix_builtins.ref "map"` produces the builtin
//! `builtins.map`. The op is shared by both IRs: its result is a
//! `nix_hir.value` in the HIR and a `nix_mir.value` in the MIR. Which builtins
//! exist comes from the [`builtins`] registry.
//!
//! The dialect is named `nix_builtins` rather than `builtins` to avoid
//! confusion with pliron's own `builtin` dialect (`builtin.module`, ...).

use pliron::builtin::attributes::StringAttr;
use pliron::builtin::op_interfaces::{NOpdsInterface, NResultsInterface};
use pliron::common_traits::Verify;
use pliron::context::Context;
use pliron::derive::pliron_op;
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::result::Result;
use pliron::r#type::{TypeHandle, Typed};
use pliron::verify_err;

use crate::{hir, mir};

#[pliron_op(
    name = "nix_builtins.ref",
    // The builtin `builtins.<name>`, as a value.
    format = "attr($builtin_name, $StringAttr) ` : ` type($0)",
    interfaces = [
        NOpdsInterface<0>,
        NResultsInterface<1>,
    ],
    attributes = (builtin_name: StringAttr)
)]
pub struct RefOp;

impl RefOp {
    /// A reference to `builtins.<name>` of type `ty` (`nix_hir.value` or
    /// `nix_mir.value`); `name` is the registry name, e.g. `add` for the
    /// global `__add`.
    pub fn new(ctx: &mut Context, name: &str, ty: TypeHandle) -> Self {
        let op = RefOp {
            op: Operation::new(
                ctx,
                Self::get_concrete_op_info(),
                vec![ty],
                vec![],
                vec![],
                0,
            ),
        };
        op.set_attr_builtin_name(ctx, StringAttr::new(name.to_string()));
        op
    }

    /// The registry entry this op refers to, if the name is valid.
    pub fn builtin(&self, ctx: &Context) -> Option<&'static builtins::Builtin> {
        let name = self.get_attr_builtin_name(ctx)?;
        builtins::get(name.as_str())
    }
}

impl Verify for RefOp {
    fn verify(&self, ctx: &Context) -> Result<()> {
        let Some(name) = self
            .get_attr_builtin_name(ctx)
            .map(|n| n.as_str().to_string())
        else {
            return verify_err!(self.loc(ctx), "nix_builtins.ref has no builtin_name");
        };
        if builtins::get(&name).is_none() {
            return verify_err!(self.loc(ctx), "'{}' is not a Nix builtin", name);
        }
        let ty = self.op.deref(ctx).get_result(0).get_type(ctx);
        let hir_value: TypeHandle = hir::ValueType::get(ctx).into();
        let mir_value: TypeHandle = mir::ValueType::get(ctx).into();
        if ty != hir_value && ty != mir_value {
            return verify_err!(
                self.loc(ctx),
                "nix_builtins.ref must produce a nix_hir.value or a nix_mir.value"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pliron::op::verify_op;

    use super::*;

    #[test]
    fn verifies_known_builtins_only() {
        let ctx = &mut Context::new();
        let hir_value = hir::ValueType::get(ctx).into();
        let known = RefOp::new(ctx, "map", hir_value);
        assert!(verify_op(&known, ctx).is_ok());
        assert_eq!(known.builtin(ctx).map(|b| b.name), Some("map"));
        let unknown = RefOp::new(ctx, "notABuiltin", hir_value);
        assert!(verify_op(&unknown, ctx).is_err());
    }

    #[test]
    fn result_must_be_a_value_type() {
        let ctx = &mut Context::new();
        let mir_value = mir::ValueType::get(ctx).into();
        assert!(verify_op(&RefOp::new(ctx, "map", mir_value), ctx).is_ok());
        let thunk = mir::ThunkType::get(ctx).into();
        assert!(verify_op(&RefOp::new(ctx, "map", thunk), ctx).is_err());
    }
}
