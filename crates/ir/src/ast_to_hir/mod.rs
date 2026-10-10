//! Lowering from the rnix AST to `nix_hir`.
//!
//! The entry point is [`lower_root`], which wraps the lowered expression of a
//! whole file in a `builtin.module` holding `builtin.func @main` of type
//! `() -> (nix_hir.value)`.
//!
//! # Name resolution
//! Names are resolved at lowering time where Nix semantics allow it:
//! - Lambda parameters and pattern formals without a default are SSA values
//!   (block arguments or values derived from them) and are referenced directly.
//! - Formals with a default (`{ a ? b, b ? 1 }:`) form a recursive scope: any
//!   default may refer to any formal. They are bound in a `nix_hir.let_in` that
//!   wraps the lambda body, and referenced with `nix_hir.var` like `let` names.
//! - Names bound by `let` and `rec { }` are recursive, so they cannot be plain
//!   SSA values. References to them lower to `nix_hir.var "name"`, which
//!   resolves to the innermost enclosing `nix_hir.let_in` /
//!   `nix_hir.rec_attr_set` binding it.
//! - Global builtins (`map`, `__add`, `true`, ...; see the [`builtins`]
//!   registry) lower to `nix_builtins.ref "name"` (`true`/`false`/`null` to
//!   `nix_hir.const`). They live in the outermost lexical scope, so they take
//!   precedence over `with`: `with { map = 1; }; map` is the builtin.
//! - Any other name lowers to `nix_hir.lookup "name"`, which resolves through
//!   the enclosing `nix_hir.with` scopes at runtime. Outside every `with` such
//!   a name is an "undefined variable" error, as in Nix.
//!
//! # Desugaring
//! - Attribute paths in bindings (`a.b = 1; a.c = 2;`) are merged into nested
//!   attribute sets, as Nix does (see `ast_to_hir/bindings.rs`).
//! - `e.a.b` becomes a chain of `nix_hir.select`s; `e ? a.b` and `e.a.b or d`
//!   become `nix_hir.has_attr` checks guarded by `nix_hir.if`.
//! - `a |> f` and `f <| a` become `nix_hir.apply`.
//! - `<p>` becomes `__findFile __nixPath "p"`.
//! - `let { ...; body = e; }` becomes `(rec { ...; body = e; }).body`.
//!
//! # Literals
//! Number literals are validated exactly as the Nix lexer does it (see
//! `ast_to_hir/literals.rs`); path literals are resolved as the Nix parser
//! does it (see [`crate::paths`]).
//!
//! # Locations
//! Every op is located at the source range of the syntax it was lowered from
//! (see [`crate::diagnostics`]). Ops introduced by desugaring take the range of
//! the construct being desugared; `nix_hir.bind` takes the range of its
//! binding.

mod bindings;
mod lambda;
mod literals;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use pliron::basic_block::BasicBlock;
use pliron::builtin::op_interfaces::SingleBlockRegionInterface;
use pliron::builtin::ops::{FuncOp, ModuleOp};
use pliron::builtin::types::FunctionType;
use pliron::context::{Context, Ptr};
use pliron::identifier::Identifier;
use pliron::location::Located;
use pliron::op::{Op, verify_op};
use pliron::value::Value;
use rnix::TextRange;
use rnix::ast::{self, InterpolPart};
use rowan::ast::AstNode;

use self::bindings::{Group, Key, key_of};
use crate::builtins_dialect::RefOp;
use crate::diagnostics::{Diag, ErrorKind, SourceError, SourceFile};
use crate::hir::{
    ApplyOp, AssertOp, AttrSetOp, BinOp, BinaryOp, BindOp, ConstOp, HasAttrOp, IfOp, InterpKind,
    InterpolateOp, ListOp, Literal, LookupOp, NixFloat, SelectOp, UnOp, UnaryOp, ValueType, VarOp,
    WithOp, YieldOp, add_region_block, result_of,
};

type Result<T> = std::result::Result<T, Diag>;

/// Inputs to lowering that come from the environment rather than the source.
#[derive(Clone, Debug, Default)]
pub struct LowerOptions {
    /// The directory `~/` path literals resolve against.
    pub home_dir: Option<String>,
    /// Nix's `pure-eval`: the impure builtins (`currentSystem`,
    /// `currentTime`) do not exist.
    pub pure_eval: bool,
}

