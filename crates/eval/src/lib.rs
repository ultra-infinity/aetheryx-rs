//! The reference interpreter for `nix_mir`.
//!
//! It executes the MIR op by op, calling the [`runtime`] for the semantics of
//! each op, so it is the oracle that compiled code will be checked against.
//! Isolated regions become runtime thunks and closures holding the values of
//! their captures; `if` regions run in place.
//!
//! # Differences from Nix
//! Output and error messages match Nix 2.28 (`samples/expected` is checked
//! byte for byte), except:
//! - A failed `assert` reports `assertion failed`. Nix reprints the condition
//!   (`assertion '(! true)' failed`) and, for `==`, explains the difference.
//! - For "called with unexpected argument", Nix reports the first unexpected
//!   argument in its internal symbol order; this reports the first by name.
//! - A duplicate dynamic attribute is reported without the position of its
//!   first definition.
//! - Not supported yet, each failing with an explicit error: search paths
//!   (`<name>`, `findFile`), `import`, derivations and the store, copying
//!   paths to the store in string interpolation, string contexts, and the
//!   builtins not implemented in `runtime::primops`.
//! - `letrec` groups form reference cycles that are never freed: fine for a
//!   single evaluation; the compiled runtime will need a garbage collector.

use std::collections::HashMap;
use std::rc::Rc;

use ir::ast_to_hir::{LowerOptions, lower_root_with};
use ir::builtins_dialect::RefOp;
use ir::diagnostics::{ErrorKind, ParseErrors, SourceError, SourceFile, span_of};
use ir::hir::{BinOp, InterpKind, Literal, UnOp};
use ir::hir_to_mir::lower_module;
use ir::mir;
use pliron::basic_block::BasicBlock;
use pliron::builtin::op_interfaces::SingleBlockRegionInterface;
use pliron::builtin::ops::{FuncOp, ModuleOp};
use pliron::context::{Context, Ptr};
use pliron::linked_list::ContainsLinkedList;
use pliron::location::Located;
use pliron::operation::Operation;
use pliron::value::Value as MirValue;
use runtime::print::print_strict;
use runtime::{Builtins, Data, Error, Result, Value, ops};

/// Why a file could not be evaluated.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum EvalError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Parse(#[from] ParseErrors),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Source(#[from] SourceError),
}

/// Parses, lowers and evaluates `file`, and prints the result like
/// `nix-instantiate --eval --strict`.
///
/// Nix calls may nest [`runtime::ops::MAX_CALL_DEPTH`] deep, and the
/// interpreter recurses with them: run this on a thread with a large stack
/// (the driver uses 256 MiB).
pub fn evaluate(
    mut ctx: Context,
    file: &SourceFile,
    options: &LowerOptions,
) -> std::result::Result<String, EvalError> {
    let parse = rnix::Root::parse(file.text());
    file.check_parse(parse.errors())?;
    let hir = lower_root_with(&mut ctx, file, &parse.tree(), options)?;
    let module = lower_module(&mut ctx, hir).map_err(|e| {
        let span = span_of(&e.loc).map(|s| s.offset()..s.offset() + s.len());
        file.error_at(ErrorKind::Verify(e.err.to_string()), span)
    })?;
    let to_source_error = |e: Error| file.error_at(ErrorKind::Eval(e.message), e.span);
    let value = eval_module(Rc::new(ctx), module).map_err(to_source_error)?;
    Ok(print_strict(&value).map_err(to_source_error)?)
}

/// Evaluates `@main` of the MIR module `module` to weak head normal form.
pub fn eval_module(ctx: Rc<Context>, module: ModuleOp) -> Result<Value> {
    let main = module
        .get_body(&ctx, 0)
        .deref(&ctx)
        .iter(&ctx)
        .find_map(|op| Operation::get_op::<FuncOp>(op, &ctx))
        .expect("MIR modules contain @main");
    let entry = main.get_entry_block(&ctx);
    let interp = Rc::new(Interp {
        ctx,
        builtins: Builtins::new(),
    });
    let value = interp.run_block(entry, &mut HashMap::new())?;
    value.force()?;
    Ok(value)
}

struct Interp {
    ctx: Rc<Context>,
    builtins: Builtins,
}

type Env = HashMap<MirValue, Value>;

