//! Lowering from `nix_hir` to `nix_mir`.
//!
//! The entry point is [`lower_module`].
//!
//! # Laziness
//! Every HIR value gets a *mode*, decided from its uses, that says where and
//! how it is computed:
//! - **trivial** (`const`, `nix_builtins.ref`, `lambda`, `var`, attribute
//!   sets without dynamic keys): computed where it is defined; it cannot fail,
//!   so it is never wrapped in a thunk (like Nix's `maybeThunk`).
//! - **eager** (`formals`): computed where it is defined, because Nix checks
//!   the pattern when the function is called.
//! - **inline**: used exactly once, strictly (or conditionally, for the right
//!   side of `&&`, `||`, `->`), in the same block: computed at that use.
//! - **lazy**: everything else, wrapped in a `nix_mir.thunk` where it is
//!   defined and forced by strict uses.
//!
//! These are the positions where Nix allocates thunks: function arguments,
//! list elements, attribute and `let` values, `with` namespaces.
//!
//! # Closures
//! `thunk`, `lambda` and `letrec` regions are isolated, so each one is a
//! *frame*. Using a value from an outer frame captures it through every frame
//! in between.
//!
//! # Recursion
//! The binding block of a `let` or `rec { }` becomes one `nix_mir.letrec`
//! holding every non-inline value of the block, so the bindings can refer to
//! each other. `nix_hir.var` resolves to the group member a name is bound to.

mod bindings;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use pliron::basic_block::BasicBlock;
use pliron::builtin::op_interfaces::{SingleBlockRegionInterface, SymbolOpInterface};
use pliron::builtin::ops::{FuncOp, ModuleOp};
use pliron::builtin::types::FunctionType;
use pliron::context::{Context, Ptr};
use pliron::linked_list::ContainsLinkedList;
use pliron::location::{Located, Location};
use pliron::op::{Op, verify_op};
use pliron::operation::Operation;
use pliron::result::Result;
use pliron::value::{DefiningEntity, Value};

use crate::builtins_dialect::RefOp;
use crate::hir::{self, BinOp};
use crate::mir::{self, Ty};

/// Lowers a module produced by [`crate::ast_to_hir`] to a verified `nix_mir`
/// module with the same name, containing `@main` of type
/// `() -> (nix_mir.value)`.
pub fn lower_module(ctx: &mut Context, hir: ModuleOp) -> Result<ModuleOp> {
    let hir_main = hir
        .get_body(ctx, 0)
        .deref(ctx)
        .iter(ctx)
        .find_map(|op| Operation::get_op::<FuncOp>(op, ctx))
        .expect("HIR modules contain @main");

    let name = hir.get_symbol_name(ctx);
    let module = ModuleOp::new(ctx, name);
    let value_ty = Ty::Value.handle(ctx);
    let func_ty = FunctionType::get(ctx, vec![], vec![value_ty]);
    let main = FuncOp::new(ctx, hir_main.get_symbol_name(ctx), func_ty);
    module.append_operation(ctx, main.get_operation(), 0);
    for (from, to) in [
        (hir.get_operation(), module.get_operation()),
        (hir_main.get_operation(), main.get_operation()),
    ] {
        let loc = from.deref(ctx).loc();
        to.deref_mut(ctx).set_loc(loc);
    }

    let entry = main.get_entry_block(ctx);
    let lambda_names = lambda_names(ctx, hir.get_operation());
    let mut lowerer = Lowerer {
        ctx,
        frames: vec![Frame {
            op: main.get_operation(),
            region: 0,
            aliases: HashMap::new(),
        }],
        captures: HashMap::new(),
        env: HashMap::new(),
        scopes: Vec::new(),
        withs: Vec::new(),
        lambda_names,
        loc: Location::Unknown,
    };
    let value = lowerer.lower_block(hir_main.get_entry_block(lowerer.ctx), entry);
    let ret = mir::YieldOp::new(lowerer.ctx, value);
    lowerer.append(entry, &ret);

    verify_op(&module, ctx)?;
    Ok(module)
}

