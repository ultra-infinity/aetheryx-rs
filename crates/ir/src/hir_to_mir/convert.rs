//! The `hir-to-mir` pass: a pliron dialect conversion from `nix_hir` to
//! `nix_mir`, in place.
//!
//! The conversion itself is generic, like pliron-llvm's `builtin_to_llvm`:
//! it converts every op implementing [`ToMIRDialect`] and every type
//! implementing [`ToMIRType`]. The implementations (in [`super::ops`] and
//! [`super::bindings`]) follow one scheme, so that every HIR value becomes a
//! `nix_mir.thunk`:
//! - *Trivial* ops cannot fail (`const`, `nix_builtins.ref`, `lambda`,
//!   attribute sets without dynamic keys): they are computed where they are
//!   defined and wrapped in `nix_mir.evaluated`, so they are never suspended
//!   (like Nix's `maybeThunk`).
//! - `formals` is computed where it is defined too, because Nix checks the
//!   pattern when the function is called.
//! - Every other op becomes a `nix_mir.thunk` that forces its operands and
//!   computes the op.
//! - `yield` forces its operand.
//!
//! Computing values where Nix does is left to [`super::elide`], and regions
//! use values defined outside them until [`super::closures`] captures them.
//!
//! MIR ops built here may take HIR values that are not converted yet as
//! operands. Those values become thunks when their definitions are converted,
//! so MIR ops treat every HIR operand as a thunk.

use std::collections::HashMap;

use pliron::basic_block::BasicBlock;
use pliron::builtin::type_interfaces::FunctionTypeInterface;
use pliron::builtin::types::FunctionType;
use pliron::context::{Context, Ptr};
use pliron::derive::type_interface_impl;
use pliron::input_err;
use pliron::irbuild::dialect_conversion::{
    DialectConversion, DialectConversionRewriter, OperandsInfo, apply_dialect_conversion,
};
use pliron::irbuild::inserter::{Inserter, OpInsertionPoint};
use pliron::irbuild::rewriter::Rewriter;
use pliron::linked_list::ContainsLinkedList;
use pliron::location::{Located, Location};
use pliron::op::{Op, op_cast, op_impls};
use pliron::operation::Operation;
use pliron::pass::{AnalysisManager, Pass, PassResult};
use pliron::result::Result;
use pliron::r#type::{TypeHandle, type_cast, type_impls};
use pliron::value::Value;

use super::scopes::Scopes;
use super::{ToMIRDialect, ToMIRType, is_hir_type, region_block};
use crate::hir;
use crate::mir::{self, Ty};

/// Converts every op implementing `ToMIRDialect` (all of `nix_hir`, and
/// `@main`'s signature) to `nix_mir`.
///
/// This is a [`Pass`] of its own rather than pliron's `PassWrapper`, because
/// the `ConversionState` is computed from the module being converted.
pub struct HirToMir;

impl Pass for HirToMir {
    fn name(&self) -> &str {
        "hir-to-mir"
    }

    fn run(
        &mut self,
        op: Ptr<Operation>,
        ctx: &mut Context,
        _analyses: &mut AnalysisManager,
    ) -> Result<PassResult> {
        let mut conversion = HirToMirConversion {
            state: ConversionState {
                scopes: Scopes::compute(ctx, op),
                namespaces: HashMap::new(),
            },
        };
        let mut result = PassResult::default();
        result.ir_changed = apply_dialect_conversion(ctx, &mut conversion, op)?;
        Ok(result)
    }
}

/// What converting an op needs to know beyond the op itself: how the names
/// of the module are scoped, which the conversion of the ops around it
/// removes.
pub(crate) struct ConversionState {
    /// Computed on the HIR before it changes.
    pub(super) scopes: Scopes,
    /// The namespace of each converted `nix_hir.with`.
    pub(super) namespaces: HashMap<Ptr<Operation>, Value>,
}

/// The dialect conversion: dispatches to [`ToMIRDialect`] and [`ToMIRType`].
struct HirToMirConversion {
    state: ConversionState,
}

impl DialectConversion for HirToMirConversion {
    fn can_convert_op(&self, ctx: &Context, op: Ptr<Operation>) -> bool {
        let op_dyn = Operation::get_op_dyn(op, ctx);
        op_impls::<dyn ToMIRDialect>(op_dyn.op_ref())
    }

    fn can_convert_type(&self, ctx: &Context, ty: TypeHandle) -> bool {
        type_impls::<dyn ToMIRType>(&*ty.deref(ctx))
    }

    fn convert_type(&mut self, ctx: &mut Context, ty: TypeHandle) -> Result<TypeHandle> {
        type_cast::<dyn ToMIRType>(&*ty.deref(ctx))
            .expect("checked by can_convert_type")
            .convert(ctx)
    }

    fn rewrite(
        &mut self,
        ctx: &mut Context,
        rewriter: &mut DialectConversionRewriter,
        op: Ptr<Operation>,
        _operands_info: &OperandsInfo,
    ) -> Result<()> {
        let op_dyn = Operation::get_op_dyn(op, ctx);
        let to_mir =
            op_cast::<dyn ToMIRDialect>(op_dyn.op_ref()).expect("checked by can_convert_op");
        to_mir.rewrite(&mut Builder::new(ctx, rewriter, op), &mut self.state)
    }
}