impl Interp {
    /// Runs the ops of `block` and returns the value it yields.
    fn run_block(self: &Rc<Self>, block: Ptr<BasicBlock>, env: &mut Env) -> Result<Value> {
        let ctx = &self.ctx;
        for op in block.deref(ctx).iter(ctx) {
            if Operation::get_op::<mir::YieldOp>(op, ctx).is_some() {
                return Ok(env[&op.deref(ctx).get_operand(0)].clone());
            }
            let span = span_of(&op.deref(ctx).loc()).map(|s| s.offset()..s.offset() + s.len());
            let result = self.run_op(op, env).map_err(|e| e.at(span))?;
            if let Some(value) = result {
                let mir_value = op.deref(ctx).get_result(0);
                env.insert(mir_value, value);
            }
        }
        unreachable!("MIR regions end in nix_mir.yield")
    }

    /// Runs region `region` of the isolated `op`, whose block arguments
    /// receive `args`.
    fn run_region(
        self: &Rc<Self>,
        op: Ptr<Operation>,
        region: usize,
        args: Vec<Value>,
    ) -> Result<Value> {
        let block = op
            .deref(&self.ctx)
            .get_region(region)
            .deref(&self.ctx)
            .get_head()
            .expect("MIR regions have one block");
        let params: Vec<MirValue> = block.deref(&self.ctx).arguments().collect();
        let mut env: Env = params.into_iter().zip(args).collect();
        self.run_block(block, &mut env)
    }

    /// Runs one op; returns its result, if it has one.
    ///
    /// Each op is run by its own small function: interpreting a Nix call goes
    /// through this function more than once, so a large frame here would
    /// limit how deeply Nix code can recurse.
    fn run_op(self: &Rc<Self>, op: Ptr<Operation>, env: &mut Env) -> Result<Option<Value>> {
        let ctx = &self.ctx;
        macro_rules! is {
            ($t:ty) => {
                Operation::get_op::<$t>(op, ctx).is_some()
            };
        }
        let value = if is!(mir::ConstOp) {
            self.run_const(op)
        } else if is!(RefOp) {
            self.run_builtin(op)?
        } else if is!(mir::ForceOp) {
            run_force(ctx, op, env)?
        } else if is!(mir::EvaluatedOp) {
            operand(ctx, op, env, 0)
        } else if is!(mir::CheckBoolOp) {
            run_check_bool(ctx, op, env)?
        } else if is!(mir::ThunkOp) {
            self.run_thunk(op, env)
        } else if is!(mir::LetrecOp) {
            self.run_letrec(op, env);
            return Ok(None);
        } else if is!(mir::LambdaOp) {
            self.run_lambda(op, env)
        } else if is!(mir::CallOp) {
            run_call(ctx, op, env)?
        } else if is!(mir::FormalsOp) {
            run_formals(ctx, op, env)?
        } else if is!(mir::ListOp) {
            Value::list(operands(ctx, op, env))
        } else if is!(mir::AttrsOp) {
            run_attrs(ctx, op, env)
        } else if is!(mir::AttrsInsertOp) {
            run_attrs_insert(ctx, op, env)?
        } else if is!(mir::SelectOp) {
            run_select(ctx, op, env)?
        } else if is!(mir::HasAttrOp) {
            run_has_attr(ctx, op, env)?
        } else if is!(mir::BinaryOp) {
            run_binary(ctx, op, env)?
        } else if is!(mir::UnaryOp) {
            run_unary(ctx, op, env)?
        } else if is!(mir::InterpolateOp) {
            run_interpolate(ctx, op, env)?
        } else if is!(mir::WithLookupOp) {
            run_with_lookup(ctx, op, env)?
        } else if is!(mir::IfOp) {
            self.run_if(op, env)?
        } else if is!(mir::AssertOp) {
            run_assert(ctx, op, env)?;
            return Ok(None);
        } else {
            unreachable!("unexpected op in MIR: {}", Operation::get_opid(op, ctx))
        };
        Ok(Some(value))
    }

    #[inline(never)]
    fn run_const(&self, op: Ptr<Operation>) -> Value {
        let c = Operation::get_op::<mir::ConstOp>(op, &self.ctx).expect("a const");
        constant(&c.get_attr_mir_literal(&self.ctx).expect("has a literal"))
    }

    #[inline(never)]
    fn run_builtin(&self, op: Ptr<Operation>) -> Result<Value> {
        let r = Operation::get_op::<RefOp>(op, &self.ctx).expect("a ref");
        self.builtins
            .get(r.builtin(&self.ctx).expect("verified").name)
    }

