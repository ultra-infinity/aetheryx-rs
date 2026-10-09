//! `nix_hir`: the high-level Nix IR.
//!
//! The HIR is a desugared, name-resolved form of the Nix AST, produced by
//! [`crate::ast_to_hir`]. It stays close to the source so it can carry precise
//! source locations (see [`crate::diagnostics`]), and is lowered to
//! [`crate::mir`] for code generation.
//!
//! # Conventions
//! - Every SSA value has type `nix_hir.value` ([`ValueType`]) and denotes a
//!   possibly unevaluated Nix value. Laziness is implicit; making it explicit is
//!   the job of the MIR.
//! - A sub-expression that must only be evaluated conditionally
//!   (`nix_hir.if`, `nix_hir.assert`) or inside a new scope (`nix_hir.lambda`,
//!   `nix_hir.with`, the body of `nix_hir.let_in`) lives in a single-block
//!   region terminated by `nix_hir.yield`.
//! - Bindings of attribute sets and `let` live in a single-block region of
//!   `nix_hir.bind` / `nix_hir.dyn_bind` ops without a terminator.
//!
//! pliron requires attribute dictionary keys to be unique across all ops, which
//! is why the attribute keys below are named after their op (`bind_name`,
//! `var_name`, ...) rather than reusing `name` / `kind`.

use std::hash::{Hash, Hasher};

use pliron::basic_block::BasicBlock;
use pliron::builtin::attributes::StringAttr;
use pliron::builtin::op_interfaces::{
    IsTerminatorInterface, NOpdsInterface, NRegionsInterface, NResultsInterface,
    NoTerminatorInterface, SingleBlockRegionInterface,
};
use pliron::combine::parser::char::{char as token_char, spaces};
use pliron::combine::{Parser, between, many1, optional, satisfy, sep_by};
use pliron::context::{Context, Ptr};
use pliron::derive::{pliron_attr, pliron_op, pliron_type};
use pliron::irfmt::parsers::quoted_string_parser;
use pliron::irfmt::printers::quoted;
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::parsable::{Parsable, ParseResult, StateStream};
use pliron::printable::{self, Printable};
use pliron::value::Value;

/// The type of every value in the HIR: a possibly unevaluated Nix value.
#[pliron_type(name = "nix_hir.value", format, generate_get = true, verifier = "succ")]
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct ValueType;

// ─── Attributes ─────────────────────────────────────────────────────────────

/// A Nix float.
///
/// Compared and hashed by bit pattern so that it can be stored in an
/// attribute. Printed in a form that parses back to the same value.
#[derive(Debug, Clone, Copy)]
pub struct NixFloat(pub f64);

impl PartialEq for NixFloat {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for NixFloat {}

impl Hash for NixFloat {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl Printable for NixFloat {
    fn fmt(
        &self,
        _ctx: &Context,
        _state: &printable::State,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        // `Debug` is the shortest representation that round-trips, and always
        // marks the value as a float (`1.0`, `1e308`).
        write!(f, "{:?}", self.0)
    }
}

impl Parsable for NixFloat {
    type Arg = ();
    type Parsed = Self;

    fn parse<'a>(state_stream: &mut StateStream<'a>, _arg: ()) -> ParseResult<'a, Self> {
        many1::<String, _, _>(satisfy(|c: char| {
            c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')
        }))
        .and_then(|s: String| s.parse::<f64>().map(NixFloat))
        .parse_stream(state_stream)
        .into()
    }
}

/// The value of a `nix_hir.const`.
#[pliron_attr(name = "nix_hir.literal", format, verifier = "succ")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Literal {
    Int(i64),
    Float(NixFloat),
    String(String),
    /// An absolute path, already resolved like the Nix parser does.
    Path(String),
    Uri(String),
    Bool(bool),
    Null,
}

/// The operator of a `nix_hir.binary`.
#[pliron_attr(name = "nix_hir.binop", format, verifier = "succ")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `++`
    ConcatLists,
    /// `//`
    MergeAttrs,
    /// `&&`
    And,
    /// `||`
    Or,
    /// `->`
    Imply,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// The operator of a `nix_hir.unary`.
