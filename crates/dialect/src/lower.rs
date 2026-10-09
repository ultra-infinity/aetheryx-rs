//! Lowering from the rnix AST to the Nix dialect.
//!
//! The entry point is [`lower_root`], which wraps the lowered expression of a
//! whole file in a `builtin.module` holding `builtin.func @main` of type
//! `() -> (nix.value)`, where `nix.value` is the type [`crate::ValueType`].
//!
//! # Name resolution
//! Names are resolved at lowering time where Nix semantics allow it:
//! - Lambda parameters and pattern formals are SSA values (block arguments or
//!   values derived from them) and are referenced directly.
//! - Names bound by `let` and `rec { }` are recursive, so they cannot be plain
//!   SSA values. References to them lower to `nix.var "name"`, which resolves
//!   to the innermost enclosing `nix.let_in` / `nix.rec_attr_set` binding it.
//! - Anything else lowers to `nix.lookup "name"`, which resolves through the
//!   enclosing `nix.with` scopes and then the builtins. This mirrors Nix, where
//!   `with` never shadows a lexical binding.
//!
//! # Desugaring
//! - Attribute paths in bindings (`a.b = 1; a.c = 2;`) are merged into nested
//!   attribute sets, as Nix does.
//! - `e.a.b` becomes a chain of `nix.select`s; `e ? a.b` and `e.a.b or d` become
//!   `nix.has_attr` checks guarded by `nix.if`.
//! - `a |> f` and `f <| a` become `nix.apply`.
//! - `<p>` becomes `__findFile __nixPath "p"`.
//! - `let { ...; body = e; }` becomes `(rec { ...; body = e; }).body`.
//!
//! # Locations
//! Every op is located at the source range of the syntax it was lowered from
//! (see [`crate::diagnostics`]). Ops introduced by desugaring take the range of
//! the construct being desugared; `nix.bind` takes the range of its binding.

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
use rnix::ast::{self, HasEntry, InterpolPart};
use rowan::ast::AstNode;

use crate::diagnostics::{Diag, ErrorKind, SourceError, SourceFile};
use crate::{
    ApplyOp, AssertOp, AttrSetOp, BinOp, BinaryOp, BindOp, ConstOp, DynBindOp, FormalsOp,
    HasAttrOp, IfOp, InterpKind, InterpolateOp, LambdaOp, LetInOp, ListOp, LiteralKind, LookupOp,
    RecAttrSetOp, SelectOp, UnOp, UnaryOp, ValueType, VarOp, WithOp, YieldOp, add_region_block,
    add_region_block_with_args, result_of,
};

type Result<T> = std::result::Result<T, Diag>;

/// Lowers a parsed Nix file into a verified module containing `@main`.
///
/// `root` must have been parsed from `file`'s text.
pub fn lower_root(
    ctx: &mut Context,
    file: &SourceFile,
    root: &ast::Root,
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
    module.get_operation().deref_mut(ctx).set_loc(whole_file.clone());
    func.get_operation().deref_mut(ctx).set_loc(whole_file);

    let entry = func.get_entry_block(ctx);
    let mut lowerer = Lowerer {
        ctx,
        file,
        cur: root.syntax().text_range(),
        scopes: Vec::new(),
    };
    let lowered = lowerer
        .child(root.expr(), root.syntax(), "expression")
        .and_then(|expr| lowerer.lower_expr(entry, &expr));
    let value = lowered.map_err(|d| file.error(d))?;
    lowerer.emit_yield(entry, value);

    verify_op(&module, ctx).map_err(|e| {
        file.error(Diag::at_location(ErrorKind::Verify(e.err.to_string()), &e.loc))
    })?;
    Ok(module)
}

/// How a name in scope is bound.
#[derive(Clone, Copy)]
enum Binding {
    /// Directly available as an SSA value (lambda parameters / formals).
    Ssa(Value),
    /// Bound recursively by an enclosing `let` / `rec { }`: use `nix.var`.
    Rec,
}

/// A key in an attribute path.
#[derive(Clone)]
enum Key {
    Static(String, TextRange),
    Dynamic(ast::Expr, TextRange),
}

impl Key {
    fn range(&self) -> TextRange {
        match self {
            Key::Static(_, range) | Key::Dynamic(_, range) => *range,
        }
    }
}