    #[inline(never)]
    fn run_thunk(self: &Rc<Self>, op: Ptr<Operation>, env: &Env) -> Value {
        let (this, captures) = (self.clone(), operands(&self.ctx, op, env));
        Value::thunk(move || this.run_region(op, 0, captures))
    }

    #[inline(never)]
    fn run_letrec(self: &Rc<Self>, op: Ptr<Operation>, env: &mut Env) {
        let ctx = &self.ctx;
        let group = Operation::get_op::<mir::LetrecOp>(op, ctx).expect("a letrec");
        let members: Vec<Value> = (0..group.len(ctx)).map(|_| Value::undefined()).collect();
        let captures = operands(ctx, op, env);
        for (i, member) in members.iter().enumerate() {
            let this = self.clone();
            let args: Vec<Value> = members.iter().chain(&captures).cloned().collect();
            member.define(move || this.run_region(op, i, args));
        }
        for (i, member) in members.into_iter().enumerate() {
            env.insert(op.deref(ctx).get_result(i), member);
        }
    }

    #[inline(never)]
    fn run_lambda(self: &Rc<Self>, op: Ptr<Operation>, env: &Env) -> Value {
        let lambda = Operation::get_op::<mir::LambdaOp>(op, &self.ctx).expect("a lambda");
        let (this, captures) = (self.clone(), operands(&self.ctx, op, env));
        Value::lambda(lambda.name(&self.ctx).map(Rc::from), move |arg| {
            let args = std::iter::once(arg)
                .chain(captures.iter().cloned())
                .collect();
            this.run_region(op, 0, args)
        })
    }

    #[inline(never)]
    fn run_if(self: &Rc<Self>, op: Ptr<Operation>, env: &mut Env) -> Result<Value> {
        let ctx = &self.ctx;
        let if_op = Operation::get_op::<mir::IfOp>(op, ctx).expect("an if");
        let branch = if ops::expect_bool(&data(ctx, op, env, 0)?)? {
            if_op.then_block(ctx)
        } else {
            if_op.else_block(ctx)
        };
        self.run_block(branch, env)
    }
}

fn operand(ctx: &Context, op: Ptr<Operation>, env: &Env, i: usize) -> Value {
    env[&op.deref(ctx).get_operand(i)].clone()
}