/// A HIR value is a thunk in the MIR.
#[type_interface_impl]
impl ToMIRType for hir::ValueType {
    fn convert(&self, ctx: &Context) -> Result<TypeHandle> {
        Ok(Ty::Thunk.handle(ctx))
    }
}

/// A function takes its arguments lazily and returns its results evaluated.
#[type_interface_impl]
impl ToMIRType for FunctionType {
    fn convert(&self, ctx: &Context) -> Result<TypeHandle> {
        let args = self
            .arg_types()
            .into_iter()
            .map(|ty| match type_cast::<dyn ToMIRType>(&*ty.deref(ctx)) {
                Some(to_mir) => to_mir.convert(ctx),
                None => Ok(ty),
            })
            .collect::<Result<_>>()?;
        let results = self
            .res_types()
            .into_iter()
            .map(|ty| {
                if is_hir_type(ctx, ty) {
                    Ty::Value.handle(ctx)
                } else {
                    ty
                }
            })
            .collect();
        Ok(FunctionType::get(ctx, args, results).into())
    }
}

/// Builds MIR for the HIR op being converted, at the rewriter's insertion
/// point, located at the HIR op.
pub(crate) struct Builder<'a> {
    pub ctx: &'a mut Context,
    pub rewriter: &'a mut DialectConversionRewriter,
    /// The HIR op being converted.
    op: Ptr<Operation>,
    loc: Location,
}

impl<'a> Builder<'a> {
    fn new(
        ctx: &'a mut Context,
        rewriter: &'a mut DialectConversionRewriter,
        op: Ptr<Operation>,
    ) -> Self {
        let loc = op.deref(ctx).loc();
        Builder {
            ctx,
            rewriter,
            op,
            loc,
        }
    }

    /// Operand `i` of the HIR op.
    pub fn operand(&self, i: usize) -> Value {
        self.op.deref(self.ctx).get_operand(i)
    }

    /// The operands of the HIR op.
    pub fn operands(&self) -> Vec<Value> {
        self.op.deref(self.ctx).operands().collect()
    }

    /// The single block of region `i` of the HIR op.
    pub fn region(&self, i: usize) -> Ptr<BasicBlock> {
        region_block(self.ctx, self.op, i)
    }

    /// The error for an op that the op containing it converts (a binding, or
    /// a name), reached on its own.
    pub fn converted_with_parent(&self) -> Result<()> {
        input_err!(
            self.loc.clone(),
            "{} is converted with the op that contains it",
            Operation::get_opid(self.op, self.ctx)
        )
    }

    /// Replaces the HIR op's result with `thunk`, and erases the op.
    pub fn replace(&mut self, thunk: Value) {
        self.rewriter
            .replace_operation_with_values(self.ctx, self.op, vec![thunk]);
    }

    /// Erases the HIR op, which has no result.
    pub fn erase(&mut self) {
        self.rewriter.erase_operation(self.ctx, self.op);
    }

    /// Inserts `op` at the insertion point; the insertion point stays after it.
    pub fn insert(&mut self, op: &impl Op) {
        let op = op.get_operation();
        op.deref_mut(self.ctx).set_loc(self.loc.clone());
        self.rewriter.insert_operation(self.ctx, op);
    }

    /// Inserts a single-result op and returns its result.
    pub fn value(&mut self, op: impl Op) -> Value {
        self.insert(&op);
        mir::result_of(self.ctx, &op)
    }

    pub fn force(&mut self, thunk: Value) -> Value {
        let force = mir::ForceOp::new(self.ctx, thunk);
        self.value(force)
    }

    pub fn evaluated(&mut self, value: Value) -> Value {
        let evaluated = mir::EvaluatedOp::new(self.ctx, value);
        self.value(evaluated)
    }

    pub fn r#yield(&mut self, value: Value) {
        let ret = mir::YieldOp::new(self.ctx, value);
        self.insert(&ret);
    }

    /// Runs `f` with the insertion point at the end of `block`.
    pub fn at_end_of<T>(&mut self, block: Ptr<BasicBlock>, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.rewriter.get_insertion_point();
        self.rewriter
            .set_insertion_point(OpInsertionPoint::AtBlockEnd(block));
        let result = f(self);
        self.rewriter.set_insertion_point(saved);
        result
    }

    /// A thunk whose body `f` fills, terminator included.
    pub fn thunk(&mut self, f: impl FnOnce(&mut Self)) -> Value {
        let thunk = mir::ThunkOp::new(self.ctx);
        self.insert(&thunk);
        let body = thunk.body(self.ctx);
        self.at_end_of(body, f);
        mir::result_of(self.ctx, &thunk)
    }

    /// A thunk computing the value `f` returns.
    pub fn thunk_of(&mut self, f: impl FnOnce(&mut Self) -> Value) -> Value {
        self.thunk(|b| {
            let value = f(b);
            b.r#yield(value);
        })
    }

    /// Moves the ops of `block` to the insertion point, in order.
    pub fn move_ops(&mut self, block: Ptr<BasicBlock>) {
        let ops: Vec<_> = block.deref(self.ctx).iter(self.ctx).collect();
        for op in ops {
            self.move_op(op);
        }
    }

    /// Moves `op` to the insertion point.
    pub fn move_op(&mut self, op: Ptr<Operation>) {
        let point = self.rewriter.get_insertion_point();
        self.rewriter.move_operation(self.ctx, op, point);
    }
}