/// The right-hand side of one binding, after merging attribute paths.
enum Node {
    Expr(ast::Expr),
    /// `inherit name;`
    Inherit(String),
    /// `inherit (from) name;`, `from` indexing [`Group::inherit_froms`].
    InheritFrom(usize, String),
    /// Nested attribute set built from attribute paths (`a.b = ...`).
    Group(Group),
}

/// One `key = node;` binding of a [`Group`].
struct Entry {
    key: Key,
    node: Node,
    /// The whole binding (`a.b = 1;` / `inherit (x) a;`) in the source.
    range: TextRange,
}

/// The bindings of one attribute set / let, with attribute paths merged.
///
/// Merging follows Nix (verified against Nix 2.28):
/// - `a.b = 1; a.c = 2;`, `a = { b = 1; }; a.c = 2;` and `a = { b = 1; };
///   a = { c = 2; };` all merge into one set `a`, recursively.
/// - The *first* definition decides whether the merged set is `rec`; every
///   merged binding is evaluated in that scope, and `rec` on later literals
///   is ignored. E.g. `{ a = rec { x = 1; }; a.y = x; }` gives `a.y == 1`.
/// - Duplicates are reported with their full path from the set where the
///   merge happens (`'a.b'`), or relative to the literal they occur in.
#[derive(Default)]
struct Group {
    entries: Vec<Entry>,
    /// Source expressions of `inherit (e) ...;` entries, evaluated once each.
    inherit_froms: Vec<ast::Expr>,
    /// Whether this is a `rec { }` set.
    rec: bool,
    /// Attribute path of this group, used in duplicate-attribute errors.
    path: Vec<String>,
}

impl Group {
    fn new(rec: bool, path: Vec<String>) -> Self {
        Group {
            rec,
            path,
            ..Group::default()
        }
    }

    fn from_entries<N: HasEntry>(node: &N, rec: bool, path: Vec<String>) -> Result<Self> {
        let mut group = Group::new(rec, path);
        group.add_entries(node)?;
        Ok(group)
    }