#[pliron_attr(name = "nix_hir.unop", format, verifier = "succ")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    /// `!`
    Not,
    /// `-`
    Neg,
}

/// What a `nix_hir.interpolate` produces.
#[pliron_attr(name = "nix_hir.interp_kind", format, verifier = "succ")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterpKind {
    /// `"a${b}c"`
    String,
    /// `./a/${b}`
    Path,
}

/// A list of names, printed as `["a", "b"]`: the formals of a
/// `{ a, b ? x, ... }` pattern, or the static keys of an attribute set.
#[pliron_attr(name = "nix_hir.name_list", verifier = "succ")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NameList(pub Vec<String>);

impl Printable for NameList {
    fn fmt(
        &self,
        ctx: &Context,
        state: &printable::State,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        write!(f, "[")?;
        for (i, name) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            quoted(name).fmt(ctx, state, f)?;
        }
        write!(f, "]")
    }
}

impl Parsable for NameList {
    type Arg = ();
    type Parsed = Self;

    fn parse<'a>(state_stream: &mut StateStream<'a>, _arg: ()) -> ParseResult<'a, Self> {
        between(
            token_char('[').skip(spaces()),
            token_char(']'),
            sep_by::<Vec<String>, _, _, _>(
                quoted_string_parser().skip(spaces()),
                token_char(',').skip(spaces()),
            ),
        )
        .map(NameList)
        .parse_stream(state_stream)
        .into()
    }
}

/// A `{ a, b ? x, ... }` pattern: the formals in order, whether each has a
/// default, and whether there is an ellipsis. Printed like the pattern
/// without default values: `{ a, b ?, ... }`.
#[pliron_attr(name = "nix_hir.formals_spec", verifier = "succ")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FormalsSpec {
    /// `(name, has_default)`, in declaration order.
    pub formals: Vec<(String, bool)>,
    pub ellipsis: bool,
}

impl Printable for FormalsSpec {
    fn fmt(
        &self,
        _ctx: &Context,
        _state: &printable::State,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        let mut items: Vec<String> = self
            .formals
            .iter()
            .map(|(name, has_default)| {
                if *has_default {
                    format!("{name} ?")
                } else {
                    name.clone()
                }
            })
            .collect();
        if self.ellipsis {
            items.push("...".into());
        }
        if items.is_empty() {
            write!(f, "{{ }}")
        } else {
            write!(f, "{{ {} }}", items.join(", "))
        }
    }
}

impl Parsable for FormalsSpec {
    type Arg = ();
    type Parsed = Self;

    fn parse<'a>(state_stream: &mut StateStream<'a>, _arg: ()) -> ParseResult<'a, Self> {
        // A formal name is a Nix identifier: [A-Za-z_][A-Za-z0-9_'-]*.
        let name = many1::<String, _, _>(satisfy(|c: char| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '-')
        }));
        let formal = name
            .skip(spaces())
            .and(optional(token_char('?').skip(spaces())))
            .map(|(name, default)| Item::Formal(name, default.is_some()));
        let ellipsis = pliron::combine::parser::char::string("...")
            .skip(spaces())
            .map(|_| Item::Ellipsis);
        enum Item {
            Formal(String, bool),
            Ellipsis,
        }
        between(
            token_char('{').skip(spaces()),
            token_char('}'),
            sep_by::<Vec<Item>, _, _, _>(
                pliron::combine::attempt(ellipsis).or(formal),
                token_char(',').skip(spaces()),
            ),
        )
        .map(|items| {
            let mut spec = FormalsSpec {
                formals: Vec::new(),
                ellipsis: false,
            };
            for item in items {
                match item {
                    Item::Formal(name, default) => spec.formals.push((name, default)),
                    Item::Ellipsis => spec.ellipsis = true,
                }
            }
            spec
        })
        .parse_stream(state_stream)
        .into()
    }
}

// ─── Operations ─────────────────────────────────────────────────────────────

