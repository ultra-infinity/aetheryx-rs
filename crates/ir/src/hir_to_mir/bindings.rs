//! Attribute sets and `let`: `nix_hir.bind` regions become `nix_mir.attrs`
//! (plus `attrs_insert` for dynamic keys) and, for recursive ones, a
//! `nix_mir.letrec`.

use std::collections::HashMap;

use pliron::basic_block::BasicBlock;
use pliron::context::{Context, Ptr};
use pliron::linked_list::ContainsLinkedList;
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::value::{DefiningEntity, Value};

use super::{Binding, Lowerer, Mode};
use crate::hir;
use crate::mir::{self, Ty};

/// Whether the attribute set `op` has `${key} = ...` bindings, whose keys are
/// evaluated when the set is.
pub(super) fn has_dynamic_keys(ctx: &Context, op: Ptr<Operation>) -> bool {
    let block = op
        .deref(ctx)
        .get_region(0)
        .deref(ctx)
        .get_head()
        .expect("HIR regions have one block");
    block
        .deref(ctx)
        .iter(ctx)
        .any(|op| Operation::get_op::<hir::DynBindOp>(op, ctx).is_some())
}

/// The bindings of a set or `let`, as HIR values.
#[derive(Default)]
struct Bindings {
    statics: Vec<(String, Value)>,
    dynamics: Vec<(Value, Value)>,
}

fn collect_bindings(ctx: &Context, block: Ptr<BasicBlock>) -> Bindings {
    let mut bindings = Bindings::default();
    for op in block.deref(ctx).iter(ctx) {
        let operand = |i| op.deref(ctx).get_operand(i);
        if let Some(bind) = Operation::get_op::<hir::BindOp>(op, ctx) {
            let name = bind
                .get_attr_bind_name(ctx)
                .expect("named")
                .as_str()
                .to_string();
            bindings.statics.push((name, operand(0)));
        } else if Operation::get_op::<hir::DynBindOp>(op, ctx).is_some() {
            bindings.dynamics.push((operand(0), operand(1)));
        }
    }
    bindings
}

impl Lowerer<'_> {
    /// `{ ... }`: the bindings are lowered in order, then the set is built.
    pub(super) fn compute_attr_set(&mut self, op: Ptr<Operation>, block: Ptr<BasicBlock>) -> Value {
        let hir_block = self.hir_region_block(op, 0);
        let ops: Vec<_> = hir_block.deref(self.ctx).iter(self.ctx).collect();
        for op in ops {
            if self.mode(op) != Mode::Statement {
                self.materialize(op, block);
            }
        }
        let bindings = collect_bindings(self.ctx, hir_block);
        self.build_set(block, bindings)
    }

    /// `rec { ... }`: a `letrec` of the bindings, then the set.
    pub(super) fn compute_rec_attr_set(
        &mut self,
        op: Ptr<Operation>,
        block: Ptr<BasicBlock>,
    ) -> Value {
        let hir_block = self.hir_region_block(op, 0);
        let bindings = self.lower_recursive(hir_block, block);
        // Dynamic keys are evaluated in the recursive scope.
        let set = self.build_set(block, bindings);
        self.scopes.pop();
        set
    }

    /// `let ... in body`: a `letrec` of the bindings, then the body.
    pub(super) fn compute_let_in(&mut self, op: Ptr<Operation>, block: Ptr<BasicBlock>) -> Value {
        self.lower_recursive(self.hir_region_block(op, 0), block);
        let value = self.lower_block(self.hir_region_block(op, 1), block);
        self.scopes.pop();
        value
    }

    /// Builds the set of `bindings` in `block`: static keys first, then the
    /// dynamic ones in order.
    fn build_set(&mut self, block: Ptr<BasicBlock>, bindings: Bindings) -> Value {
        let (names, values): (Vec<_>, Vec<_>) = bindings
            .statics
            .into_iter()
            .map(|(name, value)| (name, self.operand(block, value, Ty::Thunk)))
            .unzip();
        let attrs = mir::AttrsOp::new(self.ctx, names, values);
        let mut set = self.emit(block, attrs);
        for (key, value) in bindings.dynamics {
            let key = self.operand(block, key, Ty::Value);
            let value = self.operand(block, value, Ty::Thunk);
            let insert = mir::AttrsInsertOp::new(self.ctx, set, key, value);
            set = self.emit(block, insert);
        }
        set
    }

    /// Lowers the recursive binding block `hir_block` into a `letrec` in
    /// `block`, and pushes the scope of the names it binds (the caller pops
    /// it). Returns the bindings.
    fn lower_recursive(&mut self, hir_block: Ptr<BasicBlock>, block: Ptr<BasicBlock>) -> Bindings {
        let ops: Vec<_> = hir_block.deref(self.ctx).iter(self.ctx).collect();
        // Every value of the block that is not computed at its use is a
        // member, so that all of them can refer to the names being bound.
        let members: Vec<Ptr<Operation>> = ops
            .into_iter()
            .filter(|&op| !matches!(self.mode(op), Mode::Statement | Mode::Inline))
            .collect();
        let bindings = collect_bindings(self.ctx, hir_block);

        let letrec = (!members.is_empty()).then(|| {
            let letrec = mir::LetrecOp::new(self.ctx, members.len());
            self.at(members[0], |this| this.append(block, &letrec));
            letrec
        });
        let results: Vec<Value> = letrec
            .map(|l| l.get_operation().deref(self.ctx).results().collect())
            .unwrap_or_default();
        for (member, &result) in members.iter().zip(&results) {
            let hir_result = member.deref(self.ctx).get_result(0);
            let binding = self.here(result);
            self.env.insert(hir_result, binding);
        }

        let scope: HashMap<String, Binding> = bindings
            .statics
            .iter()
            .map(|(name, value)| (name.clone(), self.binding_of(*value)))
            .collect();
        self.scopes.push(scope);

        if let Some(letrec) = letrec {
            for (i, &member) in members.iter().enumerate() {
                let body = letrec.body(self.ctx, i);
                let args: Vec<Value> = body.deref(self.ctx).arguments().collect();
                let aliases = results.iter().copied().zip(args).collect();
                self.in_frame(letrec.get_operation(), i, aliases, |this| {
                    let value = this.at(member, |this| this.compute(member, body));
                    let ret = mir::YieldOp::new(this.ctx, value);
                    this.at(member, |this| this.append(body, &ret));
                });
            }
        }
        bindings
    }

    /// The binding of a bound HIR value: a member of the group being built,
    /// or a value lowered before (an `inherit` from the enclosing scope).
    fn binding_of(&self, value: Value) -> Binding {
        if let Some(binding) = self.env.get(&value) {
            return *binding;
        }
        let DefiningEntity::Op(def) = value.defining_entity() else {
            unreachable!("bound values are lowered before their binding")
        };
        unreachable!(
            "{} is bound but neither a group member nor lowered before",
            Operation::get_opid(def, self.ctx)
        )
    }
}
