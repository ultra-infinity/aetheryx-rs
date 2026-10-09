//! Lambdas and their parameters.

use std::collections::HashMap;

use pliron::basic_block::BasicBlock;
use pliron::context::Ptr;
use pliron::op::Op;
use pliron::value::Value;
use rnix::TextRange;
use rnix::ast;
use rowan::ast::AstNode;

use super::{Binding, Lowerer, Result, ident_name};
use crate::diagnostics::{Diag, ErrorKind};
use crate::hir::{
    BindOp, FormalsOp, HasAttrOp, LambdaOp, LetInOp, SelectOp, add_region_block,
    add_region_block_with_args, result_of,
};

impl Lowerer<'_> {
    // ─── Lambdas ────────────────────────────────────────────────────────────

    pub(super) fn lower_lambda(
        &mut self,
        block: Ptr<BasicBlock>,
        lambda: &ast::Lambda,
    ) -> Result<Value> {
        let param = self.child(lambda.param(), lambda.syntax(), "lambda parameter")?;
        let body = self.child(lambda.body(), lambda.syntax(), "lambda body")?;

        let op = LambdaOp::new(self.ctx);
        self.append(block, &op);
        let body_block = add_region_block_with_args(self.ctx, op.get_operation(), 0, 1);
        let arg = body_block.deref(self.ctx).get_argument(0);

        let depth = self.scopes.len();
        self.scopes.push(HashMap::new());
        let res = self
            .at(param.syntax().text_range(), |this| {
                this.bind_param(body_block, arg, &param)
            })
            .and_then(|defaults| match defaults {
                None => self.lower_expr(body_block, &body),
                // The body goes inside the `nix_hir.let_in` binding the defaulted formals.
                Some((let_op, let_body)) => {
                    let v = self.lower_expr(let_body, &body)?;
                    self.emit_yield(let_body, v);
                    Ok(result_of(self.ctx, &let_op))
                }
            });
        // `bind_param` may have pushed a scope for the defaulted formals.
        self.scopes.truncate(depth);
        let v = res?;
        self.emit_yield(body_block, v);
        Ok(result_of(self.ctx, &op))
    }

    /// Brings the lambda parameter(s) into scope.
    ///
    /// If there are formals with defaults, they are bound in a new `nix_hir.let_in`
    /// appended to `block`, whose scope is pushed. Returns that op and the
    /// block the lambda body must be lowered into.
    fn bind_param(
        &mut self,
        block: Ptr<BasicBlock>,
        arg: Value,
        param: &ast::Param,
    ) -> Result<Option<(LetInOp, Ptr<BasicBlock>)>> {
        let pattern = match param {
            ast::Param::IdentParam(p) => {
                let id = self.child(p.ident(), p.syntax(), "parameter name")?;
                let name = ident_name(&id);
                self.name_value(arg, &name);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(name, Binding::Ssa(arg));
                return Ok(None);
            }
            ast::Param::Pattern(p) => p,
        };

        let bind = pattern.pat_bind().and_then(|b| b.ident());
        let mut formals = Vec::new();
        for entry in pattern.pat_entries() {
            let id = self.child(entry.ident(), entry.syntax(), "formal name")?;
            formals.push((
                ident_name(&id),
                entry.default(),
                entry.syntax().text_range(),
            ));
        }

        // rnix does not report these; Nix rejects `{ a, a }:` and `a@{ a }:`.
        let mut seen: Vec<(String, TextRange)> = Vec::new();
        let names_with_ranges = bind
            .iter()
            .map(|id| (ident_name(id), id.syntax().text_range()))
            .chain(formals.iter().map(|(n, _, r)| (n.clone(), *r)));
        for (name, range) in names_with_ranges {
            if let Some((_, first)) = seen.iter().find(|(n, _)| *n == name) {
                return Err(Diag::new(ErrorKind::DuplicateFormal(name), range)
                    .with_label(*first, "first defined here"));
            }
            seen.push((name, range));
        }

        match bind {
            Some(id) => {
                let name = ident_name(&id);
                self.name_value(arg, &name);
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(name, Binding::Ssa(arg));
            }
            None => self.name_value(arg, "arg"),
        }
        let names = formals.iter().map(|(n, _, _)| n.clone()).collect();
        let checked = FormalsOp::new(self.ctx, arg, names, pattern.ellipsis_token().is_some());
        let checked = self.emit(block, checked);

        let (plain, defaulted): (Vec<_>, Vec<_>) =
            formals.into_iter().partition(|(_, d, _)| d.is_none());
        for (name, _, range) in plain {
            let v = self.at(range, |this| {
                let key = this.emit_str(block, &name);
                let op = SelectOp::new(this.ctx, checked, key);
                this.emit(block, op)
            });
            self.scopes
                .last_mut()
                .unwrap()
                .insert(name, Binding::Ssa(v));
        }
        if defaulted.is_empty() {
            return Ok(None);
        }

        // In Nix, every default can refer to every formal (even later ones, or
        // itself), so the defaulted formals are bound recursively:
        //   { a ? b, b ? 1 }: body  ==>  let a = arg.a or b; b = arg.b or 1; in body
        // where `arg.x or d` is spelled with `nix_hir.has_attr` / `nix_hir.if`.
        let let_op = LetInOp::new(self.ctx);
        self.append(block, &let_op);
        let bindings = add_region_block(self.ctx, let_op.get_operation(), 0);
        let let_body = add_region_block(self.ctx, let_op.get_operation(), 1);
        self.scopes.push(
            defaulted
                .iter()
                .map(|(name, _, _)| (name.clone(), Binding::Rec))
                .collect(),
        );
        for (name, default, range) in defaulted {
            let default = default.unwrap();
            self.at(range, |this| -> Result<()> {
                let key = this.emit_str(bindings, &name);
                let has = HasAttrOp::new(this.ctx, checked, key);
                let has = this.emit(bindings, has);
                let v = this.emit_if(
                    bindings,
                    has,
                    |this, b| {
                        let op = SelectOp::new(this.ctx, checked, key);
                        Ok(this.emit(b, op))
                    },
                    |this, b| this.lower_expr(b, &default),
                )?;
                let bind = BindOp::new(this.ctx, &name, v);
                this.append(bindings, &bind);
                Ok(())
            })?;
        }
        Ok(Some((let_op, let_body)))
    }
}