#[pliron_op(
    name = "nix_hir.const",
    // A literal value.
    format = "attr($literal, $Literal) ` : ` type($0)",
    interfaces = [
        NOpdsInterface<0>,
        NResultsInterface<1>,
    ],
    attributes = (literal: Literal),
    verifier = "succ"
)]
pub struct ConstOp;

#[pliron_op(
    name = "nix_hir.list",
    // [ elem0 elem1 ... ], printed as `[v0, v1]`: pliron skips whitespace
    // when parsing, so the separator must not be a space.
    format = "`[` operands(CharSpace(`,`)) `] : ` type($0)",
    interfaces = [
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct ListOp;

#[pliron_op(
    name = "nix_hir.attr_set",
    // { attr1 = ...; attr2 = ...; }: a region of binding ops.
    format = "`: ` type($0) ` ` region($0)",
    interfaces = [
        NOpdsInterface<0>,
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NoTerminatorInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct AttrSetOp;

#[pliron_op(
    name = "nix_hir.rec_attr_set",
    // rec { attr1 = ...; attr2 = ...; }: bindings may refer to each other
    // through `nix_hir.var`.
    format = "`: ` type($0) ` ` region($0)",
    interfaces = [
        NOpdsInterface<0>,
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NoTerminatorInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct RecAttrSetOp;

#[pliron_op(
    name = "nix_hir.let_in",
    // let <bindings> in <body>: region 0 holds binding ops (no terminator),
    // region 1 ends in `nix_hir.yield`.
    format = "`: ` type($0) ` ` region($0) ` in ` region($1)",
    interfaces = [
        NOpdsInterface<0>,
        NRegionsInterface<2>,
        SingleBlockRegionInterface,
        NoTerminatorInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct LetInOp;

#[pliron_op(
    name = "nix_hir.bind",
    // name = value; inside an attribute set / let binding region.
    format = "attr($bind_name, $StringAttr) ` = ` $0",
    interfaces = [
        NOpdsInterface<1>,
        NResultsInterface<0>,
    ],
    attributes = (bind_name: StringAttr),
    verifier = "succ"
)]
pub struct BindOp;

#[pliron_op(
    name = "nix_hir.dyn_bind",
    // ${key} = value; with a key computed at runtime.
    format = "$0 ` = ` $1",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<0>,
    ],
    verifier = "succ"
)]
pub struct DynBindOp;

#[pliron_op(
    name = "nix_hir.lambda",
    // A function; the region's single block argument is the argument.
    format = "`: ` type($0) ` ` region($0)",
    interfaces = [
        NOpdsInterface<0>,
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct LambdaOp;

#[pliron_op(
    name = "nix_hir.formals",
    // Checks a lambda argument against a `{ a, b ? x, ... }` pattern and
    // returns it unchanged.
    format = "$0 ` ` attr($formals, $FormalsSpec) ` : ` type($0)",
    interfaces = [
        NOpdsInterface<1>,
        NResultsInterface<1>,
    ],
    attributes = (formals: FormalsSpec),
    verifier = "succ"
)]
pub struct FormalsOp;

#[pliron_op(
    name = "nix_hir.apply",
    // Function application: f x
    format = "$0 ` ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct ApplyOp;

#[pliron_op(
    name = "nix_hir.if",
    // if <cond> then <region 0> else <region 1>
    format = "$0 ` : ` type($0) ` then ` region($0) ` else ` region($1)",
    interfaces = [
        NOpdsInterface<1>,
        NRegionsInterface<2>,
        SingleBlockRegionInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct IfOp;

#[pliron_op(
    name = "nix_hir.assert",
    // assert <cond>; <region>
    format = "$0 ` : ` type($0) ` ` region($0)",
    interfaces = [
        NOpdsInterface<1>,
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct AssertOp;

#[pliron_op(
    name = "nix_hir.with",
    // with <scope>; <region>
    format = "$0 ` : ` type($0) ` ` region($0)",
    interfaces = [
        NOpdsInterface<1>,
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct WithOp;

#[pliron_op(
    name = "nix_hir.select",
    // base.key
    format = "$0 ` . ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct SelectOp;

#[pliron_op(
    name = "nix_hir.has_attr",
    // base ? key
    format = "$0 ` ? ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct HasAttrOp;

#[pliron_op(
    name = "nix_hir.unary",
    format = "attr($unop, $UnOp) ` ` $0 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<1>,
        NResultsInterface<1>,
    ],
    attributes = (unop: UnOp),
    verifier = "succ"
)]
pub struct UnaryOp;