/// Names lambdas after the binding they are the value of (`f = x: ...;`), as
/// Nix does for its error messages.
fn lambda_names(ctx: &Context, root: Ptr<Operation>) -> HashMap<Ptr<Operation>, String> {
    let mut names = HashMap::new();
    let mut stack = vec![root];
    while let Some(op) = stack.pop() {
        if let Some(bind) = Operation::get_op::<hir::BindOp>(op, ctx) {
            let value = op.deref(ctx).get_operand(0);
            if let DefiningEntity::Op(def) = value.defining_entity()
                && Operation::get_op::<hir::LambdaOp>(def, ctx).is_some()
                && let Some(name) = bind.get_attr_bind_name(ctx)
            {
                names.insert(def, name.as_str().to_string());
            }
        }
        for region in op.deref(ctx).regions() {
            for block in region.deref(ctx).iter(ctx) {
                stack.extend(block.deref(ctx).iter(ctx));
            }
        }
    }
    names
}

/// Where and how a HIR value is computed; see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// An op without results (`bind`, `dyn_bind`, `yield`).
    Statement,
    Trivial,
    Eager,
    Inline,
    Lazy,
}

/// How an op uses one of its operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    Strict,
    /// Evaluated only on one branch (the right side of `&&`, `||`, `->`).
    Conditional,
    Lazy,
}

/// A MIR value and the frame it is defined in.
#[derive(Debug, Clone, Copy)]
struct Binding {
    depth: usize,
    value: Value,
}

/// An isolated region being filled.
struct Frame {
    op: Ptr<Operation>,
    /// Which region of `op` (for `letrec`, one per member).
    region: usize,
    /// Values of the enclosing frame that are available here without being
    /// captured: the members of a `letrec` inside its own regions.
    aliases: HashMap<Value, Value>,
}

struct Lowerer<'a> {
    ctx: &'a mut Context,
    /// Innermost last; `frames[0]` is `@main`'s body.
    frames: Vec<Frame>,
    /// The block arguments created for each (isolated op, captured value).
    captures: HashMap<(Ptr<Operation>, Value), Vec<Value>>,
    /// The MIR value of each lowered HIR value.
    env: HashMap<Value, Binding>,
    /// The names bound by the enclosing `let` / `rec { }`, innermost last.
    scopes: Vec<HashMap<String, Binding>>,
    /// The enclosing `with` namespaces, innermost last.
    withs: Vec<Binding>,
    lambda_names: HashMap<Ptr<Operation>, String>,
    /// The location given to emitted ops: that of the HIR op being lowered.
    loc: Location,
}