impl LowerOptions {
    /// Reads the options from the process environment, as Nix does.
    pub fn from_env() -> Self {
        LowerOptions {
            home_dir: std::env::home_dir().and_then(|p| p.to_str().map(str::to_owned)),
            pure_eval: false,
        }
    }
}

/// Lowers a parsed Nix file into a verified module containing `@main`, using
/// [`LowerOptions::from_env`].
///
/// `root` must have been parsed from `file`'s text.
pub fn lower_root(
    ctx: &mut Context,
    file: &SourceFile,
    root: &ast::Root,
) -> std::result::Result<ModuleOp, SourceError> {
    lower_root_with(ctx, file, root, &LowerOptions::from_env())
}

/// Like [`lower_root`], with explicit [`LowerOptions`].
pub fn lower_root_with(
    ctx: &mut Context,
    file: &SourceFile,
    root: &ast::Root,
    options: &LowerOptions,
) -> std::result::Result<ModuleOp, SourceError> {
    let module_name = file
        .path()
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "nix".to_string());
    let module = ModuleOp::new(ctx, ident(&module_name));
    let value_ty = ValueType::get(ctx).into();
    let func_ty = FunctionType::get(ctx, vec![], vec![value_ty]);
    let func = FuncOp::new(ctx, ident("main"), func_ty);
    module.append_operation(ctx, func.get_operation(), 0);
    let whole_file = file.location(file.full_range());
    module
        .get_operation()
        .deref_mut(ctx)
        .set_loc(whole_file.clone());
    func.get_operation().deref_mut(ctx).set_loc(whole_file);

    let entry = func.get_entry_block(ctx);
    let mut lowerer = Lowerer {
        ctx,
        file,
        options,
        with_depth: 0,
        cur: root.syntax().text_range(),
        scopes: Vec::new(),
    };
    let lowered = lowerer
        .child(root.expr(), root.syntax(), "expression")
        .and_then(|expr| lowerer.lower_expr(entry, &expr));
    let value = lowered.map_err(|d| file.error(d))?;
    lowerer.emit_yield(entry, value);

    verify_op(&module, ctx).map_err(|e| {
        file.error(Diag::at_location(
            ErrorKind::Verify(e.err.to_string()),
            &e.loc,
        ))
    })?;
    Ok(module)
}

/// How a name in scope is bound.
#[derive(Clone, Copy)]
enum Binding {
    /// Directly available as an SSA value (lambda parameters / formals).
    Ssa(Value),
    /// Bound recursively by an enclosing `let` / `rec { }`: use `nix_hir.var`.
    Rec,
}

fn ident_name(id: &ast::Ident) -> String {
    id.syntax().text().to_string()
}

fn ident(name: &str) -> Identifier {
    Identifier::try_from(sanitize_identifier(name)).expect("sanitized identifiers are valid")
}

/// Maps a Nix name onto pliron's `[a-zA-Z_][a-zA-Z0-9_]*`.
fn sanitize_identifier(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        s.insert(0, '_');
    }
    s
}

/// The state of lowering one file.
struct Lowerer<'a> {
    ctx: &'a mut Context,
    file: &'a SourceFile,
    options: &'a LowerOptions,
    /// Number of `with` bodies enclosing the code being lowered.
    with_depth: usize,
    /// Source range that newly emitted ops are located at.
    cur: TextRange,
    /// Innermost scope last.
    scopes: Vec<HashMap<String, Binding>>,
}

