//! [`ToMIRDialect`] for the expression ops of `nix_hir`, `nix_builtins.ref`
//! and `builtin.func`. The binding ops are in [`super::bindings`]; the scheme
//! they all follow is described in [`super::convert`].

use pliron::builtin::attributes::TypeAttr;
use pliron::builtin::ops::FuncOp;
use pliron::derive::op_interface_impl;
use pliron::irbuild::rewriter::Rewriter;
use pliron::linked_list::ContainsLinkedList;
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::result::Result;
use pliron::r#type::{Typed, type_cast};
use pliron::value::Value;

use super::convert::{Builder, ConversionState};
use super::{Lowered, ToMIRDialect, ToMIRType, is_hir_type};
use crate::builtins_dialect::RefOp;
use crate::hir::{self, BinOp};
use crate::mir::{self, Ty};

// ─── Trivial and eager ops ──────────────────────────────────────────────────

#[op_interface_impl]
impl ToMIRDialect for hir::ConstOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let literal = self
            .get_attr_literal(b.ctx)
            .expect("consts have a literal")
            .clone();
        let constant = mir::ConstOp::new(b.ctx, literal);
        let value = b.value(constant);
        Ok(Lowered::Value(b.evaluated(value)))
    }
}

/// Shared by both IRs: only a reference producing a `nix_hir.value` is
/// converted.
#[op_interface_impl]
impl ToMIRDialect for RefOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let ty = self
            .get_operation()
            .deref(b.ctx)
            .get_result(0)
            .get_type(b.ctx);
        if !is_hir_type(b.ctx, ty) {
            return Ok(Lowered::InPlace);
        }
        let name = self.builtin(b.ctx).expect("verified").name;
        let value_ty = Ty::Value.handle(b.ctx);
        let reference = RefOp::new(b.ctx, name, value_ty);
        let value = b.value(reference);
        Ok(Lowered::Value(b.evaluated(value)))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::LambdaOp {
    fn to_mir(&self, b: &mut Builder, state: &mut ConversionState) -> Result<Lowered> {
        let hir_body = b.region(0);
        let hir_arg = hir_body.deref(b.ctx).get_argument(0);
        let lambda = mir::LambdaOp::new(b.ctx);
        if let Some(name) = state.scopes.lambda_names.get(&self.get_operation()) {
            lambda.set_name(b.ctx, name);
        }
        // The pattern is checked by the `nix_hir.formals` on the argument.
        let pattern = hir_body
            .deref(b.ctx)
            .iter(b.ctx)
            .filter_map(|op| Operation::get_op::<hir::FormalsOp>(op, b.ctx))
            .find(|formals| formals.get_operation().deref(b.ctx).get_operand(0) == hir_arg)
            .map(|formals| {
                formals
                    .get_attr_formals(b.ctx)
                    .expect("formals have a pattern")
                    .clone()
            });
        if let Some(pattern) = pattern {
            lambda.set_formals(b.ctx, pattern);
        }
        b.insert(&lambda);
        let arg = lambda.argument(b.ctx);
        b.rewriter.replace_value_uses_with(b.ctx, hir_arg, arg);
        let body = lambda.body(b.ctx);
        b.at_end_of(body, |b| b.move_ops(hir_body));
        let value = mir::result_of(b.ctx, &lambda);
        Ok(Lowered::Value(b.evaluated(value)))
    }
}

/// Eager: Nix checks the pattern when the function is called.
#[op_interface_impl]
impl ToMIRDialect for hir::FormalsOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let spec = self
            .get_attr_formals(b.ctx)
            .expect("formals have a pattern")
            .clone();
        let arg = b.force(b.operand(0));
        let checked = mir::FormalsOp::new(b.ctx, arg, spec);
        let value = b.value(checked);
        Ok(Lowered::Value(b.evaluated(value)))
    }
}

// ─── Suspended ops ──────────────────────────────────────────────────────────

#[op_interface_impl]
impl ToMIRDialect for hir::ApplyOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let (func, arg) = (b.operand(0), b.operand(1));
        Ok(Lowered::Value(b.thunk_of(|b| {
            let func = b.force(func);
            let call = mir::CallOp::new(b.ctx, func, arg);
            b.value(call)
        })))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::SelectOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let (set, key) = (b.operand(0), b.operand(1));
        Ok(Lowered::Value(b.thunk_of(|b| {
            let set = b.force(set);
            let key = b.force(key);
            let select = mir::SelectOp::new(b.ctx, set, key);
            let attr = b.value(select);
            b.force(attr)
        })))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::HasAttrOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let (set, key) = (b.operand(0), b.operand(1));
        Ok(Lowered::Value(b.thunk_of(|b| {
            let set = b.force(set);
            let key = b.force(key);
            let has_attr = mir::HasAttrOp::new(b.ctx, set, key);
            b.value(has_attr)
        })))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::UnaryOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let unop = *self.get_attr_unop(b.ctx).expect("has an operator");
        let arg = b.operand(0);
        Ok(Lowered::Value(b.thunk_of(|b| {
            let arg = b.force(arg);
            let unary = mir::UnaryOp::new(b.ctx, unop, arg);
            b.value(unary)
        })))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::BinaryOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let binop = *self.get_attr_binop(b.ctx).expect("has an operator");
        let (lhs, rhs) = (b.operand(0), b.operand(1));
        Ok(Lowered::Value(
            b.thunk_of(|b| binary_value(b, binop, lhs, rhs)),
        ))
    }
}