impl Lowerer<'_> {
    // ─── Emission ───────────────────────────────────────────────────────────

    fn append(&mut self, block: Ptr<BasicBlock>, op: &impl Op) {
        let op = op.get_operation();
        op.deref_mut(self.ctx).set_loc(self.loc.clone());
        op.insert_at_back(block, self.ctx);
    }

    /// Appends a single-result op and returns its result.
    fn emit(&mut self, block: Ptr<BasicBlock>, op: impl Op) -> Value {
        self.append(block, &op);
        mir::result_of(self.ctx, &op)
    }

    /// Converts `value` to `ty`, forcing a thunk or wrapping a value.
    fn convert(&mut self, block: Ptr<BasicBlock>, value: Value, ty: Ty) -> Value {
        match (Ty::of(self.ctx, value), ty) {
            (Some(Ty::Thunk), Ty::Value) => {
                let op = mir::ForceOp::new(self.ctx, value);
                self.emit(block, op)
            }
            (Some(Ty::Value), Ty::Thunk) => {
                let op = mir::EvaluatedOp::new(self.ctx, value);
                self.emit(block, op)
            }
            _ => value,
        }
    }

    // ─── Frames ─────────────────────────────────────────────────────────────

    fn depth(&self) -> usize {
        self.frames.len() - 1
    }

    fn here(&self, value: Value) -> Binding {
        Binding {
            depth: self.depth(),
            value,
        }
    }

    /// `binding` as a value usable in the innermost frame, capturing it
    /// through every frame in between.
    fn import(&mut self, binding: Binding) -> Value {
        let mut value = binding.value;
        for depth in binding.depth + 1..self.frames.len() {
            let frame = &self.frames[depth];
            if let Some(alias) = frame.aliases.get(&value) {
                value = *alias;
                continue;
            }
            let (op, region) = (frame.op, frame.region);
            let args = match self.captures.get(&(op, value)) {
                Some(args) => args.clone(),
                None => {
                    let args = mir::push_capture(self.ctx, op, value);
                    self.captures.insert((op, value), args.clone());
                    args
                }
            };
            value = args[region];
        }
        value
    }

    /// Runs `f` inside region `region` of the isolated `op`.
    fn in_frame<T>(
        &mut self,
        op: Ptr<Operation>,
        region: usize,
        aliases: HashMap<Value, Value>,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.frames.push(Frame {
            op,
            region,
            aliases,
        });
        let result = f(self);
        self.frames.pop();
        result
    }

    // ─── Modes ──────────────────────────────────────────────────────────────

    fn mode(&self, op: Ptr<Operation>) -> Mode {
        let ctx = &*self.ctx;
        let is = |f: fn(&Context, Ptr<Operation>) -> bool| f(ctx, op);
        if op.deref(ctx).get_num_results() == 0 {
            return Mode::Statement;
        }
        if is(|c, o| Operation::get_op::<hir::FormalsOp>(o, c).is_some()) {
            return Mode::Eager;
        }
        let trivial = is(|c, o| {
            Operation::get_op::<hir::ConstOp>(o, c).is_some()
                || Operation::get_op::<RefOp>(o, c).is_some()
                || Operation::get_op::<hir::LambdaOp>(o, c).is_some()
                || Operation::get_op::<hir::VarOp>(o, c).is_some()
        }) || (is(|c, o| {
            Operation::get_op::<hir::AttrSetOp>(o, c).is_some()
                || Operation::get_op::<hir::RecAttrSetOp>(o, c).is_some()
        }) && !bindings::has_dynamic_keys(ctx, op));
        if trivial {
            return Mode::Trivial;
        }
        let result = op.deref(ctx).get_result(0);
        let uses = result.uses(ctx);
        if let [single] = uses.as_slice() {
            let user = single.user_op();
            let same_block = user.deref(ctx).get_parent_block() == op.deref(ctx).get_parent_block();
            let index = user
                .deref(ctx)
                .operands()
                .position(|operand| operand == result);
            let usage = index.map(|i| operand_use(ctx, user, i));
            if same_block && matches!(usage, Some(Use::Strict | Use::Conditional)) {
                return Mode::Inline;
            }
        }
        Mode::Lazy
    }

    // ─── Blocks ─────────────────────────────────────────────────────────────

    /// Lowers the ops of `hir_block` into `block` and returns the value the
    /// block yields, evaluated.
    fn lower_block(&mut self, hir_block: Ptr<BasicBlock>, block: Ptr<BasicBlock>) -> Value {
        let ops: Vec<_> = hir_block.deref(self.ctx).iter(self.ctx).collect();
        for op in ops {
            if let Some(ret) = Operation::get_op::<hir::YieldOp>(op, self.ctx) {
                let value = ret.get_operation().deref(self.ctx).get_operand(0);
                return self.at(op, |this| this.operand(block, value, Ty::Value));
            }
            self.materialize(op, block);
        }
        unreachable!("HIR value regions end in nix_hir.yield")
    }

    /// Lowers `op` where it is defined, according to its mode.
    fn materialize(&mut self, op: Ptr<Operation>, block: Ptr<BasicBlock>) {
        match self.mode(op) {
            Mode::Inline => {}
            Mode::Statement => unreachable!("binding ops are lowered with their set"),
            Mode::Trivial if Operation::get_op::<hir::VarOp>(op, self.ctx).is_some() => {
                let binding = self.resolve_var(op);
                self.env.insert(self.result(op), binding);
            }
            Mode::Trivial | Mode::Eager => {
                let value = self.at(op, |this| this.compute(op, block));
                let binding = self.here(value);
                self.env.insert(self.result(op), binding);
            }
            Mode::Lazy => {
                let thunk = self.at(op, |this| {
                    let thunk = mir::ThunkOp::new(this.ctx);
                    this.append(block, &thunk);
                    let body = thunk.body(this.ctx);
                    this.in_frame(thunk.get_operation(), 0, HashMap::new(), |this| {
                        let value = this.compute(op, body);
                        let ret = mir::YieldOp::new(this.ctx, value);
                        this.append(body, &ret);
                    });
                    mir::result_of(this.ctx, &thunk)
                });
                let binding = self.here(thunk);
                self.env.insert(self.result(op), binding);
            }
        }
    }

    /// Runs `f` with emitted ops located at `op`.
    fn at<T>(&mut self, op: Ptr<Operation>, f: impl FnOnce(&mut Self) -> T) -> T {
        let loc = op.deref(self.ctx).loc();
        let saved = std::mem::replace(&mut self.loc, loc);
        let result = f(self);
        self.loc = saved;
        result
    }

    /// The MIR value of the HIR value `value` in `block`, as `ty`.
    fn operand(&mut self, block: Ptr<BasicBlock>, value: Value, ty: Ty) -> Value {
        let mir_value = match self.env.get(&value).copied() {
            Some(binding) => self.import(binding),
            None => {
                let DefiningEntity::Op(def) = value.defining_entity() else {
                    unreachable!("block arguments are bound when their region is entered")
                };
                debug_assert_eq!(self.mode(def), Mode::Inline);
                self.at(def, |this| this.compute(def, block))
            }
        };
        self.convert(block, mir_value, ty)
    }

    fn result(&self, op: Ptr<Operation>) -> Value {
        op.deref(self.ctx).get_result(0)
    }

    fn hir_operand(&self, op: Ptr<Operation>, i: usize) -> Value {
        op.deref(self.ctx).get_operand(i)
    }

    fn hir_region_block(&self, op: Ptr<Operation>, i: usize) -> Ptr<BasicBlock> {
        op.deref(self.ctx)
            .get_region(i)
            .deref(self.ctx)
            .get_head()
            .expect("HIR regions have one block")
    }

    // ─── Ops ────────────────────────────────────────────────────────────────

    /// Computes the HIR op `op` in `block` and returns its value, evaluated.
    fn compute(&mut self, op: Ptr<Operation>, block: Ptr<BasicBlock>) -> Value {
        let ctx = &*self.ctx;
        macro_rules! is {
            ($t:ty) => {
                Operation::get_op::<$t>(op, ctx)
            };
        }
        if let Some(c) = is!(hir::ConstOp) {
            let literal = c
                .get_attr_literal(ctx)
                .expect("consts have a literal")
                .clone();
            let op = mir::ConstOp::new(self.ctx, literal);
            return self.emit(block, op);
        }
        if let Some(r) = is!(RefOp) {
            let name = r.builtin(ctx).expect("verified").name;
            let ty = Ty::Value.handle(self.ctx);
            let op = RefOp::new(self.ctx, name, ty);
            return self.emit(block, op);
        }
        if is!(hir::VarOp).is_some() {
            let binding = self.resolve_var(op);
            let thunk = self.import(binding);
            return self.convert(block, thunk, Ty::Value);
        }
        if let Some(lookup) = is!(hir::LookupOp) {
            let name = lookup
                .get_attr_lookup_name(ctx)
                .expect("named")
                .as_str()
                .to_string();
            let withs = self.withs.clone();
            let namespaces = withs.into_iter().rev().map(|ns| self.import(ns)).collect();
            let op = mir::WithLookupOp::new(self.ctx, &name, namespaces);
            let thunk = self.emit(block, op);
            return self.convert(block, thunk, Ty::Value);
        }
        if is!(hir::LambdaOp).is_some() {
            return self.compute_lambda(op, block);
        }
        if let Some(formals) = is!(hir::FormalsOp) {
            let spec = formals.get_attr_formals(ctx).expect("has formals").clone();
            let arg = self.operand(block, self.hir_operand(op, 0), Ty::Value);
            let op = mir::FormalsOp::new(self.ctx, arg, spec);
            return self.emit(block, op);
        }
        if is!(hir::ApplyOp).is_some() {
            let f = self.operand(block, self.hir_operand(op, 0), Ty::Value);
            let x = self.operand(block, self.hir_operand(op, 1), Ty::Thunk);
            let op = mir::CallOp::new(self.ctx, f, x);
            return self.emit(block, op);
        }
        if is!(hir::SelectOp).is_some() {
            let set = self.operand(block, self.hir_operand(op, 0), Ty::Value);
            let key = self.operand(block, self.hir_operand(op, 1), Ty::Value);
            let op = mir::SelectOp::new(self.ctx, set, key);
            let thunk = self.emit(block, op);
            return self.convert(block, thunk, Ty::Value);
        }
        if is!(hir::HasAttrOp).is_some() {
            let set = self.operand(block, self.hir_operand(op, 0), Ty::Value);
            let key = self.operand(block, self.hir_operand(op, 1), Ty::Value);
            let op = mir::HasAttrOp::new(self.ctx, set, key);
            return self.emit(block, op);
        }
        if let Some(unary) = is!(hir::UnaryOp) {
            let unop = *unary.get_attr_unop(ctx).expect("has an operator");
            let operand = self.operand(block, self.hir_operand(op, 0), Ty::Value);
            let op = mir::UnaryOp::new(self.ctx, unop, operand);
            return self.emit(block, op);
        }
        if let Some(binary) = is!(hir::BinaryOp) {
            let binop = *binary.get_attr_binop(ctx).expect("has an operator");
            return self.compute_binary(op, binop, block);
        }
        if let Some(interp) = is!(hir::InterpolateOp) {
            let kind = *interp.get_attr_interp_kind(ctx).expect("has a kind");
            let n = op.deref(ctx).get_num_operands();
            let parts = (0..n)
                .map(|i| self.operand(block, self.hir_operand(op, i), Ty::Value))
                .collect();
            let op = mir::InterpolateOp::new(self.ctx, kind, parts);
            return self.emit(block, op);
        }
        if is!(hir::ListOp).is_some() {
            let n = op.deref(ctx).get_num_operands();
            let items = (0..n)
                .map(|i| self.operand(block, self.hir_operand(op, i), Ty::Thunk))
                .collect();
            let op = mir::ListOp::new(self.ctx, items);
            return self.emit(block, op);
        }
        if is!(hir::IfOp).is_some() {
            let cond = self.operand(block, self.hir_operand(op, 0), Ty::Value);
            let if_op = mir::IfOp::new(self.ctx, cond);
            self.append(block, &if_op);
            for (i, branch) in [if_op.then_block(self.ctx), if_op.else_block(self.ctx)]
                .into_iter()
                .enumerate()
            {
                let value = self.lower_block(self.hir_region_block(op, i), branch);
                let ret = mir::YieldOp::new(self.ctx, value);
                self.append(branch, &ret);
            }
            return mir::result_of(self.ctx, &if_op);
        }
        if is!(hir::AssertOp).is_some() {
            let cond = self.operand(block, self.hir_operand(op, 0), Ty::Value);
            let assert = mir::AssertOp::new(self.ctx, cond);
            self.append(block, &assert);
            return self.lower_block(self.hir_region_block(op, 0), block);
        }
        if is!(hir::WithOp).is_some() {
            let namespace = self.operand(block, self.hir_operand(op, 0), Ty::Thunk);
            let binding = self.here(namespace);
            self.withs.push(binding);
            let value = self.lower_block(self.hir_region_block(op, 0), block);
            self.withs.pop();
            return value;
        }
        if is!(hir::AttrSetOp).is_some() {
            return self.compute_attr_set(op, block);
        }
        if is!(hir::RecAttrSetOp).is_some() {
            return self.compute_rec_attr_set(op, block);
        }
        if is!(hir::LetInOp).is_some() {
            return self.compute_let_in(op, block);
        }
        unreachable!(
            "unexpected op in HIR: {}",
            Operation::get_opid(op, self.ctx)
        )
    }

    fn compute_lambda(&mut self, op: Ptr<Operation>, block: Ptr<BasicBlock>) -> Value {
        let lambda = mir::LambdaOp::new(self.ctx);
        if let Some(name) = self.lambda_names.get(&op) {
            lambda.set_name(self.ctx, name);
        }
        self.append(block, &lambda);
        let body = lambda.body(self.ctx);
        let hir_body = self.hir_region_block(op, 0);
        let hir_arg = hir_body.deref(self.ctx).get_argument(0);
        self.in_frame(lambda.get_operation(), 0, HashMap::new(), |this| {
            let arg = lambda.argument(this.ctx);
            let binding = this.here(arg);
            this.env.insert(hir_arg, binding);
            let value = this.lower_block(hir_body, body);
            let ret = mir::YieldOp::new(this.ctx, value);
            this.append(body, &ret);
        });
        mir::result_of(self.ctx, &lambda)
    }

    /// Strict operators map to `nix_mir.binary`; `&&`, `||` and `->` become
    /// `nix_mir.if`, evaluating (and type-checking) the right side only on
    /// the branch that needs it.
    fn compute_binary(
        &mut self,
        op: Ptr<Operation>,
        binop: BinOp,
        block: Ptr<BasicBlock>,
    ) -> Value {
        let lhs = self.operand(block, self.hir_operand(op, 0), Ty::Value);
        let rhs_hir = self.hir_operand(op, 1);
        let (short_circuit_on, short_value) = match binop {
            BinOp::And => (false, false),
            BinOp::Or => (true, true),
            BinOp::Imply => (false, true),
            _ => {
                let rhs = self.operand(block, rhs_hir, Ty::Value);
                let op = mir::BinaryOp::new(self.ctx, binop, lhs, rhs);
                return self.emit(block, op);
            }
        };
        let if_op = mir::IfOp::new(self.ctx, lhs);
        self.append(block, &if_op);
        let (short_block, rhs_block) = if short_circuit_on {
            (if_op.then_block(self.ctx), if_op.else_block(self.ctx))
        } else {
            (if_op.else_block(self.ctx), if_op.then_block(self.ctx))
        };
        let constant = mir::ConstOp::new(self.ctx, hir::Literal::Bool(short_value));
        let constant = self.emit(short_block, constant);
        let ret = mir::YieldOp::new(self.ctx, constant);
        self.append(short_block, &ret);
        let rhs = self.operand(rhs_block, rhs_hir, Ty::Value);
        let checked = mir::CheckBoolOp::new(self.ctx, rhs);
        let checked = self.emit(rhs_block, checked);
        let ret = mir::YieldOp::new(self.ctx, checked);
        self.append(rhs_block, &ret);
        mir::result_of(self.ctx, &if_op)
    }

    /// The binding `nix_hir.var` `op` refers to.
    fn resolve_var(&self, op: Ptr<Operation>) -> Binding {
        let var = Operation::get_op::<hir::VarOp>(op, self.ctx).expect("a var");
        let name = var.get_attr_var_name(self.ctx).expect("named");
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name.as_str()).copied())
            .expect("nix_hir.var refers to an enclosing binding")
    }
}

