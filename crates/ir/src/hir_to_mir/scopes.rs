//! Name resolution facts the conversion needs, computed on the HIR before
//! it changes: once an op is converted, the regions that scoped its names
//! are gone.

use std::collections::{HashMap, HashSet};

use pliron::context::{Context, Ptr};
use pliron::linked_list::ContainsLinkedList;
use pliron::operation::Operation;
use pliron::value::DefiningEntity;

use super::region_block;
use crate::hir;

#[derive(Default)]
pub(super) struct Scopes {
    /// The `nix_hir.var` ops that refer to each `let_in` / `rec_attr_set`.
    pub vars: HashMap<Ptr<Operation>, Vec<Ptr<Operation>>>,
    /// The `nix_hir.with` ops enclosing each `nix_hir.lookup`, innermost
    /// first.
    pub withs: HashMap<Ptr<Operation>, Vec<Ptr<Operation>>>,
    /// The binding each lambda is the value of (`f = x: ...;`), which Nix
    /// names it after in error messages.
    pub lambda_names: HashMap<Ptr<Operation>, String>,
}

impl Scopes {
    pub fn compute(ctx: &Context, root: Ptr<Operation>) -> Scopes {
        let mut walker = Walker {
            ctx,
            binders: Vec::new(),
            withs: Vec::new(),
            scopes: Scopes::default(),
        };
        walker.op(root);
        walker.scopes
    }
}

/// The names a `let_in` / `rec_attr_set` binds, if `op` is one.
pub(super) fn bound_names(ctx: &Context, op: Ptr<Operation>) -> Option<HashSet<String>> {
    let is_binder = Operation::get_op::<hir::LetInOp>(op, ctx).is_some()
        || Operation::get_op::<hir::RecAttrSetOp>(op, ctx).is_some();
    is_binder.then(|| {
        let block = region_block(ctx, op, 0);
        block
            .deref(ctx)
            .iter(ctx)
            .filter_map(|op| Operation::get_op::<hir::BindOp>(op, ctx))
            .map(|bind| bind_name(ctx, bind))
            .collect()
    })
}

pub(super) fn bind_name(ctx: &Context, bind: hir::BindOp) -> String {
    bind.get_attr_bind_name(ctx)
        .expect("bindings are named")
        .as_str()
        .to_string()
}

struct Walker<'a> {
    ctx: &'a Context,
    /// The enclosing `let_in` / `rec_attr_set` ops and the names they bind,
    /// innermost last.
    binders: Vec<(Ptr<Operation>, HashSet<String>)>,
    /// The enclosing `nix_hir.with` ops, innermost last.
    withs: Vec<Ptr<Operation>>,
    scopes: Scopes,
}

impl Walker<'_> {
    fn op(&mut self, op: Ptr<Operation>) {
        crate::stack::grow(|| self.op_here(op))
    }

    fn op_here(&mut self, op: Ptr<Operation>) {
        let ctx = self.ctx;
        if let Some(var) = Operation::get_op::<hir::VarOp>(op, ctx) {
            let name = var.get_attr_var_name(ctx).expect("vars are named");
            let (binder, _) = self
                .binders
                .iter()
                .rev()
                .find(|(_, names)| names.contains(name.as_str()))
                .expect("nix_hir.var refers to an enclosing binding");
            self.scopes.vars.entry(*binder).or_default().push(op);
        } else if Operation::get_op::<hir::LookupOp>(op, ctx).is_some() {
            let withs = self.withs.iter().rev().copied().collect();
            self.scopes.withs.insert(op, withs);
        } else if let Some(bind) = Operation::get_op::<hir::BindOp>(op, ctx)
            && let DefiningEntity::Op(def) = op.deref(ctx).get_operand(0).defining_entity()
            && Operation::get_op::<hir::LambdaOp>(def, ctx).is_some()
        {
            self.scopes.lambda_names.insert(def, bind_name(ctx, bind));
        }

        let is_binder = bound_names(ctx, op)
            .map(|names| self.binders.push((op, names)))
            .is_some();
        let is_with = Operation::get_op::<hir::WithOp>(op, ctx).is_some();
        if is_with {
            self.withs.push(op);
        }
        for region in op.deref(ctx).regions() {
            for block in region.deref(ctx).iter(ctx) {
                for nested in block.deref(ctx).iter(ctx) {
                    self.op(nested);
                }
            }
        }
        if is_with {
            self.withs.pop();
        }
        if is_binder {
            self.binders.pop();
        }
    }
}
