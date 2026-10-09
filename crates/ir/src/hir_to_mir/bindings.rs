//! [`ToMIRDialect`] for attribute sets and `let`: the `nix_hir.bind` /
//! `nix_hir.dyn_bind` regions become `nix_mir.attrs` (plus `attrs_insert`
//! for dynamic keys) and, for recursive ones, a `nix_mir.letrec`, whose
//! members the `nix_hir.var` ops naming them become.

use std::collections::HashMap;

use pliron::basic_block::BasicBlock;
use pliron::context::{Context, Ptr};
use pliron::derive::op_interface_impl;
use pliron::irbuild::dialect_conversion::{DialectConversionRewriter, OperandsInfo};
use pliron::irbuild::rewriter::Rewriter;
use pliron::linked_list::ContainsLinkedList;
use pliron::location::Located;
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::region::Region;
use pliron::result::Result;
use pliron::value::Value;

use super::convert::{Builder, ConversionState, converted_with_parent};
use super::scopes::{Scopes, bind_name};
use super::{ToMIRDialect, region_block};
use crate::hir::{self, BinOp};
use crate::mir;

/// `let ... in body`: a thunk computing the group, then the body.
#[op_interface_impl]
impl ToMIRDialect for hir::LetInOp {
    fn rewrite(
        &self,
        ctx: &mut Context,
        rewriter: &mut DialectConversionRewriter,
        _operands_info: &OperandsInfo,
        state: &mut ConversionState,
    ) -> Result<()> {
        let op = self.get_operation();
        let (bindings, body) = (region_block(ctx, op, 0), region_block(ctx, op, 1));
        let mut b = Builder::new(ctx, rewriter, self);
        let thunk = b.thunk(|b| {
            let group = Recursive::build(b, bindings);
            b.move_ops(body);
            group.resolve(b, &state.scopes, op);
        });
        b.replace(thunk)
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::AttrSetOp {
    fn rewrite(
        &self,
        ctx: &mut Context,
        rewriter: &mut DialectConversionRewriter,
        _operands_info: &OperandsInfo,
        state: &mut ConversionState,
    ) -> Result<()> {
        let mut b = Builder::new(ctx, rewriter, self);
        let thunk = attr_set(&mut b, &state.scopes, self.get_operation(), false);
        b.replace(thunk)
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::RecAttrSetOp {
    fn rewrite(
        &self,
        ctx: &mut Context,
        rewriter: &mut DialectConversionRewriter,
        _operands_info: &OperandsInfo,
        state: &mut ConversionState,
    ) -> Result<()> {
        let mut b = Builder::new(ctx, rewriter, self);
        let thunk = attr_set(&mut b, &state.scopes, self.get_operation(), true);
        b.replace(thunk)
    }
}

/// Converted with its set or `let`.
#[op_interface_impl]
impl ToMIRDialect for hir::BindOp {
    fn rewrite(
        &self,
        ctx: &mut Context,
        _rewriter: &mut DialectConversionRewriter,
        _operands_info: &OperandsInfo,
        _state: &mut ConversionState,
    ) -> Result<()> {
        converted_with_parent(ctx, self)
    }
}

/// Converted with its set.
#[op_interface_impl]
impl ToMIRDialect for hir::DynBindOp {
    fn rewrite(
        &self,
        ctx: &mut Context,
        _rewriter: &mut DialectConversionRewriter,
        _operands_info: &OperandsInfo,
        _state: &mut ConversionState,
    ) -> Result<()> {
        converted_with_parent(ctx, self)
    }
}

/// Converted with the `let` / `rec { }` that binds its name, which becomes a
/// member of the group.
#[op_interface_impl]
impl ToMIRDialect for hir::VarOp {
    fn rewrite(
        &self,
        ctx: &mut Context,
        _rewriter: &mut DialectConversionRewriter,
        _operands_info: &OperandsInfo,
        _state: &mut ConversionState,
    ) -> Result<()> {
        converted_with_parent(ctx, self)
    }
}

/// `{ ... }` / `rec { ... }`. A set without dynamic keys cannot fail, so it
/// is built where it is defined; dynamic keys are evaluated with the set.
/// Returns the thunk of the set.
fn attr_set(b: &mut Builder, scopes: &Scopes, op: Ptr<Operation>, recursive: bool) -> Value {
    let block = region_block(b.ctx, op, 0);
    let build = |b: &mut Builder| {
        let bindings = if recursive {
            Recursive::build(b, block).resolve(b, scopes, op)
        } else {
            let ops = block.deref(b.ctx).iter(b.ctx).collect();
            let bindings = take_bindings(b, ops);
            b.move_ops(block);
            bindings
        };
        build_set(b, bindings)
    };
    if has_dynamic_keys(b.ctx, block) {
        b.thunk_of(build)
    } else {
        let set = build(b);
        b.evaluated(set)
    }
}

/// Whether the binding block `block` has `${key} = ...` bindings.
fn has_dynamic_keys(ctx: &Context, block: Ptr<BasicBlock>) -> bool {
    block
        .deref(ctx)
        .iter(ctx)
        .any(|op| Operation::get_op::<hir::DynBindOp>(op, ctx).is_some())
}

/// The bindings of a set or `let`.
#[derive(Default)]
struct Bindings {
    statics: Vec<(String, Value)>,
    dynamics: Vec<(Value, Value)>,
}

/// Collects the bindings made by `ops` and erases the binding ops.
fn take_bindings(b: &mut Builder, ops: Vec<Ptr<Operation>>) -> Bindings {
    let mut bindings = Bindings::default();
    for op in ops {
        let operand = |ctx: &Context, i| op.deref(ctx).get_operand(i);
        if let Some(bind) = Operation::get_op::<hir::BindOp>(op, b.ctx) {
            let name = bind_name(b.ctx, bind);
            bindings.statics.push((name, operand(b.ctx, 0)));
        } else if Operation::get_op::<hir::DynBindOp>(op, b.ctx).is_some() {
            let binding = (operand(b.ctx, 0), operand(b.ctx, 1));
            bindings.dynamics.push(binding);
        } else {
            continue;
        }
        b.rewriter.erase_operation(b.ctx, op);
    }
    bindings
}

/// Builds the set of `bindings` at the insertion point: static keys first,
/// then the dynamic ones in order.
fn build_set(b: &mut Builder, bindings: Bindings) -> Value {
    let (names, values) = bindings.statics.into_iter().unzip();
    let attrs = mir::AttrsOp::new(b.ctx, names, values);
    let mut set = b.value(attrs);
    for (key, value) in bindings.dynamics {
        let key = b.force(key);
        let insert = mir::AttrsInsertOp::new(b.ctx, set, key, value);
        set = b.value(insert);
    }
    set
}

/// The binding block of a `let` or `rec { }`, turned into a `letrec` at the
/// insertion point.
///
/// Every value of the block that is not computed at its only use is a member
/// of the group, so that the bindings can refer to all of them. The other
/// values are moved with the member (or dynamic key) that uses them.
struct Recursive {
    /// The `bind` / `dyn_bind` ops, moved to the insertion point.
    statements: Vec<Ptr<Operation>>,
    /// The value each name is bound to, as in the HIR.
    bound: HashMap<String, Value>,
    /// The values of the members, in order.
    members: Vec<Value>,
    group: Option<Group>,
}

impl Recursive {
    fn build(b: &mut Builder, block: Ptr<BasicBlock>) -> Recursive {
        let ctx = &*b.ctx;
        let (values, statements): (Vec<_>, Vec<_>) = block
            .deref(ctx)
            .iter(ctx)
            .partition(|op| op.deref(ctx).get_num_results() == 1);
        let bound = statements
            .iter()
            .filter_map(|&op| Operation::get_op::<hir::BindOp>(op, ctx))
            .map(|bind| {
                (
                    bind_name(ctx, bind),
                    bind.get_operation().deref(ctx).get_operand(0),
                )
            })
            .collect();
        let members: Vec<Ptr<Operation>> = values
            .iter()
            .copied()
            .filter(|&op| !is_inline(ctx, op))
            .collect();
        // The member each value is computed in; `None` for the insertion
        // point (dynamic keys, which are evaluated with the set).
        let mut places: HashMap<Ptr<Operation>, Option<usize>> = members
            .iter()
            .enumerate()
            .map(|(i, &m)| (m, Some(i)))
            .collect();
        for &op in values.iter().rev() {
            if !places.contains_key(&op) {
                let user = result(ctx, op).uses(ctx)[0].user_op();
                let place = places.get(&user).copied().flatten();
                places.insert(op, place);
            }
        }
        let group = (!members.is_empty()).then(|| Group::new(b, members.len()));
        for &op in &values {
            match (places[&op], &group) {
                (Some(i), Some(group)) => {
                    let body = group.op.body(b.ctx, i);
                    b.at_end_of(body, |b| b.move_op(op));
                }
                _ => b.move_op(op),
            }
        }
        for &op in &statements {
            b.move_op(op);
        }
        if let Some(group) = &group {
            for (i, &member) in members.iter().enumerate() {
                let loc = member.deref(b.ctx).loc();
                let ret = hir::YieldOp::new(b.ctx, result(b.ctx, member));
                let body = group.op.body(b.ctx, i);
                b.at_end_of(body, |b| b.insert(&ret));
                ret.get_operation().deref_mut(b.ctx).set_loc(loc);
            }
        }
        Recursive {
            statements,
            bound,
            members: members.iter().map(|&m| result(b.ctx, m)).collect(),
            group,
        }
    }

    /// Makes every reference to a member (a use of its value, or a
    /// `nix_hir.var` naming it) refer to the group, once everything the
    /// names are visible in has been moved to its place. Returns the
    /// bindings, which then refer to the group too.
    fn resolve(self, b: &mut Builder, scopes: &Scopes, binder: Ptr<Operation>) -> Bindings {
        let member_of: HashMap<Value, usize> = self
            .members
            .iter()
            .enumerate()
            .map(|(i, &value)| (value, i))
            .collect();
        if let Some(group) = &self.group {
            for (j, &value) in self.members.iter().enumerate() {
                for r#use in value.uses(b.ctx) {
                    match group.region_of(b.ctx, r#use.user_op()) {
                        // The member's own region, where it is defined.
                        Some(k) if k == j => {}
                        place => {
                            let member = group.member(b.ctx, j, place);
                            value.replace_use_with(b.ctx, r#use, &member);
                        }
                    }
                }
            }
        }
        for &var in scopes.vars.get(&binder).into_iter().flatten() {
            let name = Operation::get_op::<hir::VarOp>(var, b.ctx)
                .expect("a var")
                .get_attr_var_name(b.ctx)
                .expect("vars are named")
                .as_str()
                .to_string();
            let target = self.bound[&name];
            let value = result(b.ctx, var);
            match (&self.group, member_of.get(&target)) {
                (Some(group), Some(&j)) => {
                    for r#use in value.uses(b.ctx) {
                        let place = group.region_of(b.ctx, r#use.user_op());
                        let member = group.member(b.ctx, j, place);
                        value.replace_use_with(b.ctx, r#use, &member);
                    }
                }
                // Inherited from the enclosing scope.
                _ => value.replace_all_uses_with(b.ctx, &target),
            }
            b.rewriter.erase_operation(b.ctx, var);
        }
        take_bindings(b, self.statements)
    }
}

/// A `letrec`, with the index of each of its regions.
struct Group {
    op: mir::LetrecOp,
    regions: HashMap<Ptr<Region>, usize>,
    /// The block the `letrec` is in.
    block: Ptr<BasicBlock>,
}

impl Group {
    /// Inserts a `letrec` of `n` members at the insertion point.
    fn new(b: &mut Builder, n: usize) -> Group {
        let op = mir::LetrecOp::new(b.ctx, n);
        b.insert(&op);
        let operation = op.get_operation().deref(b.ctx);
        Group {
            regions: operation.regions().zip(0..).collect(),
            block: operation.get_parent_block().expect("inserted"),
            op,
        }
    }

    /// The index of the region of the group that `op` is nested in, or
    /// `None` if it is outside the group (but in the same block).
    fn region_of(&self, ctx: &Context, op: Ptr<Operation>) -> Option<usize> {
        let mut block = op.deref(ctx).get_parent_block().expect("users are linked");
        while block != self.block {
            let region = block
                .deref(ctx)
                .get_parent_region()
                .expect("blocks are linked");
            if let Some(&i) = self.regions.get(&region) {
                return Some(i);
            }
            block = region
                .deref(ctx)
                .get_parent_block(ctx)
                .expect("references are nested in the block of the group");
        }
        None
    }

    /// Member `j`, as seen from region `place`, or from outside the group.
    fn member(&self, ctx: &Context, j: usize, place: Option<usize>) -> Value {
        match place {
            Some(k) => self.op.body(ctx, k).deref(ctx).get_argument(j),
            None => self.op.get_operation().deref(ctx).get_result(j),
        }
    }
}

fn result(ctx: &Context, op: Ptr<Operation>) -> Value {
    op.deref(ctx).get_result(0)
}

/// Whether the value of `op` is used once, strictly (or conditionally), in
/// its own block: it is then computed at that use, not in a member of its
/// own.
fn is_inline(ctx: &Context, op: Ptr<Operation>) -> bool {
    let value = result(ctx, op);
    let uses = value.uses(ctx);
    let [single] = uses.as_slice() else {
        return false;
    };
    let user = single.user_op();
    let same_block = user.deref(ctx).get_parent_block() == op.deref(ctx).get_parent_block();
    let index = user
        .deref(ctx)
        .operands()
        .position(|operand| operand == value)
        .expect("a user has the value as an operand");
    same_block
        && matches!(
            operand_usage(ctx, user, index),
            Usage::Strict | Usage::Conditional
        )
}

/// How an op uses one of its operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Usage {
    Strict,
    /// Evaluated only on one branch (the right side of `&&`, `||`, `->`).
    Conditional,
    Lazy,
}

/// How the HIR op `user` uses its operand `index`.
fn operand_usage(ctx: &Context, user: Ptr<Operation>, index: usize) -> Usage {
    let is = |f: fn(&Context, Ptr<Operation>) -> bool| f(ctx, user);
    if is(|c, o| Operation::get_op::<hir::ApplyOp>(o, c).is_some()) {
        return if index == 0 {
            Usage::Strict
        } else {
            Usage::Lazy
        };
    }
    if let Some(binary) = Operation::get_op::<hir::BinaryOp>(user, ctx) {
        let lazy_rhs = matches!(
            binary.get_attr_binop(ctx).map(|b| *b),
            Some(BinOp::And | BinOp::Or | BinOp::Imply)
        );
        return if lazy_rhs && index == 1 {
            Usage::Conditional
        } else {
            Usage::Strict
        };
    }
    if is(|c, o| Operation::get_op::<hir::DynBindOp>(o, c).is_some()) {
        return if index == 0 {
            Usage::Strict
        } else {
            Usage::Lazy
        };
    }
    if is(|c, o| {
        Operation::get_op::<hir::ListOp>(o, c).is_some()
            || Operation::get_op::<hir::BindOp>(o, c).is_some()
            || Operation::get_op::<hir::WithOp>(o, c).is_some()
    }) {
        return Usage::Lazy;
    }
    // select, has_attr, unary, interpolate, if, assert, formals, yield.
    Usage::Strict
}