impl Lowerer<'_> {
    // ─── Emission helpers ───────────────────────────────────────────────────

    /// Runs `f` with emitted ops located at `range`.
    fn at<T>(&mut self, range: TextRange, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.cur, range);
        let res = f(self);
        self.cur = saved;
        res
    }

    fn append(&mut self, block: Ptr<BasicBlock>, op: &impl Op) {
        let loc = self.file.location(self.cur);
        let op = op.get_operation();
        op.deref_mut(self.ctx).set_loc(loc);
        op.insert_at_back(block, self.ctx);
    }

    /// Appends a value-producing op to `block` and returns its result.
    fn emit(&mut self, block: Ptr<BasicBlock>, op: impl Op) -> Value {
        self.append(block, &op);
        result_of(self.ctx, &op)
    }

    fn emit_yield(&mut self, block: Ptr<BasicBlock>, value: Value) {
        let op = YieldOp::new(self.ctx, value);
        self.append(block, &op);
    }

    fn emit_const(&mut self, block: Ptr<BasicBlock>, literal: Literal) -> Value {
        let op = ConstOp::new(self.ctx, literal);
        self.emit(block, op)
    }

    fn emit_str(&mut self, block: Ptr<BasicBlock>, s: &str) -> Value {
        self.emit_const(block, Literal::String(s.to_string()))
    }

    /// Emits `nix_hir.if cond` and fills its branches with `then_` and `else_`.
    fn emit_if(
        &mut self,
        block: Ptr<BasicBlock>,
        cond: Value,
        then_: impl FnOnce(&mut Self, Ptr<BasicBlock>) -> Result<Value>,
        else_: impl FnOnce(&mut Self, Ptr<BasicBlock>) -> Result<Value>,
    ) -> Result<Value> {
        let if_op = IfOp::new(self.ctx, cond);
        self.append(block, &if_op);
        let then_block = add_region_block(self.ctx, if_op.get_operation(), 0);
        let v = then_(self, then_block)?;
        self.emit_yield(then_block, v);
        let else_block = add_region_block(self.ctx, if_op.get_operation(), 1);
        let v = else_(self, else_block)?;
        self.emit_yield(else_block, v);
        Ok(result_of(self.ctx, &if_op))
    }

    fn child<T>(
        &self,
        child: Option<T>,
        parent: &rnix::SyntaxNode,
        what: &'static str,
    ) -> Result<T> {
        child.ok_or_else(|| Diag::new(ErrorKind::Missing(what), parent.text_range()))
    }

    // ─── Scopes ─────────────────────────────────────────────────────────────

    fn resolve(&self, name: &str) -> Option<Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    /// Resolves an identifier in Nix's order: lexical scopes, then the global
    /// builtins (which, being lexical too, take precedence over `with`), then
    /// the enclosing `with` scopes. Without an enclosing `with`, an unbound
    /// name is an error, as in Nix.
    fn lower_ident(&mut self, block: Ptr<BasicBlock>, name: &str) -> Result<Value> {
        match self.resolve(name) {
            Some(Binding::Ssa(v)) => return Ok(v),
            Some(Binding::Rec) => {
                let op = VarOp::new(self.ctx, name);
                return Ok(self.emit(block, op));
            }
            None => {}
        }
        if let Some(builtin) = builtins::lookup_global(name)
            && (builtin.in_pure_eval || !self.options.pure_eval)
        {
            let lit = match builtin.name {
                "true" => Some(Literal::Bool(true)),
                "false" => Some(Literal::Bool(false)),
                "null" => Some(Literal::Null),
                _ => None,
            };
            return Ok(match lit {
                Some(lit) => self.emit_const(block, lit),
                None => {
                    let ty = ValueType::get(self.ctx).into();
                    let op = RefOp::new(self.ctx, builtin.name, ty);
                    self.emit(block, op)
                }
            });
        }
        if self.with_depth > 0 {
            let op = LookupOp::new(self.ctx, name);
            return Ok(self.emit(block, op));
        }
        Err(Diag::new(
            ErrorKind::UndefinedVariable(name.to_string()),
            self.cur,
        ))
    }

    /// Gives a block argument a readable name when the Nix name allows it.
    fn name_value(&mut self, value: Value, name: &str) {
        if let Ok(id) = Identifier::try_from(sanitize_identifier(name)) {
            value.set_name(self.ctx, Some(id));
        }
    }

    // ─── Expressions ────────────────────────────────────────────────────────

    /// Lowers `expr` into `block`, locating the emitted ops at `expr`.
    ///
    /// Fails like Nix, with "stack overflow", when the expression nests too
    /// deeply for the remaining stack.
    fn lower_expr(&mut self, block: Ptr<BasicBlock>, expr: &ast::Expr) -> Result<Value> {
        if crate::stack::is_low() {
            let range = expr.syntax().text_range();
            return Err(Diag::new(ErrorKind::StackOverflow, range));
        }
        self.at(expr.syntax().text_range(), |this| {
            this.lower_expr_here(block, expr)
        })
    }

    /// [`Self::lower_expr`] without changing the current location.
    fn lower_expr_here(&mut self, block: Ptr<BasicBlock>, expr: &ast::Expr) -> Result<Value> {
        use ast::Expr;
        match expr {
            Expr::Literal(lit) => {
                let text = lit.syntax().text().to_string();
                let literal = match lit.kind() {
                    ast::LiteralKind::Integer(_) => literals::parse_int(&text)
                        .map(Literal::Int)
                        .ok_or(ErrorKind::InvalidInteger(text)),
                    ast::LiteralKind::Float(_) => literals::parse_float(&text)
                        .map(|f| Literal::Float(NixFloat(f)))
                        .ok_or(ErrorKind::InvalidFloat(text)),
                    ast::LiteralKind::Uri(_) => Ok(Literal::Uri(text)),
                }
                .map_err(|kind| Diag::new(kind, self.cur))?;
                Ok(self.emit_const(block, literal))
            }
            Expr::Str(s) => self.lower_str(block, s),
            Expr::PathAbs(_) | Expr::PathRel(_) | Expr::PathHome(_) | Expr::PathSearch(_) => {
                let path = self.child(
                    ast::Path::cast(expr.syntax().clone()),
                    expr.syntax(),
                    "path",
                )?;
                self.lower_path(block, &path)
            }
            Expr::Ident(id) => self.lower_ident(block, &ident_name(id)),
            Expr::Paren(p) => {
                let inner = self.child(p.expr(), p.syntax(), "parenthesized expression")?;
                self.lower_expr(block, &inner)
            }
            Expr::Root(r) => {
                let inner = self.child(r.expr(), r.syntax(), "expression")?;
                self.lower_expr(block, &inner)
            }
            Expr::List(list) => {
                let mut elems = Vec::new();
                for item in list.items() {
                    elems.push(self.lower_expr(block, &item)?);
                }
                let op = ListOp::new(self.ctx, elems);
                Ok(self.emit(block, op))
            }
            Expr::AttrSet(set) => {
                let rec = set.rec_token().is_some();
                let group = Group::from_entries(set, rec, Vec::new(), self.file)?;
                self.lower_group(block, group)
            }
            Expr::LetIn(let_in) => self.lower_let_in(block, let_in),
            Expr::LegacyLet(legacy) => {
                let group = Group::from_entries(legacy, true, Vec::new(), self.file)?;
                let set = self.lower_rec_attr_set(block, group)?;
                let key = self.emit_str(block, "body");
                let op = SelectOp::new(self.ctx, set, key);
                Ok(self.emit(block, op))
            }
            Expr::Lambda(lambda) => self.lower_lambda(block, lambda),
            Expr::Apply(apply) => {
                let f = self.child(apply.lambda(), apply.syntax(), "function")?;
                let x = self.child(apply.argument(), apply.syntax(), "argument")?;
                let f = self.lower_expr(block, &f)?;
                let x = self.lower_expr(block, &x)?;
                let op = ApplyOp::new(self.ctx, f, x);
                Ok(self.emit(block, op))
            }
            Expr::IfElse(ite) => {
                let cond = self.child(ite.condition(), ite.syntax(), "condition")?;
                let then_ = self.child(ite.body(), ite.syntax(), "then branch")?;
                let else_ = self.child(ite.else_body(), ite.syntax(), "else branch")?;
                let cond = self.lower_expr(block, &cond)?;
                self.emit_if(
                    block,
                    cond,
                    |this, b| this.lower_expr(b, &then_),
                    |this, b| this.lower_expr(b, &else_),
                )
            }
            Expr::Assert(assert) => {
                let cond = self.child(assert.condition(), assert.syntax(), "condition")?;
                let body = self.child(assert.body(), assert.syntax(), "body")?;
                let cond = self.lower_expr(block, &cond)?;
                let op = AssertOp::new(self.ctx, cond);
                self.append(block, &op);
                let body_block = add_region_block(self.ctx, op.get_operation(), 0);
                let v = self.lower_expr(body_block, &body)?;
                self.emit_yield(body_block, v);
                Ok(result_of(self.ctx, &op))
            }
            Expr::With(with) => {
                let ns = self.child(with.namespace(), with.syntax(), "namespace")?;
                let body = self.child(with.body(), with.syntax(), "body")?;
                let ns = self.lower_expr(block, &ns)?;
                let op = WithOp::new(self.ctx, ns);
                self.append(block, &op);
                let body_block = add_region_block(self.ctx, op.get_operation(), 0);
                self.with_depth += 1;
                let v = self.lower_expr(body_block, &body);
                self.with_depth -= 1;
                self.emit_yield(body_block, v?);
                Ok(result_of(self.ctx, &op))
            }
            Expr::Select(select) => {
                let base = self.child(select.expr(), select.syntax(), "expression")?;
                let path = self.child(select.attrpath(), select.syntax(), "attribute path")?;
                let base = self.lower_expr(block, &base)?;
                let keys = self.lower_attrpath(block, &path)?;
                match select.default_expr() {
                    None => Ok(self.emit_select_path(block, base, &keys)),
                    Some(default) => {
                        let has = self.emit_has_path(block, base, &keys)?;
                        self.emit_if(
                            block,
                            has,
                            |this, b| Ok(this.emit_select_path(b, base, &keys)),
                            |this, b| this.lower_expr(b, &default),
                        )
                    }
                }
            }
            Expr::HasAttr(has) => {
                let base = self.child(has.expr(), has.syntax(), "expression")?;
                let path = self.child(has.attrpath(), has.syntax(), "attribute path")?;
                let base = self.lower_expr(block, &base)?;
                let keys = self.lower_attrpath(block, &path)?;
                self.emit_has_path(block, base, &keys)
            }
            Expr::UnaryOp(un) => {
                let kind = match un.operator() {
                    Some(ast::UnaryOpKind::Invert) => UnOp::Not,
                    Some(ast::UnaryOpKind::Negate) => UnOp::Neg,
                    None => return Err(Diag::new(ErrorKind::Missing("unary operator"), self.cur)),
                };
                let operand = self.child(un.expr(), un.syntax(), "operand")?;
                let operand = self.lower_expr(block, &operand)?;
                let op = UnaryOp::new(self.ctx, kind, operand);
                Ok(self.emit(block, op))
            }
            Expr::BinOp(bin) => self.lower_binop(block, bin),
            Expr::CurPos(pos) => {
                let Some(file) = self.file.cur_pos_file().map(str::to_string) else {
                    return Ok(self.emit_const(block, Literal::Null));
                };
                let (line, column) = self.file.line_col(pos.syntax().text_range().start());
                let as_int =
                    |n: usize| Literal::Int(i64::try_from(n).expect("source positions fit in i64"));
                let set = AttrSetOp::new(self.ctx);
                self.append(block, &set);
                let inner = add_region_block(self.ctx, set.get_operation(), 0);
                for (name, literal) in [
                    ("column", as_int(column)),
                    ("file", Literal::String(file)),
                    ("line", as_int(line)),
                ] {
                    let v = self.emit_const(inner, literal);
                    let bind = BindOp::new(self.ctx, name, v);
                    self.append(inner, &bind);
                }
                Ok(result_of(self.ctx, &set))
            }
            Expr::Error(_) => Err(Diag::new(ErrorKind::Syntax, self.cur)),
        }
    }

    fn lower_binop(&mut self, block: Ptr<BasicBlock>, bin: &ast::BinOp) -> Result<Value> {
        use ast::BinOpKind as K;
        let kind = self.child(bin.operator(), bin.syntax(), "binary operator")?;
        let lhs = self.child(bin.lhs(), bin.syntax(), "left operand")?;
        let rhs = self.child(bin.rhs(), bin.syntax(), "right operand")?;
        let lhs = self.lower_expr(block, &lhs)?;
        let rhs = self.lower_expr(block, &rhs)?;
        let kind = match kind {
            K::PipeRight => {
                let op = ApplyOp::new(self.ctx, rhs, lhs);
                return Ok(self.emit(block, op));
            }
            K::PipeLeft => {
                let op = ApplyOp::new(self.ctx, lhs, rhs);
                return Ok(self.emit(block, op));
            }
            K::Concat => BinOp::ConcatLists,
            K::Update => BinOp::MergeAttrs,
            K::Add => BinOp::Add,
            K::Sub => BinOp::Sub,
            K::Mul => BinOp::Mul,
            K::Div => BinOp::Div,
            K::And => BinOp::And,
            K::Or => BinOp::Or,
            K::Implication => BinOp::Imply,
            K::Equal => BinOp::Eq,
            K::NotEqual => BinOp::Ne,
            K::Less => BinOp::Lt,
            K::LessOrEq => BinOp::Le,
            K::More => BinOp::Gt,
            K::MoreOrEq => BinOp::Ge,
        };
        let op = BinaryOp::new(self.ctx, kind, lhs, rhs);
        Ok(self.emit(block, op))
    }

    fn lower_str(&mut self, block: Ptr<BasicBlock>, s: &ast::Str) -> Result<Value> {
        let parts = s.normalized_parts();
        match parts.as_slice() {
            [] => return Ok(self.emit_str(block, "")),
            [InterpolPart::Literal(lit)] => return Ok(self.emit_str(block, lit)),
            _ => {}
        }
        let mut values = Vec::new();
        for part in &parts {
            values.push(match part {
                InterpolPart::Literal(lit) => self.emit_str(block, lit),
                InterpolPart::Interpolation(i) => {
                    let e = self.child(i.expr(), i.syntax(), "interpolated expression")?;
                    self.lower_expr(block, &e)?
                }
            });
        }
        let op = InterpolateOp::new(self.ctx, InterpKind::String, values);
        Ok(self.emit(block, op))
    }

    fn lower_path(&mut self, block: Ptr<BasicBlock>, path: &ast::Path) -> Result<Value> {
        if let ast::Path::PathSearch(search) = path {
            // <p>  ==>  __findFile __nixPath "p"
            let content = self.child(search.content(), search.syntax(), "search path")?;
            let name = content.text().trim_start_matches('<').trim_end_matches('>');
            let find_file = self.lower_ident(block, "__findFile")?;
            let nix_path = self.lower_ident(block, "__nixPath")?;
            let name = self.emit_str(block, name);
            let partial = ApplyOp::new(self.ctx, find_file, nix_path);
            let partial = self.emit(block, partial);
            let op = ApplyOp::new(self.ctx, partial, name);
            return Ok(self.emit(block, op));
        }

        let parts = path.parts();
        let interpolated = parts.len() > 1;
        let resolve = |this: &Self, text: &str| {
            crate::paths::resolve_literal(
                text,
                this.file.base_dir(),
                this.options.home_dir.as_deref(),
                interpolated,
            )
            .ok_or_else(|| Diag::new(ErrorKind::NoHomeDir(text.to_string()), this.cur))
        };
        if let [InterpolPart::Literal(lit)] = parts.as_slice() {
            let resolved = resolve(self, lit.text())?;
            return Ok(self.emit_const(block, Literal::Path(resolved)));
        }
        // Like Nix, resolve only the leading literal (as a path value); the
        // other pieces are raw strings.
        let mut values = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            values.push(match part {
                InterpolPart::Literal(lit) if i == 0 => {
                    let resolved = resolve(self, lit.text())?;
                    self.emit_const(block, Literal::Path(resolved))
                }
                InterpolPart::Literal(lit) => self.emit_str(block, lit.text()),
                InterpolPart::Interpolation(i) => {
                    let e = self.child(i.expr(), i.syntax(), "interpolated expression")?;
                    self.lower_expr(block, &e)?
                }
            });
        }
        let op = InterpolateOp::new(self.ctx, InterpKind::Path, values);
        Ok(self.emit(block, op))
    }

    // ─── Attribute access ───────────────────────────────────────────────────

    fn lower_key(&mut self, block: Ptr<BasicBlock>, key: &Key) -> Result<Value> {
        match key {
            Key::Static(name, range) => Ok(self.at(*range, |this| this.emit_str(block, name))),
            Key::Dynamic(e, _) => self.lower_expr(block, e),
        }
    }

    fn lower_attrpath(
        &mut self,
        block: Ptr<BasicBlock>,
        path: &ast::Attrpath,
    ) -> Result<Vec<Value>> {
        path.attrs()
            .map(|attr| self.lower_key(block, &key_of(attr)))
            .collect()
    }

    fn emit_select_path(&mut self, block: Ptr<BasicBlock>, base: Value, keys: &[Value]) -> Value {
        keys.iter().fold(base, |acc, &key| {
            let op = SelectOp::new(self.ctx, acc, key);
            self.emit(block, op)
        })
    }

    /// `base ? k0.k1...`: each step is only checked if the previous one held.
    fn emit_has_path(
        &mut self,
        block: Ptr<BasicBlock>,
        base: Value,
        keys: &[Value],
    ) -> Result<Value> {
        let (&first, rest) = keys
            .split_first()
            .ok_or_else(|| Diag::new(ErrorKind::Missing("attribute path"), self.cur))?;
        let has = HasAttrOp::new(self.ctx, base, first);
        let has = self.emit(block, has);
        if rest.is_empty() {
            return Ok(has);
        }
        self.emit_if(
            block,
            has,
            |this, b| {
                let op = SelectOp::new(this.ctx, base, first);
                let next = this.emit(b, op);
                this.emit_has_path(b, next, rest)
            },
            |this, b| Ok(this.emit_const(b, Literal::Bool(false))),
        )
    }
}