/// Strict operators map to `nix_mir.binary`; `&&`, `||` and `->` become
/// `nix_mir.if`, evaluating (and type-checking) the right side only on the
/// branch that needs it.
fn binary_value(b: &mut Builder, binop: BinOp, lhs: Value, rhs: Value) -> Value {
    let lhs = b.force(lhs);
    let (short_circuit_on, short_value) = match binop {
        BinOp::And => (false, false),
        BinOp::Or => (true, true),
        BinOp::Imply => (false, true),
        _ => {
            let rhs = b.force(rhs);
            let binary = mir::BinaryOp::new(b.ctx, binop, lhs, rhs);
            return b.value(binary);
        }
    };
    let if_op = mir::IfOp::new(b.ctx, lhs);
    b.insert(&if_op);
    let (short_block, rhs_block) = if short_circuit_on {
        (if_op.then_block(b.ctx), if_op.else_block(b.ctx))
    } else {
        (if_op.else_block(b.ctx), if_op.then_block(b.ctx))
    };
    b.at_end_of(short_block, |b| {
        let constant = mir::ConstOp::new(b.ctx, hir::Literal::Bool(short_value));
        let constant = b.value(constant);
        b.yield_(constant);
    });
    b.at_end_of(rhs_block, |b| {
        let rhs = b.force(rhs);
        let checked = mir::CheckBoolOp::new(b.ctx, rhs);
        let checked = b.value(checked);
        b.yield_(checked);
    });
    mir::result_of(b.ctx, &if_op)
}

#[op_interface_impl]
impl ToMIRDialect for hir::InterpolateOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let kind = *self.get_attr_interp_kind(b.ctx).expect("has a kind");
        let parts = b.operands();
        Ok(Lowered::Value(b.thunk_of(|b| {
            let parts = parts.into_iter().map(|part| b.force(part)).collect();
            let interp = mir::InterpolateOp::new(b.ctx, kind, parts);
            b.value(interp)
        })))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::ListOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let items = b.operands();
        Ok(Lowered::Value(b.thunk_of(|b| {
            let list = mir::ListOp::new(b.ctx, items);
            b.value(list)
        })))
    }
}

/// Searches the namespaces of the enclosing `with` ops, innermost first.
#[op_interface_impl]
impl ToMIRDialect for hir::LookupOp {
    fn to_mir(&self, b: &mut Builder, state: &mut ConversionState) -> Result<Lowered> {
        let name = self
            .get_attr_lookup_name(b.ctx)
            .expect("lookups are named")
            .as_str()
            .to_string();
        // The enclosing `with` ops were converted before the ops they
        // contain.
        let namespaces = state.scopes.withs[&self.get_operation()]
            .iter()
            .map(|with| state.namespaces[with])
            .collect();
        Ok(Lowered::Value(b.thunk_of(|b| {
            let lookup = mir::WithLookupOp::new(b.ctx, &name, namespaces);
            let found = b.value(lookup);
            b.force(found)
        })))
    }
}

// ─── Ops with regions ───────────────────────────────────────────────────────

#[op_interface_impl]
impl ToMIRDialect for hir::IfOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let (cond, then_hir, else_hir) = (b.operand(0), b.region(0), b.region(1));
        Ok(Lowered::Value(b.thunk_of(|b| {
            let cond = b.force(cond);
            let if_op = mir::IfOp::new(b.ctx, cond);
            b.insert(&if_op);
            let then_block = if_op.then_block(b.ctx);
            b.at_end_of(then_block, |b| b.move_ops(then_hir));
            let else_block = if_op.else_block(b.ctx);
            b.at_end_of(else_block, |b| b.move_ops(else_hir));
            mir::result_of(b.ctx, &if_op)
        })))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::AssertOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let (cond, body) = (b.operand(0), b.region(0));
        Ok(Lowered::Value(b.thunk(|b| {
            let cond = b.force(cond);
            let assert = mir::AssertOp::new(b.ctx, cond);
            b.insert(&assert);
            // The body's `nix_hir.yield` becomes the thunk's terminator.
            b.move_ops(body);
        })))
    }
}

/// Records the namespace for the lookups in the body.
#[op_interface_impl]
impl ToMIRDialect for hir::WithOp {
    fn to_mir(&self, b: &mut Builder, state: &mut ConversionState) -> Result<Lowered> {
        state.namespaces.insert(self.get_operation(), b.operand(0));
        let body = b.region(0);
        Ok(Lowered::Value(b.thunk(|b| b.move_ops(body))))
    }
}

#[op_interface_impl]
impl ToMIRDialect for hir::YieldOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let value = b.force(b.operand(0));
        b.yield_(value);
        Ok(Lowered::Erased)
    }
}

/// Converts the signature (see the [`ToMIRType`] of `builtin.function`).
#[op_interface_impl]
impl ToMIRDialect for FuncOp {
    fn to_mir(&self, b: &mut Builder, _: &mut ConversionState) -> Result<Lowered> {
        let ty = self.get_type(b.ctx);
        let converted = type_cast::<dyn ToMIRType>(&*ty.deref(b.ctx))
            .expect("function types convert to MIR")
            .convert(b.ctx)?;
        if converted != ty {
            self.set_attr_builtin_func_type(b.ctx, TypeAttr::new(converted));
            b.rewriter.mark_modified();
        }
        Ok(Lowered::InPlace)
    }
}
