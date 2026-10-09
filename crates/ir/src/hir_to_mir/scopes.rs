//! Name resolution facts the conversion needs, computed on the HIR before
//! it changes: once an op is converted, the regions that scoped its names
//! are gone.

use std::collections::{HashMap, HashSet};

use pliron::basic_block::BasicBlock;
use pliron::context::{Context, Ptr};
use pliron::linked_list::ContainsLinkedList;
use pliron::operation::Operation;
use pliron::value::{DefiningEntity, Value};

use super::region_block;
use crate::hir;

#[derive(Default)]
pub(super) struct Scopes {
    /// The `nix_hir.var` ops that refer to each `let_in` / `rec_attr_set`.
    pub vars: HashMap<Ptr<Operation>, Vec<Ptr<Operation>>>,
    /// The `nix_hir.with` ops enclosing each `nix_hir.lookup`, innermost
    /// first.
    pub withs: HashMap<Ptr<Operation>, Vec<Ptr<Operation>>>,
    /// The binding each lambda is named after in Nix's error messages: the
    /// one it is the value of (`f = x: ...;`), also for the lambdas that are
    /// directly the bodies of such a lambda (`f = x: y: ...;`).
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

/// The `nix_hir.lambda` defining `value`, if any.
fn lambda_of(ctx: &Context, value: Value) -> Option<Ptr<Operation>> {
    match value.defining_entity() {
        DefiningEntity::Op(def) if Operation::get_op::<hir::LambdaOp>(def, ctx).is_some() => {
            Some(def)
        }
        _ => None,
    }
}

/// The lambda that is directly the body of `lambda`, as Nix passes a
/// lambda's name on to it (`ExprLambda::setName`).
///
/// With defaulted formals, the body is inside the `nix_hir.let_in` that
/// [`crate::ast_to_hir`] binds them in, which the region yields; a `let`
/// written in the source is inside that one, and is not looked through.
fn body_lambda(ctx: &Context, lambda: Ptr<Operation>) -> Option<Ptr<Operation>> {
    let block = region_block(ctx, lambda, 0);
    let has_defaults = block
        .deref(ctx)
        .iter(ctx)
        .filter_map(|op| Operation::get_op::<hir::FormalsOp>(op, ctx))
        .any(|formals| {
            formals
                .get_attr_formals(ctx)
                .is_some_and(|spec| spec.formals.iter().any(|&(_, default)| default))
        });
    let mut body = yielded(ctx, block);
    if has_defaults {
        let DefiningEntity::Op(formals_let) = body.defining_entity() else {
            unreachable!("defaulted formals are bound in a nix_hir.let_in")
        };
        body = yielded(ctx, region_block(ctx, formals_let, 1));
    }
    lambda_of(ctx, body)
}

/// The value `block` yields.
fn yielded(ctx: &Context, block: Ptr<BasicBlock>) -> Value {
    let ret = block
        .deref(ctx)
        .get_terminator(ctx)
        .expect("value regions end in nix_hir.yield");
    ret.deref(ctx).get_operand(0)
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
        } else if let Some(bind) = Operation::get_op::<hir::BindOp>(op, ctx) {
            let name = bind_name(ctx, bind);
            let mut lambda = lambda_of(ctx, op.deref(ctx).get_operand(0));
            while let Some(named) = lambda {
                self.scopes.lambda_names.insert(named, name.clone());
                lambda = body_lambda(ctx, named);
            }
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