#[pliron_op(
    name = "nix_hir.binary",
    format = "$0 ` ` attr($binop, $BinOp) ` ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    attributes = (binop: BinOp),
    verifier = "succ"
)]
pub struct BinaryOp;

#[pliron_op(
    name = "nix_hir.interpolate",
    // "a${b}c" / ./a/${b}: the concatenation of the operands.
    format = "attr($interp_kind, $InterpKind) ` (` operands(CharSpace(`,`)) `) : ` type($0)",
    interfaces = [
        NResultsInterface<1>,
    ],
    attributes = (interp_kind: InterpKind),
    verifier = "succ"
)]
pub struct InterpolateOp;

#[pliron_op(
    name = "nix_hir.var",
    // A name bound by an enclosing `nix_hir.let_in` / `nix_hir.rec_attr_set`.
    format = "attr($var_name, $StringAttr) ` : ` type($0)",
    interfaces = [
        NOpdsInterface<0>,
        NResultsInterface<1>,
    ],
    attributes = (var_name: StringAttr),
    verifier = "succ"
)]
pub struct VarOp;

#[pliron_op(
    name = "nix_hir.lookup",
    // A name that is neither bound lexically nor a global builtin: resolved
    // through the enclosing `nix_hir.with` scopes, innermost first. Only
    // emitted inside a `nix_hir.with`.
    format = "attr($lookup_name, $StringAttr) ` : ` type($0)",
    interfaces = [
        NOpdsInterface<0>,
        NResultsInterface<1>,
    ],
    attributes = (lookup_name: StringAttr),
    verifier = "succ"
)]
pub struct LookupOp;

#[pliron_op(
    name = "nix_hir.yield",
    // Terminates a region, producing the value of the enclosing op.
    format = "$0",
    interfaces = [
        IsTerminatorInterface,
        NOpdsInterface<1>,
        NResultsInterface<0>,
    ],
    verifier = "succ"
)]
pub struct YieldOp;

// ─── Construction ───────────────────────────────────────────────────────────

/// Creates an operation of kind `T` whose results are all `nix_hir.value`.
pub(crate) fn new_operation<T: Op>(
    ctx: &mut Context,
    n_results: usize,
    operands: Vec<Value>,
    n_regions: usize,
) -> Ptr<Operation> {
    let ty = ValueType::get(ctx).into();
    Operation::new(
        ctx,
        T::get_concrete_op_info(),
        vec![ty; n_results],
        operands,
        vec![],
        n_regions,
    )
}

/// Creates the (single) block of region `idx` of `op`.
pub fn add_region_block(ctx: &mut Context, op: Ptr<Operation>, idx: usize) -> Ptr<BasicBlock> {
    add_region_block_with_args(ctx, op, idx, 0)
}

/// Creates the (single) block of region `idx` of `op`, with `n_args` block
/// arguments of type `nix_hir.value`.
pub fn add_region_block_with_args(
    ctx: &mut Context,
    op: Ptr<Operation>,
    idx: usize,
    n_args: usize,
) -> Ptr<BasicBlock> {
    let ty = ValueType::get(ctx).into();
    let block = BasicBlock::new(ctx, None, vec![ty; n_args]);
    let region = op.deref(ctx).get_region(idx);
    block.insert_at_front(region, ctx);
    block
}

/// The single result of a value-producing op.
pub fn result_of(ctx: &Context, op: &impl Op) -> Value {
    op.get_operation().deref(ctx).get_result(0)
}

impl ConstOp {
    pub fn new(ctx: &mut Context, literal: Literal) -> Self {
        let op = ConstOp {
            op: new_operation::<Self>(ctx, 1, vec![], 0),
        };
        op.set_attr_literal(ctx, literal);
        op
    }
}

