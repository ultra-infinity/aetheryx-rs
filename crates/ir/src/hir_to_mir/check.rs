//! The `check-mir` pass: checks that the pipeline produced valid MIR, with
//! nothing left of the HIR.

use pliron::builtin::ops::FuncOp;
use pliron::context::{Context, Ptr};
use pliron::dialect::DialectName;
use pliron::linked_list::ContainsLinkedList;
use pliron::location::Located;
use pliron::operation::{Operation, verify_operation};
use pliron::pass::{AnalysisManager, Pass, PassResult};
use pliron::result::Result;
use pliron::r#type::Typed;
use pliron::verify_err;

use super::{is_hir_type, nested_ops, signature};

pub struct CheckMir;

impl Pass for CheckMir {
    fn name(&self) -> &str {
        "check-mir"
    }

    fn run(
        &mut self,
        op: Ptr<Operation>,
        ctx: &mut Context,
        _analyses: &mut AnalysisManager,
    ) -> Result<PassResult> {
        let hir = DialectName::try_new("nix_hir").expect("a valid dialect name");
        for nested in nested_ops(ctx, op) {
            if Operation::get_opid(nested, ctx).dialect == hir || has_hir_types(ctx, nested) {
                return verify_err!(
                    nested.deref(ctx).loc(),
                    "{} is left over from nix_hir",
                    Operation::get_opid(nested, ctx)
                );
            }
        }
        verify_operation(op, ctx)?;
        Ok(PassResult::default())
    }
}

/// Whether a result, a block argument or the signature of `op` has the
/// `nix_hir.value` type.
fn has_hir_types(ctx: &Context, op: Ptr<Operation>) -> bool {
    if let Some(func) = Operation::get_op::<FuncOp>(op, ctx) {
        let (args, results) = signature(ctx, &func);
        if args.iter().chain(&results).any(|&ty| is_hir_type(ctx, ty)) {
            return true;
        }
    }
    let opr = op.deref(ctx);
    let mut args = opr
        .regions()
        .flat_map(|region| region.deref(ctx).iter(ctx).collect::<Vec<_>>())
        .flat_map(|block| block.deref(ctx).arguments().collect::<Vec<_>>());
    opr.results()
        .chain(&mut args)
        .any(|value| is_hir_type(ctx, value.get_type(ctx)))
}
