//! Lowering from `nix_hir` to `nix_mir`, in place, as a pipeline of pliron
//! passes ([`pipeline`]):
//!
//! 1. [`HirToMir`] (`hir-to-mir`): a pliron dialect conversion. Each op
//!    converts itself through its `ToMIRDialect` implementation, and the
//!    `nix_hir.value` type through `ToMIRType`, the way pliron-llvm's
//!    `ToLLVMDialect` / `ToLLVMType` work. Every HIR value becomes a
//!    `nix_mir.thunk`: ops that cannot fail are computed where they are
//!    defined, and everything else is suspended there. Names are resolved:
//!    `nix_hir.var` becomes a member of the `nix_mir.letrec` of its `let` /
//!    `rec { }`, and `nix_hir.lookup` a `nix_mir.with_lookup` of the
//!    enclosing `with` namespaces.
//! 2. [`ElideThunks`] (`elide-thunks`): computes values where Nix does, at
//!    their only use when they are not in a lazy position.
//! 3. [`ConvertClosures`] (`convert-closures`): makes the regions of `thunk`,
//!    `lambda` and `letrec` isolated, with explicit captures, so that they can
//!    be outlined to LLVM functions.
//! 4. [`CheckMir`] (`check-mir`): checks that nothing is left of the HIR and
//!    verifies the module.
//!
//! The IR between the passes mixes both dialects or has implicit captures,
//! so it is not valid MIR until the pipeline has run.

mod bindings;
mod check;
mod closures;
mod convert;
mod elide;
mod ops;
mod scopes;
#[cfg(test)]
mod tests;

pub use check::CheckMir;
pub use closures::ConvertClosures;
pub use convert::HirToMir;
pub use elide::ElideThunks;

use pliron::basic_block::BasicBlock;
use pliron::builtin::ops::{FuncOp, ModuleOp};
use pliron::builtin::type_interfaces::FunctionTypeInterface;
use pliron::builtin::types::FunctionType;
use pliron::context::{Context, Ptr};
use pliron::derive::{op_interface, type_interface};
use pliron::linked_list::ContainsLinkedList;
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::pass::{AnalysisManager, Pass, Passes};
use pliron::result::Result;
use pliron::r#type::{Type, TypeHandle};

use crate::hir;
use convert::{Builder, ConversionState};

/// Interface for converting an op to `nix_mir`, used by [`HirToMir`] the way
/// pliron-llvm's `builtin_to_llvm` uses `ToLLVMDialect`. Every `nix_hir` op
/// implements it, and so do `nix_builtins.ref` (shared by both IRs) and
/// `builtin.func` (whose signature changes).
///
/// Like `ToLLVMDialect::rewrite`, an implementation rewrites its op
/// completely: it builds the MIR, then replaces the op ([`Builder::replace`]),
/// erases it ([`Builder::erase`]) or changes it in place. An op that is
/// neither replaced nor erased stays in the IR, and if it is still HIR,
/// [`CheckMir`] reports it as left over. Unlike `ToLLVMDialect::rewrite`, it
/// also gets the [`ConversionState`]: converting a name needs the scope it
/// is resolved in, which an op cannot see once the ops around it are
/// converted.
#[op_interface]
pub(crate) trait ToMIRDialect {
    /// Rewrites `self` to `nix_mir` with `b`, whose insertion point is before
    /// `self`. The definitions of `self`'s operands are already converted.
    fn rewrite(&self, b: &mut Builder, state: &mut ConversionState) -> Result<()>;

    fn verify(_op: &dyn Op, _ctx: &Context) -> Result<()>
    where
        Self: Sized,
    {
        Ok(())
    }
}

/// Interface for converting a type to `nix_mir`, used by [`HirToMir`].
#[type_interface]
pub(crate) trait ToMIRType {
    /// Converts `self` to a `nix_mir` type.
    fn convert(&self, ctx: &Context) -> Result<TypeHandle>;

    fn verify(_ty: &dyn Type, _ctx: &Context) -> Result<()>
    where
        Self: Sized,
    {
        Ok(())
    }
}

/// The passes lowering a module produced by [`crate::ast_to_hir`] to a
/// verified `nix_mir` module, in order.
pub fn pipeline() -> Passes {
    let mut passes = Passes::default();
    passes.add_pass(HirToMir);
    passes.add_pass(ElideThunks);
    passes.add_pass(ConvertClosures);
    passes.add_pass(CheckMir);
    passes
}

/// Runs the [`pipeline`] on `module`: afterwards it is a verified `nix_mir`
/// module whose `@main` has type `() -> (nix_mir.value)`.
pub fn lower_module(ctx: &mut Context, module: ModuleOp) -> Result<()> {
    let mut analyses = AnalysisManager::default();
    pipeline().run(module.get_operation(), ctx, &mut analyses)?;
    Ok(())
}

/// The single block of region `idx` of `op`.
fn region_block(ctx: &Context, op: Ptr<Operation>, idx: usize) -> Ptr<BasicBlock> {
    op.deref(ctx)
        .get_region(idx)
        .deref(ctx)
        .get_head()
        .expect("regions have one block")
}

/// `root` and the ops nested in it, in pre-order.
fn nested_ops(ctx: &Context, root: Ptr<Operation>) -> Vec<Ptr<Operation>> {
    let mut ops = Vec::new();
    let mut stack = vec![root];
    while let Some(op) = stack.pop() {
        ops.push(op);
        let opr = op.deref(ctx);
        let nested: Vec<_> = opr
            .regions()
            .flat_map(|region| region.deref(ctx).iter(ctx).collect::<Vec<_>>())
            .flat_map(|block| block.deref(ctx).iter(ctx).collect::<Vec<_>>())
            .collect();
        stack.extend(nested.into_iter().rev());
    }
    ops
}

fn is_hir_type(ctx: &Context, ty: TypeHandle) -> bool {
    ty == hir::ValueType::get(ctx).into()
}

/// The argument and result types of `func`.
fn signature(ctx: &Context, func: &FuncOp) -> (Vec<TypeHandle>, Vec<TypeHandle>) {
    let ty = func.get_type(ctx);
    let ty = ty.deref(ctx);
    let ty = ty
        .downcast_ref::<FunctionType>()
        .expect("functions have a function type");
    (ty.arg_types(), ty.res_types())
}