impl ListOp {
    pub fn new(ctx: &mut Context, elems: Vec<Value>) -> Self {
        ListOp {
            op: new_operation::<Self>(ctx, 1, elems, 0),
        }
    }
}

impl AttrSetOp {
    pub fn new(ctx: &mut Context) -> Self {
        AttrSetOp {
            op: new_operation::<Self>(ctx, 1, vec![], 1),
        }
    }
}

impl RecAttrSetOp {
    pub fn new(ctx: &mut Context) -> Self {
        RecAttrSetOp {
            op: new_operation::<Self>(ctx, 1, vec![], 1),
        }
    }
}

impl LetInOp {
    pub fn new(ctx: &mut Context) -> Self {
        LetInOp {
            op: new_operation::<Self>(ctx, 1, vec![], 2),
        }
    }
}

impl BindOp {
    pub fn new(ctx: &mut Context, name: &str, value: Value) -> Self {
        let op = BindOp {
            op: new_operation::<Self>(ctx, 0, vec![value], 0),
        };
        op.set_attr_bind_name(ctx, StringAttr::new(name.to_string()));
        op
    }
}

impl DynBindOp {
    pub fn new(ctx: &mut Context, key: Value, value: Value) -> Self {
        DynBindOp {
            op: new_operation::<Self>(ctx, 0, vec![key, value], 0),
        }
    }
}

impl LambdaOp {
    pub fn new(ctx: &mut Context) -> Self {
        LambdaOp {
            op: new_operation::<Self>(ctx, 1, vec![], 1),
        }
    }
}

impl FormalsOp {
    pub fn new(ctx: &mut Context, arg: Value, spec: FormalsSpec) -> Self {
        let op = FormalsOp {
            op: new_operation::<Self>(ctx, 1, vec![arg], 0),
        };
        op.set_attr_formals(ctx, spec);
        op
    }
}

impl ApplyOp {
    pub fn new(ctx: &mut Context, func: Value, arg: Value) -> Self {
        ApplyOp {
            op: new_operation::<Self>(ctx, 1, vec![func, arg], 0),
        }
    }
}

impl IfOp {
    pub fn new(ctx: &mut Context, cond: Value) -> Self {
        IfOp {
            op: new_operation::<Self>(ctx, 1, vec![cond], 2),
        }
    }
}

impl AssertOp {
    pub fn new(ctx: &mut Context, cond: Value) -> Self {
        AssertOp {
            op: new_operation::<Self>(ctx, 1, vec![cond], 1),
        }
    }
}

impl WithOp {
    pub fn new(ctx: &mut Context, scope: Value) -> Self {
        WithOp {
            op: new_operation::<Self>(ctx, 1, vec![scope], 1),
        }
    }
}

impl SelectOp {
    pub fn new(ctx: &mut Context, base: Value, key: Value) -> Self {
        SelectOp {
            op: new_operation::<Self>(ctx, 1, vec![base, key], 0),
        }
    }
}

impl HasAttrOp {
    pub fn new(ctx: &mut Context, base: Value, key: Value) -> Self {
        HasAttrOp {
            op: new_operation::<Self>(ctx, 1, vec![base, key], 0),
        }
    }
}

impl UnaryOp {
    pub fn new(ctx: &mut Context, unop: UnOp, operand: Value) -> Self {
        let op = UnaryOp {
            op: new_operation::<Self>(ctx, 1, vec![operand], 0),
        };
        op.set_attr_unop(ctx, unop);
        op
    }
}

impl BinaryOp {
    pub fn new(ctx: &mut Context, binop: BinOp, lhs: Value, rhs: Value) -> Self {
        let op = BinaryOp {
            op: new_operation::<Self>(ctx, 1, vec![lhs, rhs], 0),
        };
        op.set_attr_binop(ctx, binop);
        op
    }
}

impl InterpolateOp {
    pub fn new(ctx: &mut Context, kind: InterpKind, parts: Vec<Value>) -> Self {
        let op = InterpolateOp {
            op: new_operation::<Self>(ctx, 1, parts, 0),
        };
        op.set_attr_interp_kind(ctx, kind);
        op
    }
}