fn operands(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Vec<Value> {
    op.deref(ctx).operands().map(|v| env[&v].clone()).collect()
}

/// Operand `i`, evaluated.
fn data(ctx: &Context, op: Ptr<Operation>, env: &Env, i: usize) -> Result<Data> {
    operand(ctx, op, env, i).force()
}

#[inline(never)]
fn run_force(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let thunk = operand(ctx, op, env, 0);
    thunk.force()?;
    Ok(thunk)
}

#[inline(never)]
fn run_check_bool(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let value = operand(ctx, op, env, 0);
    ops::expect_bool(&value.force()?)?;
    Ok(value)
}

#[inline(never)]
fn run_call(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let result = ops::call(&data(ctx, op, env, 0)?, operand(ctx, op, env, 1))?;
    result.force()?;
    Ok(result)
}

#[inline(never)]
fn run_formals(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let formals = Operation::get_op::<mir::FormalsOp>(op, ctx).expect("formals");
    let spec = formals.get_attr_mir_formals(ctx).expect("has formals");
    let name = enclosing_lambda_name(ctx, op);
    let arg = operand(ctx, op, env, 0);
    ops::check_formals(&arg.force()?, &spec.formals, spec.ellipsis, name.as_deref())?;
    Ok(arg)
}

#[inline(never)]
fn run_attrs(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Value {
    let attrs = Operation::get_op::<mir::AttrsOp>(op, ctx).expect("attrs");
    let names = attrs.get_attr_mir_attr_names(ctx).expect("has names");
    let values = operands(ctx, op, env);
    let set = names
        .0
        .iter()
        .map(|n| Rc::from(n.as_str()))
        .zip(values)
        .collect();
    Value::attrs(set)
}

#[inline(never)]
fn run_attrs_insert(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let (set, key) = (data(ctx, op, env, 0)?, data(ctx, op, env, 1)?);
    Ok(Value::ready(ops::attrs_insert(
        &set,
        &key,
        operand(ctx, op, env, 2),
    )?))
}

#[inline(never)]
fn run_select(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    ops::select(&data(ctx, op, env, 0)?, &data(ctx, op, env, 1)?)
}

#[inline(never)]
fn run_has_attr(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let has = ops::has_attr(&data(ctx, op, env, 0)?, &data(ctx, op, env, 1)?)?;
    Ok(Value::bool(has))
}

#[inline(never)]
fn run_binary(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let binary = Operation::get_op::<mir::BinaryOp>(op, ctx).expect("a binary op");
    let binop = *binary.get_attr_mir_binop(ctx).expect("has an operator");
    let (a, b) = (data(ctx, op, env, 0)?, data(ctx, op, env, 1)?);
    Ok(Value::ready(binary_op(binop, &a, &b)?))
}

#[inline(never)]
fn run_unary(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let unary = Operation::get_op::<mir::UnaryOp>(op, ctx).expect("a unary op");
    let unop = *unary.get_attr_mir_unop(ctx).expect("has an operator");
    let operand = data(ctx, op, env, 0)?;
    Ok(Value::ready(match unop {
        UnOp::Not => ops::not(&operand)?,
        UnOp::Neg => ops::neg(&operand)?,
    }))
}

#[inline(never)]
fn run_interpolate(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let interp = Operation::get_op::<mir::InterpolateOp>(op, ctx).expect("an interpolation");
    let kind = *interp.get_attr_mir_interp_kind(ctx).expect("has a kind");
    let parts: Vec<Data> = operands(ctx, op, env)
        .iter()
        .map(Value::force)
        .collect::<Result<_>>()?;
    Ok(Value::ready(match kind {
        InterpKind::String => ops::interpolate_string(&parts)?,
        InterpKind::Path => ops::interpolate_path(&parts)?,
    }))
}

#[inline(never)]
fn run_with_lookup(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<Value> {
    let lookup = Operation::get_op::<mir::WithLookupOp>(op, ctx).expect("a lookup");
    let name = lookup.get_attr_mir_with_name(ctx).expect("named");
    ops::with_lookup(name.as_str(), &operands(ctx, op, env))
}

#[inline(never)]
fn run_assert(ctx: &Context, op: Ptr<Operation>, env: &Env) -> Result<()> {
    if ops::expect_bool(&data(ctx, op, env, 0)?)? {
        Ok(())
    } else {
        Err(Error::new("assertion failed"))
    }
}

fn constant(literal: &Literal) -> Value {
    match literal {
        Literal::Int(i) => Value::int(*i),
        Literal::Float(f) => Value::float(f.0),
        // URIs are strings in Nix.
        Literal::String(s) | Literal::Uri(s) => Value::string(s.as_str()),
        Literal::Path(p) => Value::path(p.as_str()),
        Literal::Bool(b) => Value::bool(*b),
        Literal::Null => Value::null(),
    }
}

/// The strict binary operators. Nix evaluates `a > b` as `b < a`,
/// `a <= b` as `!(b < a)` and `a >= b` as `!(a < b)`, which also decides the
/// operand order in error messages.
fn binary_op(op: BinOp, a: &Data, b: &Data) -> Result<Data> {
    Ok(match op {
        BinOp::Add => ops::add(a, b)?,
        BinOp::Sub => ops::sub(a, b)?,
        BinOp::Mul => ops::mul(a, b)?,
        BinOp::Div => ops::div(a, b)?,
        BinOp::ConcatLists => ops::concat_lists(a, b)?,
        BinOp::MergeAttrs => ops::update(a, b)?,
        BinOp::Eq => Data::Bool(ops::equal(a, b)?),
        BinOp::Ne => Data::Bool(!ops::equal(a, b)?),
        BinOp::Lt => Data::Bool(ops::less_than(a, b)?),
        BinOp::Gt => Data::Bool(ops::less_than(b, a)?),
        BinOp::Le => Data::Bool(!ops::less_than(b, a)?),
        BinOp::Ge => Data::Bool(!ops::less_than(a, b)?),
        BinOp::And | BinOp::Or | BinOp::Imply => {
            unreachable!("the MIR verifier rejects lazy operators")
        }
    })
}

/// The name of the lambda whose body contains `op`, if it has one.
fn enclosing_lambda_name(ctx: &Context, op: Ptr<Operation>) -> Option<String> {
    let mut current = op.deref(ctx).get_parent_op(ctx);
    while let Some(parent) = current {
        if let Some(lambda) = Operation::get_op::<mir::LambdaOp>(parent, ctx) {
            return lambda.name(ctx);
        }
        current = parent.deref(ctx).get_parent_op(ctx);
    }
    None
}

#[cfg(test)]
mod tests;