/// How `user` uses its operand `index`.
fn operand_use(ctx: &Context, user: Ptr<Operation>, index: usize) -> Use {
    let is = |f: fn(&Context, Ptr<Operation>) -> bool| f(ctx, user);
    if is(|c, o| Operation::get_op::<hir::ApplyOp>(o, c).is_some()) {
        return if index == 0 { Use::Strict } else { Use::Lazy };
    }
    if let Some(binary) = Operation::get_op::<hir::BinaryOp>(user, ctx) {
        let lazy_rhs = matches!(
            binary.get_attr_binop(ctx).map(|b| *b),
            Some(BinOp::And | BinOp::Or | BinOp::Imply)
        );
        return if lazy_rhs && index == 1 {
            Use::Conditional
        } else {
            Use::Strict
        };
    }
    if is(|c, o| Operation::get_op::<hir::DynBindOp>(o, c).is_some()) {
        return if index == 0 { Use::Strict } else { Use::Lazy };
    }
    if is(|c, o| {
        Operation::get_op::<hir::ListOp>(o, c).is_some()
            || Operation::get_op::<hir::BindOp>(o, c).is_some()
            || Operation::get_op::<hir::WithOp>(o, c).is_some()
    }) {
        return Use::Lazy;
    }
    // select, has_attr, unary, interpolate, if, assert, formals, yield.
    Use::Strict
}