impl VarOp {
    pub fn new(ctx: &mut Context, name: &str) -> Self {
        let op = VarOp {
            op: new_operation::<Self>(ctx, 1, vec![], 0),
        };
        op.set_attr_var_name(ctx, StringAttr::new(name.to_string()));
        op
    }
}

impl LookupOp {
    pub fn new(ctx: &mut Context, name: &str) -> Self {
        let op = LookupOp {
            op: new_operation::<Self>(ctx, 1, vec![], 0),
        };
        op.set_attr_lookup_name(ctx, StringAttr::new(name.to_string()));
        op
    }
}

impl YieldOp {
    pub fn new(ctx: &mut Context, value: Value) -> Self {
        YieldOp {
            op: new_operation::<Self>(ctx, 0, vec![value], 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use pliron::parsable::parse_from_str;

    use super::*;

    /// Prints `value` and parses it back.
    fn round_trip<T: Printable + Parsable<Arg = (), Parsed = T>>(value: &T) -> T {
        let ctx = &mut Context::new();
        let printed = value.disp(ctx).to_string();
        match parse_from_str(T::parser(()), ctx, &printed) {
            Ok(parsed) => parsed,
            Err(e) => panic!("cannot parse back {printed:?}: {}", e.disp(ctx)),
        }
    }

    #[test]
    fn floats_round_trip_exactly() {
        for x in [
            0.0,
            1.0,
            0.1,
            1500.0,
            1e308,
            1e-307,
            f64::MAX,
            f64::MIN_POSITIVE,
            1.0 / 3.0,
        ] {
            assert_eq!(round_trip(&NixFloat(x)), NixFloat(x), "{x}");
        }
    }

    #[test]
    fn floats_compare_by_bits() {
        assert_eq!(NixFloat(0.5), NixFloat(0.5));
        assert_ne!(NixFloat(0.0), NixFloat(-0.0));
        let set: HashSet<_> = [NixFloat(0.5), NixFloat(0.5)].into_iter().collect();
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn name_lists_round_trip() {
        let ctx = &Context::new();
        for names in [
            vec![],
            vec!["a".to_string()],
            vec!["a".into(), "b'".into(), "c-d".into()],
        ] {
            let names = NameList(names);
            assert_eq!(round_trip(&names), names);
        }
        let printed = NameList(vec!["a".into(), "b".into()]).disp(ctx).to_string();
        assert_eq!(printed, r#"["a", "b"]"#);
    }

    #[test]
    fn formals_specs_round_trip() {
        let ctx = &Context::new();
        let spec = FormalsSpec {
            formals: vec![
                ("a".into(), false),
                ("b'".into(), true),
                ("c-d".into(), false),
            ],
            ellipsis: true,
        };
        assert_eq!(spec.disp(ctx).to_string(), "{ a, b' ?, c-d, ... }");
        assert_eq!(round_trip(&spec), spec);
        for spec in [
            FormalsSpec {
                formals: vec![],
                ellipsis: false,
            },
            FormalsSpec {
                formals: vec![],
                ellipsis: true,
            },
            FormalsSpec {
                formals: vec![("x".into(), true)],
                ellipsis: false,
            },
        ] {
            assert_eq!(round_trip(&spec), spec);
        }
    }

    #[test]
    fn ops_declare_their_shape() {
        fn results_1<T: NResultsInterface<1>>() {}
        fn operands<const N: usize, T: NOpdsInterface<N>>() {}
        fn regions<const N: usize, T: NRegionsInterface<N> + SingleBlockRegionInterface>() {}

        results_1::<ConstOp>();
        operands::<0, ConstOp>();
        operands::<2, ApplyOp>();
        operands::<1, IfOp>();
        regions::<2, IfOp>();
        regions::<2, LetInOp>();
        regions::<1, LambdaOp>();
        regions::<1, WithOp>();
        regions::<1, AttrSetOp>();
        operands::<2, SelectOp>();
        operands::<1, UnaryOp>();
        operands::<2, BinaryOp>();
    }
}