    fn add_entries<N: HasEntry>(&mut self, node: &N) -> Result<()> {
        for entry in node.entries() {
            let range = entry.syntax().text_range();
            match entry {
                ast::Entry::AttrpathValue(apv) => {
                    let missing = |what| Diag::new(ErrorKind::Missing(what), range);
                    let path = apv.attrpath().ok_or_else(|| missing("attribute path"))?;
                    let keys = path.attrs().map(key_of).collect::<Vec<_>>();
                    let value = apv.value().ok_or_else(|| missing("binding value"))?;
                    self.insert(&keys, Node::Expr(value), range)?;
                }
                ast::Entry::Inherit(inherit) => {
                    let from = inherit.from().and_then(|f| f.expr()).map(|e| {
                        self.inherit_froms.push(e);
                        self.inherit_froms.len() - 1
                    });
                    for attr in inherit.attrs() {
                        let key = key_of(attr);
                        let Key::Static(name, _) = &key else {
                            return Err(Diag::new(
                                ErrorKind::DynamicAttrNotAllowed("inherit"),
                                key.range(),
                            ));
                        };
                        let node = match from {
                            Some(idx) => Node::InheritFrom(idx, name.clone()),
                            None => Node::Inherit(name.clone()),
                        };
                        self.insert(&[key], node, range)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The path of the attribute `key` of this group.
    fn child_path(&self, key: &Key) -> Vec<String> {
        let segment = match key {
            Key::Static(name, _) => name.clone(),
            Key::Dynamic(e, _) => format!("${{{}}}", e.syntax().text()),
        };
        self.path.iter().cloned().chain([segment]).collect()
    }

    fn insert(&mut self, path: &[Key], node: Node, range: TextRange) -> Result<()> {
        let (first, rest) = path
            .split_first()
            .ok_or_else(|| Diag::new(ErrorKind::Missing("attribute path"), range))?;
        let child_path = self.child_path(first);

        let existing = match first {
            // Dynamic keys are never merged.
            Key::Dynamic(..) => None,
            Key::Static(name, _) => self
                .entries
                .iter()
                .position(|e| matches!(&e.key, Key::Static(n, _) if n == name)),
        };
        let Some(idx) = existing else {
            let node = if rest.is_empty() {
                node
            } else {
                let mut group = Group::new(false, child_path);
                group.insert(rest, node, range)?;
                Node::Group(group)
            };
            self.entries.push(Entry {
                key: first.clone(),
                node,
                range,
            });
            return Ok(());
        };

        let new_is_set = matches!(node, Node::Expr(ast::Expr::AttrSet(_)));
        if !(new_is_set || !rest.is_empty()) || !self.entries[idx].node.is_mergeable() {
            return Err(Diag::new(ErrorKind::DuplicateAttr(child_path.join(".")), first.range())
                .with_label(self.entries[idx].key.range(), "first defined here"));
        }
        self.entries[idx].node.make_group(child_path)?;
        let Node::Group(group) = &mut self.entries[idx].node else {
            unreachable!("mergeable nodes become groups");
        };
        match node {
            Node::Expr(ast::Expr::AttrSet(set)) if rest.is_empty() => {
                // Duplicates within the literal itself are reported relative to it.
                Group::from_entries(&set, false, Vec::new())?;
                group.add_entries(&set)
            }
            node => group.insert(rest, node, range),
        }
    }

    /// Names bound by this group that are visible as recursive bindings.
    fn static_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|e| match &e.key {
                Key::Static(n, _) => Some(n.clone()),
                Key::Dynamic(..) => None,
            })
            .collect()
    }
}

impl Node {
    fn is_mergeable(&self) -> bool {
        matches!(self, Node::Group(_) | Node::Expr(ast::Expr::AttrSet(_)))
    }

    /// Turns an attribute set literal at `path` into a mergeable [`Group`],
    /// keeping its `rec`-ness.
    fn make_group(&mut self, path: Vec<String>) -> Result<()> {
        if let Node::Expr(ast::Expr::AttrSet(set)) = self {
            let rec = set.rec_token().is_some();
            // Duplicates within the literal itself are reported relative to it.
            Group::from_entries(set, rec, Vec::new())?;
            *self = Node::Group(Group::from_entries(set, rec, path)?);
        }
        Ok(())
    }
}

/// Returns the key of an attribute, folding constant strings to static keys.
fn key_of(attr: ast::Attr) -> Key {
    let range = attr.syntax().text_range();
    let static_str = |s: &ast::Str| match s.normalized_parts().as_slice() {
        [] => Some(String::new()),
        [InterpolPart::Literal(lit)] => Some(lit.clone()),
        _ => None,
    };
    match attr {
        ast::Attr::Ident(id) => Key::Static(ident_name(&id), range),
        ast::Attr::Str(s) => match static_str(&s) {
            Some(name) => Key::Static(name, range),
            None => Key::Dynamic(ast::Expr::Str(s), range),
        },
        ast::Attr::Dynamic(d) => match d.expr() {
            Some(ast::Expr::Str(s)) if static_str(&s).is_some() => {
                Key::Static(static_str(&s).unwrap(), range)
            }
            Some(e) => Key::Dynamic(e, range),
            None => Key::Static(String::new(), range),
        },
    }
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

struct Lowerer<'a> {
    ctx: &'a mut Context,
    file: &'a SourceFile,
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

    fn emit_const(&mut self, block: Ptr<BasicBlock>, kind: LiteralKind) -> Value {
        let op = ConstOp::new(self.ctx, kind);
        self.emit(block, op)
    }

    fn emit_str(&mut self, block: Ptr<BasicBlock>, s: &str) -> Value {
        self.emit_const(block, LiteralKind::String(s.to_string()))
    }

    /// Emits `nix.if cond` and fills its branches with `then_` and `else_`.
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

    fn child<T>(&self, child: Option<T>, parent: &rnix::SyntaxNode, what: &'static str) -> Result<T> {
        child.ok_or_else(|| Diag::new(ErrorKind::Missing(what), parent.text_range()))
    }

    // ─── Scopes ─────────────────────────────────────────────────────────────

    fn resolve(&self, name: &str) -> Option<Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn lower_ident(&mut self, block: Ptr<BasicBlock>, name: &str) -> Value {
        match self.resolve(name) {
            Some(Binding::Ssa(v)) => v,
            Some(Binding::Rec) => {
                let op = VarOp::new(self.ctx, name);
                self.emit(block, op)
            }
            None => {
                let lit = match name {
                    "true" => Some(LiteralKind::Bool(true)),
                    "false" => Some(LiteralKind::Bool(false)),
                    "null" => Some(LiteralKind::Null),
                    _ => None,
                };
                match lit {
                    Some(lit) => self.emit_const(block, lit),
                    None => {
                        let op = LookupOp::new(self.ctx, name);
                        self.emit(block, op)
                    }
                }
            }
        }
    }

    /// Gives a block argument a readable name when the Nix name allows it.
    fn name_value(&mut self, value: Value, name: &str) {
        if let Ok(id) = Identifier::try_from(sanitize_identifier(name)) {
            value.set_name(self.ctx, Some(id));
        }
    }

    // ─── Expressions ────────────────────────────────────────────────────────

    fn lower_expr(&mut self, block: Ptr<BasicBlock>, expr: &ast::Expr) -> Result<Value> {
        self.at(expr.syntax().text_range(), |this| this.lower_expr_here(block, expr))
    }

    fn lower_expr_here(&mut self, block: Ptr<BasicBlock>, expr: &ast::Expr) -> Result<Value> {
        use ast::Expr;
        match expr {
            Expr::Literal(lit) => {
                let text = lit.syntax().text().to_string();
                let kind = match lit.kind() {
                    ast::LiteralKind::Integer(_) | ast::LiteralKind::Float(_) => {
                        LiteralKind::Number(text)
                    }
                    ast::LiteralKind::Uri(_) => LiteralKind::Uri(text),
                };
                Ok(self.emit_const(block, kind))
            }
            Expr::Str(s) => self.lower_str(block, s),
            Expr::PathAbs(_) | Expr::PathRel(_) | Expr::PathHome(_) | Expr::PathSearch(_) => {
                let path = self.child(ast::Path::cast(expr.syntax().clone()), expr.syntax(), "path")?;
                self.lower_path(block, &path)
            }
            Expr::Ident(id) => Ok(self.lower_ident(block, &ident_name(id))),
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
                let group = Group::from_entries(set, set.rec_token().is_some(), Vec::new())?;
                self.lower_group(block, group)
            }
            Expr::LetIn(let_in) => self.lower_let_in(block, let_in),
            Expr::LegacyLet(legacy) => {
                let group = Group::from_entries(legacy, true, Vec::new())?;
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
                let v = self.lower_expr(body_block, &body)?;
                self.emit_yield(body_block, v);
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
                let (line, col) = self.file.line_col(pos.syntax().text_range().start());
                let file = self.file.path().display().to_string();
                let set = AttrSetOp::new(self.ctx);
                self.append(block, &set);
                let inner = add_region_block(self.ctx, set.get_operation(), 0);
                for (name, kind) in [
                    ("column", LiteralKind::Number(col.to_string())),
                    ("file", LiteralKind::String(file)),
                    ("line", LiteralKind::Number(line.to_string())),
                ] {
                    let v = self.emit_const(inner, kind);
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
            let find_file = self.lower_ident(block, "__findFile");
            let nix_path = self.lower_ident(block, "__nixPath");
            let name = self.emit_str(block, name);
            let partial = ApplyOp::new(self.ctx, find_file, nix_path);
            let partial = self.emit(block, partial);
            let op = ApplyOp::new(self.ctx, partial, name);
            return Ok(self.emit(block, op));
        }

        let parts = path.parts();
        if let [InterpolPart::Literal(lit)] = parts.as_slice() {
            return Ok(self.emit_const(block, LiteralKind::Path(lit.text().to_string())));
        }
        let mut values = Vec::new();
        for part in &parts {
            values.push(match part {
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

    fn lower_attrpath(&mut self, block: Ptr<BasicBlock>, path: &ast::Attrpath) -> Result<Vec<Value>> {
        path.attrs()
            .map(key_of)
            .collect::<Vec<_>>()
            .iter()
            .map(|k| self.lower_key(block, k))
            .collect()
    }

    fn emit_select_path(&mut self, block: Ptr<BasicBlock>, base: Value, keys: &[Value]) -> Value {
        keys.iter().fold(base, |acc, &key| {
            let op = SelectOp::new(self.ctx, acc, key);
            self.emit(block, op)
        })
    }

    /// `base ? k0.k1...`: each step is only checked if the previous one held.
    fn emit_has_path(&mut self, block: Ptr<BasicBlock>, base: Value, keys: &[Value]) -> Result<Value> {
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
            |this, b| Ok(this.emit_const(b, LiteralKind::Bool(false))),
        )
    }

    // ─── Bindings ───────────────────────────────────────────────────────────

    /// Lowers an attribute set, recursive or not.
    fn lower_group(&mut self, block: Ptr<BasicBlock>, group: Group) -> Result<Value> {
        if group.rec {
            return self.lower_rec_attr_set(block, group);
        }
        let set = AttrSetOp::new(self.ctx);
        self.append(block, &set);
        let inner = add_region_block(self.ctx, set.get_operation(), 0);
        self.lower_bindings(inner, group, &HashMap::new())?;
        Ok(result_of(self.ctx, &set))
    }

    /// Lowers a `rec { }` attribute set; its static names are in scope inside.
    fn lower_rec_attr_set(&mut self, block: Ptr<BasicBlock>, group: Group) -> Result<Value> {
        let inherited = self.lower_plain_inherits(block, &group);
        let set = RecAttrSetOp::new(self.ctx);
        self.append(block, &set);
        let inner = add_region_block(self.ctx, set.get_operation(), 0);
        self.push_rec_scope(&group);
        let res = self.lower_bindings(inner, group, &inherited);
        self.scopes.pop();
        res?;
        Ok(result_of(self.ctx, &set))
    }

    fn lower_let_in(&mut self, block: Ptr<BasicBlock>, let_in: &ast::LetIn) -> Result<Value> {
        let group = Group::from_entries(let_in, false, Vec::new())?;
        if let Some(e) = group.entries.iter().find(|e| matches!(e.key, Key::Dynamic(..))) {
            return Err(Diag::new(ErrorKind::DynamicAttrNotAllowed("let"), e.key.range()));
        }
        let body = self.child(let_in.body(), let_in.syntax(), "let body")?;

        let inherited = self.lower_plain_inherits(block, &group);
        let op = LetInOp::new(self.ctx);
        self.append(block, &op);
        let bindings = add_region_block(self.ctx, op.get_operation(), 0);
        let body_block = add_region_block(self.ctx, op.get_operation(), 1);

        self.push_rec_scope(&group);
        let res = self.lower_bindings(bindings, group, &inherited).and_then(|()| {
            let v = self.lower_expr(body_block, &body)?;
            self.emit_yield(body_block, v);
            Ok(())
        });
        self.scopes.pop();
        res?;
        Ok(result_of(self.ctx, &op))
    }

    fn push_rec_scope(&mut self, group: &Group) {
        let scope = group
            .static_names()
            .into_iter()
            .map(|n| (n, Binding::Rec))
            .collect();
        self.scopes.push(scope);
    }

    /// `inherit x;` in a `let` / `rec { }` refers to `x` from the *enclosing*
    /// scope, so it is lowered before the recursive scope is entered.
    fn lower_plain_inherits(&mut self, block: Ptr<BasicBlock>, group: &Group) -> HashMap<String, Value> {
        let mut inherited = HashMap::new();
        for entry in &group.entries {
            if let Node::Inherit(name) = &entry.node {
                let v = self.at(entry.key.range(), |this| this.lower_ident(block, name));
                inherited.insert(name.clone(), v);
            }
        }
        inherited
    }

    /// Emits the `nix.bind` / `nix.dyn_bind` ops of `group` into `block`.
    fn lower_bindings(
        &mut self,
        block: Ptr<BasicBlock>,
        group: Group,
        inherited: &HashMap<String, Value>,
    ) -> Result<()> {
        let mut froms: HashMap<usize, Value> = HashMap::new();
        for Entry { key, node, range } in group.entries {
            let value = match node {
                Node::Expr(e) => self.lower_expr(block, &e)?,
                Node::Group(g) => self.at(range, |this| this.lower_group(block, g))?,
                Node::Inherit(name) => match inherited.get(&name) {
                    Some(&v) => v,
                    None => self.at(key.range(), |this| this.lower_ident(block, &name)),
                },
                Node::InheritFrom(idx, name) => {
                    let from = match froms.get(&idx) {
                        Some(&v) => v,
                        None => {
                            let v = self.lower_expr(block, &group.inherit_froms[idx])?;
                            froms.insert(idx, v);
                            v
                        }
                    };
                    self.at(key.range(), |this| {
                        let key = this.emit_str(block, &name);
                        let op = SelectOp::new(this.ctx, from, key);
                        this.emit(block, op)
                    })
                }
            };
            match &key {
                Key::Static(name, _) => self.at(range, |this| {
                    let op = BindOp::new(this.ctx, name, value);
                    this.append(block, &op);
                }),
                Key::Dynamic(e, _) => {
                    let k = self.lower_expr(block, e)?;
                    self.at(range, |this| {
                        let op = DynBindOp::new(this.ctx, k, value);
                        this.append(block, &op);
                    });
                }
            }
        }
        Ok(())
    }

    // ─── Lambdas ────────────────────────────────────────────────────────────

    fn lower_lambda(&mut self, block: Ptr<BasicBlock>, lambda: &ast::Lambda) -> Result<Value> {
        let param = self.child(lambda.param(), lambda.syntax(), "lambda parameter")?;
        let body = self.child(lambda.body(), lambda.syntax(), "lambda body")?;

        let op = LambdaOp::new(self.ctx);
        self.append(block, &op);
        let body_block = add_region_block_with_args(self.ctx, op.get_operation(), 0, 1);
        let arg = body_block.deref(self.ctx).get_argument(0);

        self.scopes.push(HashMap::new());
        let res = self
            .at(param.syntax().text_range(), |this| this.bind_param(body_block, arg, &param))
            .and_then(|()| self.lower_expr(body_block, &body));
        self.scopes.pop();
        let v = res?;
        self.emit_yield(body_block, v);
        Ok(result_of(self.ctx, &op))
    }

    /// Brings the lambda parameter(s) into the innermost scope.
    fn bind_param(&mut self, block: Ptr<BasicBlock>, arg: Value, param: &ast::Param) -> Result<()> {
        let pattern = match param {
            ast::Param::IdentParam(p) => {
                let id = self.child(p.ident(), p.syntax(), "parameter name")?;
                let name = ident_name(&id);
                self.name_value(arg, &name);
                self.scopes.last_mut().unwrap().insert(name, Binding::Ssa(arg));
                return Ok(());
            }
            ast::Param::Pattern(p) => p,
        };

        let bind = pattern.pat_bind().and_then(|b| b.ident());
        let mut formals = Vec::new();
        for entry in pattern.pat_entries() {
            let id = self.child(entry.ident(), entry.syntax(), "formal name")?;
            formals.push((ident_name(&id), entry.default(), entry.syntax().text_range()));
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
                self.scopes.last_mut().unwrap().insert(name, Binding::Ssa(arg));
            }
            None => self.name_value(arg, "arg"),
        }
        let names: Vec<String> = formals.iter().map(|(n, _, _)| n.clone()).collect();
        let checked = FormalsOp::new(self.ctx, arg, &names, pattern.ellipsis_token().is_some());
        let checked = self.emit(block, checked);

        // Formals without defaults first, so that defaults can refer to them.
        let (plain, defaulted): (Vec<_>, Vec<_>) =
            formals.into_iter().partition(|(_, d, _)| d.is_none());
        for (name, _, range) in plain {
            let v = self.at(range, |this| {
                let key = this.emit_str(block, &name);
                let op = SelectOp::new(this.ctx, checked, key);
                this.emit(block, op)
            });
            self.scopes.last_mut().unwrap().insert(name, Binding::Ssa(v));
        }
        for (name, default, range) in defaulted {
            let default = default.unwrap();
            let v = self.at(range, |this| {
                let key = this.emit_str(block, &name);
                let has = HasAttrOp::new(this.ctx, checked, key);
                let has = this.emit(block, has);
                this.emit_if(
                    block,
                    has,
                    |this, b| {
                        let op = SelectOp::new(this.ctx, checked, key);
                        Ok(this.emit(b, op))
                    },
                    |this, b| this.lower_expr(b, &default),
                )
            })?;
            self.scopes.last_mut().unwrap().insert(name, Binding::Ssa(v));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use miette::{Diagnostic, SourceSpan};
    use pliron::linked_list::ContainsLinkedList;
    use pliron::printable::Printable;

    use super::*;
    use crate::diagnostics::span_of;

    fn try_lower(src: &str) -> (Context, std::result::Result<ModuleOp, SourceError>) {
        let mut ctx = Context::new();
        let file = SourceFile::new(&ctx, "test.nix", src);
        let parse = ast::Root::parse(src);
        file.check_parse(parse.errors()).unwrap();
        let res = lower_root(&mut ctx, &file, &parse.tree());
        (ctx, res)
    }

    fn lower(src: &str) -> String {
        let (ctx, res) = try_lower(src);
        res.unwrap().get_operation().disp(&ctx).to_string()
    }

    /// Source text of every op in `@main`'s entry block, in order.
    fn entry_op_texts(src: &str) -> Vec<String> {
        let (ctx, res) = try_lower(src);
        let module = res.unwrap();
        let func = module.get_body(&ctx, 0).deref(&ctx).iter(&ctx).next().unwrap();
        let entry = func.deref(&ctx).get_region(0).deref(&ctx).get_head().unwrap();
        entry
            .deref(&ctx)
            .iter(&ctx)
            .map(|op| {
                let span = span_of(&op.deref(&ctx).loc()).expect("op has a span");
                src[span.offset()..span.offset() + span.len()].to_string()
            })
            .collect()
    }

    #[test]
    fn lowers_arithmetic() {
        let ir = lower("1 + 2 * 3");
        assert!(ir.contains("nix.binary"), "{ir}");
        assert!(ir.contains("Mul"), "{ir}");
        assert!(ir.contains("nix.yield"), "{ir}");
    }

    #[test]
    fn ops_are_located_at_their_source() {
        assert_eq!(
            entry_op_texts("1 + 2 * 3"),
            ["1", "2", "3", "2 * 3", "1 + 2 * 3", "1 + 2 * 3"]
        );
    }

    #[test]
    fn locations_are_printed() {
        let ir = lower("{\n  a = 1;\n}");
        assert!(ir.contains("nix.span 4..10"), "{ir}");
        assert!(ir.contains("\"test.nix\": line: 2, column: 3"), "{ir}");
    }

    #[test]
    fn lambda_params_are_ssa() {
        let ir = lower("x: y: x + y");
        assert!(!ir.contains("nix.lookup"), "{ir}");
        assert!(!ir.contains("nix.var"), "{ir}");
    }

    #[test]
    fn let_bindings_are_vars() {
        let ir = lower("let a = 1; b = a; in b");
        assert!(ir.contains("nix.let_in"), "{ir}");
        assert!(ir.contains("nix.var \"a\""), "{ir}");
        assert!(ir.contains("nix.var \"b\""), "{ir}");
    }

    #[test]
    fn let_shadows_lambda_param() {
        let ir = lower("x: let x = 1; in x");
        assert!(ir.contains("nix.var \"x\""), "{ir}");
    }

    #[test]
    fn with_names_are_lookups() {
        let ir = lower("with builtins; length [ 1 2 ]");
        assert!(ir.contains("nix.lookup \"builtins\""), "{ir}");
        assert!(ir.contains("nix.lookup \"length\""), "{ir}");
        assert!(ir.contains("nix.with"), "{ir}");
    }

    #[test]
    fn attr_paths_are_merged() {
        let ir = lower("{ a.b = 1; a.c = 2; a = { d = 3; }; }");
        assert_eq!(ir.matches("nix.bind \"a\"").count(), 1, "{ir}");
        for name in ["b", "c", "d"] {
            assert!(ir.contains(&format!("nix.bind \"{name}\"")), "{ir}");
        }
    }

    #[test]
    fn duplicate_attr_points_at_both_definitions() {
        let (_, res) = try_lower("{ a = 1; a = 2; }");
        let Err(err) = res else { panic!("expected an error") };
        assert!(matches!(&err.kind, ErrorKind::DuplicateAttr(n) if n == "a"));
        let labels: Vec<SourceSpan> = err.labels().unwrap().map(|l| *l.inner()).collect();
        assert_eq!(labels, [SourceSpan::from(9..10), SourceSpan::from(2..3)]);
    }

    fn duplicate_name(src: &str) -> String {
        match try_lower(src).1 {
            Err(SourceError { kind: ErrorKind::DuplicateAttr(name), .. }) => name,
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("expected a duplicate attribute error for {src}"),
        }
    }

    // Expected names are the ones Nix 2.28 reports for the same input.
    #[test]
    fn duplicate_attr_names_use_full_path() {
        assert_eq!(duplicate_name("{ a.b = 1; a.c = 2; a.b = 3; }"), "a.b");
        assert_eq!(duplicate_name("{ a = { b = 1; }; a = { b = 2; }; }"), "a.b");
        assert_eq!(duplicate_name("{ a.b = { c = 1; }; a.b = { c = 2; }; }"), "a.b.c");
        assert_eq!(duplicate_name("{ x = { a.b = 1; }; x.a = { b = 2; }; }"), "x.a.b");
        assert_eq!(duplicate_name("{ a = 1; a.b = 2; }"), "a");
        assert_eq!(duplicate_name("{ a = rec { x = 1; }; a.x = 2; }"), "a.x");
        // A duplicate inside a single literal is reported relative to it.
        assert_eq!(duplicate_name("{ x = { a = 1; a = 2; }; x.b = 3; }"), "a");
    }

    #[test]
    fn merged_set_takes_rec_from_first_definition() {
        // Nix: { a = { x = 1; y = 1; }; } -- `x` in `a.y` is the rec binding.
        let ir = lower("let x = 9; in { a = rec { x = 1; }; a.y = x; }");
        assert!(ir.contains("nix.rec_attr_set"), "{ir}");
        assert!(ir.contains("nix.var \"x\""), "{ir}");

        // Nix, applied to 9: { a = { x = 1; x2 = 2; z = 9; }; } -- the later
        // `rec` is ignored, so `x2` in `z` is the lambda parameter.
        let ir = lower("x2: { a = { x = 1; }; a = rec { x2 = 2; z = x2; }; }");
        assert!(!ir.contains("nix.rec_attr_set"), "{ir}");
        assert!(!ir.contains("nix.var"), "{ir}");
    }

    #[test]
    fn duplicate_formals_are_errors() {
        // All four are rejected by Nix 2.28 with "duplicate formal function argument 'a'".
        for src in ["{ a, b, a }: a", "{ a, a ? 1 }: a", "a@{ a }: a", "{ a }@a: a"] {
            match try_lower(src).1 {
                Err(SourceError { kind: ErrorKind::DuplicateFormal(name), .. }) => {
                    assert_eq!(name, "a", "{src}")
                }
                Err(e) => panic!("{src}: unexpected error: {e}"),
                Ok(_) => panic!("{src}: expected a duplicate formal error"),
            }
        }
    }

    #[test]
    fn dynamic_let_binding_is_an_error() {
        let (_, res) = try_lower(r#"let ${"a" + "b"} = 1; in 2"#);
        let Err(err) = res else { panic!("expected an error") };
        assert!(matches!(err.kind, ErrorKind::DynamicAttrNotAllowed("let")));
        assert_eq!(err.span(), Some(SourceSpan::from(4..16)));
    }

    #[test]
    fn parse_errors_are_located() {
        let ctx = Context::new();
        let src = "{ a = 1 }";
        let file = SourceFile::new(&ctx, "test.nix", src);
        let errs = file.check_parse(ast::Root::parse(src).errors()).unwrap_err();
        // Only the real error, not rnix's follow-up end-of-file errors.
        assert_eq!(errs.errors.len(), 1, "{errs:?}");
        assert_eq!(errs.errors[0].span(), Some(SourceSpan::from(8..9)));
    }

    #[test]
    fn lowers_kitchen_sink() {
        let ir = lower(
            r#"
            { lib ? null, pkgs, ... }@args:
            let
              inherit (pkgs) hello;
              name = "hi-${hello.pname or "x"}";
              rec' = rec { a = 1; b = a; };
            in
            assert args ? pkgs;
            {
              inherit name;
              p = ./foo/${name};
              s = <nixpkgs>;
              ok = !(rec'.b == 1) || -1 < 2;
              l = [ 1 2.5 ] ++ [ "x" ];
              m = { x = 1; } // { y = 2; };
              f = 1 |> (x: x);
              u = https://example.com;
              pos = __curPos;
              ${name} = if true then null else false;
            }
            "#,
        );
        for needle in [
            "nix.formals",
            "nix.rec_attr_set",
            "nix.interpolate",
            "nix.assert",
            "nix.has_attr",
            "nix.dyn_bind",
            "nix.apply",
            "nix.if",
        ] {
            assert!(ir.contains(needle), "missing {needle} in:\n{ir}");
        }
    }
}
