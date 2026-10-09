//! The `elide-thunks` pass: computes values where Nix does.
//!
//! After [`super::convert`], every value is suspended where it is defined.
//! This pass removes the suspensions that are not needed:
//! - A thunk forced at its only use is computed there instead: its body
//!   moves to the force. Creating a thunk evaluates nothing, so evaluating its
//!   body at the one point that forces it is exactly lazy evaluation; the
//!   values the body uses dominate the thunk, which dominates the force.
//! - `force (evaluated v)` is `v`. If `v` is computed by a trivial op used
//!   only there, the op moves to the force too, so that values are defined
//!   next to their uses: pliron's dominance check walks from a definition to
//!   its use, so this keeps verification linear.
//! - Thunks and trivial values that are no longer used are erased.
//!
//! Computation never moves into a `lambda`, whose body may run many times;
//! only into `if` branches and `thunk` / `letrec` bodies, which run at most
//! once each time the code around them does.
//!
//! What stays suspended is what Nix allocates thunks for: values in lazy
//! positions (function arguments, list elements, attribute and `let` values,
//! `with` namespaces) and values forced in more than one place.

use pliron::context::{Context, Ptr};
use pliron::linked_list::ContainsLinkedList;
use pliron::operation::Operation;
use pliron::pass::{AnalysisManager, Pass, PassResult};
use pliron::result::Result;
use pliron::value::{DefiningEntity, Value};

use super::{nested_ops, region_block};
use crate::builtins_dialect::RefOp;
use crate::mir;

pub struct ElideThunks;

impl Pass for ElideThunks {
    fn name(&self) -> &str {
        "elide-thunks"
    }

    fn run(
        &mut self,
        op: Ptr<Operation>,
        ctx: &mut Context,
        _analyses: &mut AnalysisManager,
    ) -> Result<PassResult> {
        let ops = nested_ops(ctx, op);
        let forces: Vec<_> = ops
            .into_iter()
            .filter(|&op| Operation::get_op::<mir::ForceOp>(op, ctx).is_some())
            .collect();
        let mut changed = false;
        // Forces of enclosing thunks come later in pre-order than the forces
        // in their bodies, so visiting them in reverse moves each body once,
        // to its final place.
        for force in forces.into_iter().rev() {
            changed |= elide_force(ctx, force);
        }
        changed |= erase_unused(ctx, op);
        let mut result = PassResult::default();
        result.ir_changed = changed.into();
        Ok(result)
    }
}

/// Removes the `nix_mir.force` `force` if it is not needed; returns whether
/// it did.
fn elide_force(ctx: &mut Context, force: Ptr<Operation>) -> bool {
    let thunk = force.deref(ctx).get_operand(0);
    let DefiningEntity::Op(def) = thunk.defining_entity() else {
        return false;
    };
    let single_use = thunk.num_uses(ctx) == 1;
    if Operation::get_op::<mir::ThunkOp>(def, ctx).is_some() {
        if !(single_use && can_move(ctx, def, force)) {
            return false;
        }
        let body = region_block(ctx, def, 0);
        let ret = body
            .deref(ctx)
            .get_terminator(ctx)
            .expect("thunk bodies end in nix_mir.yield");
        let value = ret.deref(ctx).get_operand(0);
        let ops: Vec<_> = body.deref(ctx).iter(ctx).filter(|&op| op != ret).collect();
        for op in ops {
            move_before(ctx, op, force);
        }
        replace(ctx, force, value);
        Operation::erase(def, ctx);
        return true;
    }
    if Operation::get_op::<mir::EvaluatedOp>(def, ctx).is_some() {
        let value = def.deref(ctx).get_operand(0);
        if let DefiningEntity::Op(computed) = value.defining_entity()
            && single_use
            && value.num_uses(ctx) == 1
            && is_trivial(ctx, computed)
            && can_move(ctx, computed, force)
        {
            move_before(ctx, computed, force);
        }
        replace(ctx, force, value);
        if single_use {
            Operation::erase(def, ctx);
        }
        return true;
    }
    false
}

/// Replaces the result of `op` with `value` and erases `op`.
fn replace(ctx: &mut Context, op: Ptr<Operation>, value: Value) {
    let result = op.deref(ctx).get_result(0);
    result.replace_all_uses_with(ctx, &value);
    Operation::erase(op, ctx);
}

fn move_before(ctx: &Context, op: Ptr<Operation>, before: Ptr<Operation>) {
    op.unlink(ctx);
    op.insert_before(ctx, before);
}

/// Whether computation can move from `def` to `to`, which `def` dominates:
/// `to` is in the same block, or nested in it through `if`, `thunk` and
/// `letrec` regions but no `lambda`.
fn can_move(ctx: &Context, def: Ptr<Operation>, to: Ptr<Operation>) -> bool {
    let target = def.deref(ctx).get_parent_block();
    let mut block = to.deref(ctx).get_parent_block();
    while let Some(current) = block {
        if Some(current) == target {
            return true;
        }
        let Some(parent) = current.deref(ctx).get_parent_op(ctx) else {
            return false;
        };
        if Operation::get_op::<mir::LambdaOp>(parent, ctx).is_some() {
            return false;
        }
        block = parent.deref(ctx).get_parent_block();
    }
    false
}

/// Ops that cannot fail and have no effect, so they can be computed anywhere
/// they are dominated by their operands, or not at all.
fn is_trivial(ctx: &Context, op: Ptr<Operation>) -> bool {
    Operation::get_op::<mir::ConstOp>(op, ctx).is_some()
        || Operation::get_op::<RefOp>(op, ctx).is_some()
        || Operation::get_op::<mir::LambdaOp>(op, ctx).is_some()
        || Operation::get_op::<mir::AttrsOp>(op, ctx).is_some()
}

/// Erases the thunks and trivial values nested in `root` that nothing uses.
/// Uses come after their definitions in pre-order, so visiting the ops in
/// reverse erases chains of unused values in one sweep; and the ops nested in
/// an op are visited before it, so none is visited after being erased.
fn erase_unused(ctx: &mut Context, root: Ptr<Operation>) -> bool {
    let mut changed = false;
    for op in nested_ops(ctx, root).into_iter().rev() {
        let removable = Operation::get_op::<mir::ThunkOp>(op, ctx).is_some()
            || Operation::get_op::<mir::EvaluatedOp>(op, ctx).is_some()
            || is_trivial(ctx, op);
        if removable && !op.deref(ctx).get_result(0).is_used(ctx) {
            Operation::erase(op, ctx);
            changed = true;
        }
    }
    changed
}
