//! Attribute set and `let` bindings: merging attribute paths the way Nix
//! does, and lowering the merged bindings.

use std::collections::HashMap;

use pliron::basic_block::BasicBlock;
use pliron::context::Ptr;
use pliron::op::Op;
use pliron::value::Value;
use rnix::TextRange;
use rnix::ast::{self, HasEntry, InterpolPart};
use rowan::ast::AstNode;

use super::{Binding, Lowerer, Result, ident_name};
use crate::diagnostics::{Diag, ErrorKind};
use crate::hir::{
    AttrSetOp, BindOp, DynBindOp, LetInOp, RecAttrSetOp, SelectOp, add_region_block, result_of,
};

/// A key in an attribute path.
#[derive(Clone)]
pub(super) enum Key {
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
pub(super) struct Group {
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

    pub(super) fn from_entries<N: HasEntry>(
        node: &N,
        rec: bool,
        path: Vec<String>,
    ) -> Result<Self> {
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
            return Err(Diag::new(
                ErrorKind::DuplicateAttr(child_path.join(".")),
                first.range(),
            )
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
pub(super) fn key_of(attr: ast::Attr) -> Key {
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

impl Lowerer<'_> {
    // ─── Bindings ───────────────────────────────────────────────────────────

    /// Lowers an attribute set, recursive or not.
    pub(super) fn lower_group(&mut self, block: Ptr<BasicBlock>, group: Group) -> Result<Value> {
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
    pub(super) fn lower_rec_attr_set(
        &mut self,
        block: Ptr<BasicBlock>,
        group: Group,
    ) -> Result<Value> {
        let inherited = self.lower_plain_inherits(block, &group)?;
        let set = RecAttrSetOp::new(self.ctx);
        self.append(block, &set);
        let inner = add_region_block(self.ctx, set.get_operation(), 0);
        self.push_rec_scope(&group);
        let res = self.lower_bindings(inner, group, &inherited);
        self.scopes.pop();
        res?;
        Ok(result_of(self.ctx, &set))
    }

    pub(super) fn lower_let_in(
        &mut self,
        block: Ptr<BasicBlock>,
        let_in: &ast::LetIn,
    ) -> Result<Value> {
        let group = Group::from_entries(let_in, false, Vec::new())?;
        if let Some(e) = group
            .entries
            .iter()
            .find(|e| matches!(e.key, Key::Dynamic(..)))
        {
            return Err(Diag::new(
                ErrorKind::DynamicAttrNotAllowed("let"),
                e.key.range(),
            ));
        }
        let body = self.child(let_in.body(), let_in.syntax(), "let body")?;

        let inherited = self.lower_plain_inherits(block, &group)?;
        let op = LetInOp::new(self.ctx);
        self.append(block, &op);
        let bindings = add_region_block(self.ctx, op.get_operation(), 0);
        let body_block = add_region_block(self.ctx, op.get_operation(), 1);

        self.push_rec_scope(&group);
        let res = self
            .lower_bindings(bindings, group, &inherited)
            .and_then(|()| {
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
    fn lower_plain_inherits(
        &mut self,
        block: Ptr<BasicBlock>,
        group: &Group,
    ) -> Result<HashMap<String, Value>> {
        let mut inherited = HashMap::new();
        for entry in &group.entries {
            if let Node::Inherit(name) = &entry.node {
                let v = self.at(entry.key.range(), |this| this.lower_ident(block, name))?;
                inherited.insert(name.clone(), v);
            }
        }
        Ok(inherited)
    }

    /// Emits the `nix_hir.bind` / `nix_hir.dyn_bind` ops of `group` into `block`.
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
                    None => self.at(key.range(), |this| this.lower_ident(block, &name))?,
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
}
