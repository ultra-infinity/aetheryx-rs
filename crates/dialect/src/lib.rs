//! Nix dialect data structures built on pliron, with parsing powered by rnix.
//!
//! This module defines the types, attributes and operations of a Nix dialect
//! for pliron IR. Lowering from the rnix AST lives in [`lower`].
//!
//! Every SSA value in the dialect has type `nix.value` ([`ValueType`]) and denotes a (possibly
//! lazy) Nix value. Ops that must only evaluate a sub-expression conditionally
//! (`nix.if`, `nix.assert`, ...) or that introduce a scope (`nix.let_in`,
//! `nix.with`, `nix.lambda`, ...) carry it in a single-block region that is
//! terminated by `nix.yield`.
use pliron::basic_block::BasicBlock;
use pliron::builtin::attributes::{BoolAttr, StringAttr, VecAttr};
use pliron::builtin::op_interfaces::{
    IsTerminatorInterface, NOpdsInterface, NRegionsInterface, NResultsInterface,
    NoTerminatorInterface, SingleBlockRegionInterface,
};
use pliron::context::{Context, Ptr};
use pliron::derive::{pliron_attr, pliron_op, pliron_type};
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::value::Value;

pub mod builtins_dialect;
pub mod diagnostics;
pub mod lower;
pub mod paths;

// Dialect handle (no trait impl here; consumers may implement pliron::Dialect)
#[derive(Debug, Clone, Default)]
pub struct NixDialect;

/// The type of every Nix value in the IR.
#[pliron_type(name = "nix.value", format, generate_get = true, verifier = "succ")]
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct ValueType;

// Operators and literals

#[pliron_attr(name = "nix.binop", format, verifier = "succ")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    ConcatLists, // ++
    MergeAttrs,  // //
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Imply, // ->
}

#[pliron_attr(name = "nix.unop", format, verifier = "succ")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    Not,
    Neg,
}

#[pliron_attr(name = "nix.literal_kind", format, verifier = "succ")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LiteralKind {
    Number(String),
    String(String),
    Path(String),
    Uri(String),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttrKey(pub Vec<String>);

// Operation data structures (no trait impls provided)
#[pliron_op(
    name = "nix.const",
    format = "attr($kind, $LiteralKind) ` : ` type($0)",
    verifier = "succ",
    attributes = (kind: LiteralKind)
)]
pub struct ConstOp;

#[pliron_op(
    name = "nix.list",
    // [elem0 elem1 ...]
    format = "`[` operands(CharSpace(` `)) `]`",
    interfaces = [
        NResultsInterface<1>
    ],
    verifier = "succ"
)]
pub struct ListOp;

#[pliron_op(
    name = "nix.attr_set",
    // { attr1 = ...; attr2 = ...; }
    // Represented as a single-block region containing binding ops (no terminator).
    format = "region($0)",
    interfaces = [
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NoTerminatorInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct AttrSetOp;

#[pliron_op(
    name = "nix.lambda",
    // A lambda with a single-block body region.
    format = "region($0)",
    interfaces = [
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct LambdaOp;

#[pliron_op(
    name = "nix.apply",
    // Function application: f x
    format = "$0 ` ` $1",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct ApplyOp;

#[pliron_op(name = "nix.if",
    format = "$0 ` then ` region($0) ` else ` region($1)",
    interfaces = [
        NOpdsInterface<1>,
        NRegionsInterface<2>,
        NResultsInterface<1>,
        SingleBlockRegionInterface
    ],
    verifier = "succ",
)]
pub struct IfOp;

#[pliron_op(
    name = "nix.select",
    // Attribute selection: base.path
    // Path may be provided as an operand value.
    format = "$0 ` . ` $1",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct SelectOp;

#[pliron_op(
    name = "nix.unary",
    // Unary operation on a single operand; the operator is the `kind` attribute.
    format = "attr($unop, $UnOp) ` ` $0",
    interfaces = [
        NOpdsInterface<1>,
        NResultsInterface<1>,
    ],
    attributes = (unop: UnOp),
    verifier = "succ"
)]
pub struct UnaryOp;

#[pliron_op(
    name = "nix.binary",
    // Binary operation on two operands; the operator is the `kind` attribute.
    format = "$0 ` ` attr($binop, $BinOp) ` ` $1",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    attributes = (binop: BinOp),
    verifier = "succ"
)]
pub struct BinaryOp;

#[pliron_op(
    name = "nix.with",
    // with <scope>; <body>
    format = "`with ` $0 ` ; ` region($0)",
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
    name = "nix.let_in",
    // let <bindings> in <body>
    // Region 0 holds binding ops (no terminator); region 1 ends in `nix.yield`.
    format = "`let ` region($0) ` in ` region($1)",
    interfaces = [
        NRegionsInterface<2>,
        SingleBlockRegionInterface,
        NoTerminatorInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct LetInOp;

#[pliron_op(
    name = "nix.rec_attr_set",
    // rec { attr1 = ...; attr2 = ...; }
    // Bindings in the region may refer to each other through `nix.var`.
    format = "region($0)",
    interfaces = [
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        NoTerminatorInterface,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct RecAttrSetOp;

#[pliron_op(
    name = "nix.bind",
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
    name = "nix.dyn_bind",
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
    name = "nix.var",
    // Reference to a name bound by an enclosing `nix.let_in` / `nix.rec_attr_set`.
    format = "attr($var_name, $StringAttr)",
    interfaces = [
        NOpdsInterface<0>,
        NResultsInterface<1>,
    ],
    attributes = (var_name: StringAttr),
    verifier = "succ"
)]
pub struct VarOp;

#[pliron_op(
    name = "nix.lookup",
    // Reference to a name that is neither bound lexically nor a global builtin:
    // resolved through the enclosing `nix.with` scopes, innermost first. Only
    // emitted inside a `nix.with` (otherwise lowering reports an error).
    format = "attr($lookup_name, $StringAttr)",
    interfaces = [
        NOpdsInterface<0>,
        NResultsInterface<1>,
    ],
    attributes = (lookup_name: StringAttr),
    verifier = "succ"
)]
pub struct LookupOp;

#[pliron_op(
    name = "nix.has_attr",
    // base ? key
    format = "$0 ` ? ` $1",
    interfaces = [
        NOpdsInterface<2>,
        NResultsInterface<1>,
    ],
    verifier = "succ"
)]
pub struct HasAttrOp;

#[pliron_attr(name = "nix.interp_kind", format, verifier = "succ")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterpKind {
    String,
    Path,
}

#[pliron_op(
    name = "nix.interpolate",
    // "a${b}c" / ./a/${b}: concatenation of the stringified operands.
    format = "attr($interp_kind, $InterpKind) ` (` operands(CharSpace(`,`)) `)`",
    interfaces = [
        NResultsInterface<1>,
    ],
    attributes = (interp_kind: InterpKind),
    verifier = "succ"
)]
pub struct InterpolateOp;

#[pliron_op(
    name = "nix.assert",
    // assert <cond>; <body>
    format = "$0 ` ; ` region($0)",
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
    name = "nix.formals",
    // Checks a lambda argument against a `{ a, b ? x, ... }` pattern and
    // returns it unchanged.
    format = "$0 ` ` attr($names, $VecAttr) ` ellipsis = ` attr($ellipsis, $BoolAttr)",
    interfaces = [
        NOpdsInterface<1>,
        NResultsInterface<1>,
    ],
    attributes = (names: VecAttr, ellipsis: BoolAttr),
    verifier = "succ"
)]
pub struct FormalsOp;

#[pliron_op(
    name = "nix.yield",
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

// Constructors

/// Creates an operation whose results are all of type `nix.value`.
fn new_operation<T: Op>(
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

/// Creates the (single) entry block of region `idx` of `op`.
pub fn add_region_block(ctx: &mut Context, op: Ptr<Operation>, idx: usize) -> Ptr<BasicBlock> {
    add_region_block_with_args(ctx, op, idx, 0)
}

/// Creates the (single) entry block of region `idx` of `op`, with `n_args`
/// block arguments of type `nix.value`.
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
    pub fn new(ctx: &mut Context, kind: LiteralKind) -> Self {
        let op = ConstOp { op: new_operation::<Self>(ctx, 1, vec![], 0) };
        op.set_attr_kind(ctx, kind);
        op
    }
}

impl ListOp {
    pub fn new(ctx: &mut Context, elems: Vec<Value>) -> Self {
        ListOp { op: new_operation::<Self>(ctx, 1, elems, 0) }
    }
}

impl AttrSetOp {
    pub fn new(ctx: &mut Context) -> Self {
        AttrSetOp { op: new_operation::<Self>(ctx, 1, vec![], 1) }
    }
}

impl RecAttrSetOp {
    pub fn new(ctx: &mut Context) -> Self {
        RecAttrSetOp { op: new_operation::<Self>(ctx, 1, vec![], 1) }
    }
}

impl LambdaOp {
    pub fn new(ctx: &mut Context) -> Self {
        LambdaOp { op: new_operation::<Self>(ctx, 1, vec![], 1) }
    }
}

impl ApplyOp {
    pub fn new(ctx: &mut Context, func: Value, arg: Value) -> Self {
        ApplyOp { op: new_operation::<Self>(ctx, 1, vec![func, arg], 0) }
    }
}

impl IfOp {
    pub fn new(ctx: &mut Context, cond: Value) -> Self {
        IfOp { op: new_operation::<Self>(ctx, 1, vec![cond], 2) }
    }
}

impl SelectOp {
    pub fn new(ctx: &mut Context, base: Value, key: Value) -> Self {
        SelectOp { op: new_operation::<Self>(ctx, 1, vec![base, key], 0) }
    }
}

impl HasAttrOp {
    pub fn new(ctx: &mut Context, base: Value, key: Value) -> Self {
        HasAttrOp { op: new_operation::<Self>(ctx, 1, vec![base, key], 0) }
    }
}

impl UnaryOp {
    pub fn new(ctx: &mut Context, kind: UnOp, operand: Value) -> Self {
        let op = UnaryOp { op: new_operation::<Self>(ctx, 1, vec![operand], 0) };
        op.set_attr_unop(ctx, kind);
        op
    }
}

impl BinaryOp {
    pub fn new(ctx: &mut Context, kind: BinOp, lhs: Value, rhs: Value) -> Self {
        let op = BinaryOp { op: new_operation::<Self>(ctx, 1, vec![lhs, rhs], 0) };
        op.set_attr_binop(ctx, kind);
        op
    }
}

impl WithOp {
    pub fn new(ctx: &mut Context, scope: Value) -> Self {
        WithOp { op: new_operation::<Self>(ctx, 1, vec![scope], 1) }
    }
}

impl LetInOp {
    pub fn new(ctx: &mut Context) -> Self {
        LetInOp { op: new_operation::<Self>(ctx, 1, vec![], 2) }
    }
}

impl BindOp {
    pub fn new(ctx: &mut Context, name: &str, value: Value) -> Self {
        let op = BindOp { op: new_operation::<Self>(ctx, 0, vec![value], 0) };
        op.set_attr_bind_name(ctx, StringAttr::new(name.to_string()));
        op
    }
}

impl DynBindOp {
    pub fn new(ctx: &mut Context, key: Value, value: Value) -> Self {
        DynBindOp { op: new_operation::<Self>(ctx, 0, vec![key, value], 0) }
    }
}

impl VarOp {
    pub fn new(ctx: &mut Context, name: &str) -> Self {
        let op = VarOp { op: new_operation::<Self>(ctx, 1, vec![], 0) };
        op.set_attr_var_name(ctx, StringAttr::new(name.to_string()));
        op
    }
}

impl LookupOp {
    pub fn new(ctx: &mut Context, name: &str) -> Self {
        let op = LookupOp { op: new_operation::<Self>(ctx, 1, vec![], 0) };
        op.set_attr_lookup_name(ctx, StringAttr::new(name.to_string()));
        op
    }
}

impl InterpolateOp {
    pub fn new(ctx: &mut Context, kind: InterpKind, parts: Vec<Value>) -> Self {
        let op = InterpolateOp { op: new_operation::<Self>(ctx, 1, parts, 0) };
        op.set_attr_interp_kind(ctx, kind);
        op
    }
}

impl AssertOp {
    pub fn new(ctx: &mut Context, cond: Value) -> Self {
        AssertOp { op: new_operation::<Self>(ctx, 1, vec![cond], 1) }
    }
}

impl FormalsOp {
    pub fn new(ctx: &mut Context, arg: Value, names: &[String], ellipsis: bool) -> Self {
        let op = FormalsOp { op: new_operation::<Self>(ctx, 1, vec![arg], 0) };
        let names = names
            .iter()
            .map(|n| -> pliron::attribute::AttrObj { Box::new(StringAttr::new(n.clone())) })
            .collect();
        op.set_attr_names(ctx, VecAttr::new(names));
        op.set_attr_ellipsis(ctx, BoolAttr::new(ellipsis));
        op
    }
}

impl YieldOp {
    pub fn new(ctx: &mut Context, value: Value) -> Self {
        YieldOp { op: new_operation::<Self>(ctx, 0, vec![value], 0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use pliron::builtin::op_interfaces::{
        NOpdsInterface, NRegionsInterface, NResultsInterface, SingleBlockRegionInterface,
    };

    // Helper to assert trait impls at compile-time.
    fn assert_impl_nresults_1<T: NResultsInterface<1>>() {}
    fn assert_impl_nopds_1<T: NOpdsInterface<1>>() {}
    fn assert_impl_nopds_2<T: NOpdsInterface<2>>() {}
    fn assert_impl_nregions_1<T: NRegionsInterface<1>>() {}
    fn assert_impl_nregions_2<T: NRegionsInterface<2>>() {}
    fn assert_impl_single_block<T: SingleBlockRegionInterface>() {}

    #[test]
    fn test_literal_kind_equality_and_hash() {
        let a = LiteralKind::Number("42".to_string());
        let b = LiteralKind::Number("42".to_string());
        let c = LiteralKind::String("42".to_string());

        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }

    #[test]
    fn test_attr_key_equality_and_hash() {
        let k1 = AttrKey(vec!["x".to_string(), "y".to_string()]);
        let k2 = AttrKey(vec!["x".to_string(), "y".to_string()]);
        let k3 = AttrKey(vec!["x".to_string(), "z".to_string()]);
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
        let mut set = HashSet::new();
        set.insert(k1);
        assert!(set.contains(&k2));
        assert!(!set.contains(&k3));
    }

    #[test]
    fn test_ifop_interfaces_present() {
        // IfOp: 1 operand, 2 regions (single block), 1 result
        assert_impl_nopds_1::<IfOp>();
        assert_impl_nregions_2::<IfOp>();
        assert_impl_single_block::<IfOp>();
        assert_impl_nresults_1::<IfOp>();
    }

    #[test]
    fn test_listop_interfaces_present() {
        // ListOp: 1 result
        assert_impl_nresults_1::<ListOp>();
    }

    #[test]
    fn test_attrsetop_interfaces_present() {
        // AttrSetOp: 1 single-block region, 1 result
        assert_impl_nregions_1::<AttrSetOp>();
        assert_impl_single_block::<AttrSetOp>();
        assert_impl_nresults_1::<AttrSetOp>();
    }

    #[test]
    fn test_lambdaop_interfaces_present() {
        // LambdaOp: 1 single-block region, 1 result
        assert_impl_nregions_1::<LambdaOp>();
        assert_impl_single_block::<LambdaOp>();
        assert_impl_nresults_1::<LambdaOp>();
    }

    #[test]
    fn test_applyop_interfaces_present() {
        // ApplyOp: 2 operands, 1 result
        assert_impl_nopds_2::<ApplyOp>();
        assert_impl_nresults_1::<ApplyOp>();
    }

    #[test]
    fn test_selectop_interfaces_present() {
        // SelectOp: 2 operands, 1 result
        assert_impl_nopds_2::<SelectOp>();
        assert_impl_nresults_1::<SelectOp>();
    }

    #[test]
    fn test_unaryop_interfaces_present() {
        // UnaryOp: 1 operand, 1 result
        assert_impl_nopds_1::<UnaryOp>();
        assert_impl_nresults_1::<UnaryOp>();
    }

    #[test]
    fn test_binaryop_interfaces_present() {
        // BinaryOp: 2 operands, 1 result
        assert_impl_nopds_2::<BinaryOp>();
        assert_impl_nresults_1::<BinaryOp>();
    }

    #[test]
    fn test_withop_interfaces_present() {
        // WithOp: 1 operand, 1 single-block region, 1 result
        assert_impl_nopds_1::<WithOp>();
        assert_impl_nregions_1::<WithOp>();
        assert_impl_single_block::<WithOp>();
        assert_impl_nresults_1::<WithOp>();
    }

    #[test]
    fn test_letinop_interfaces_present() {
        // LetInOp: 2 regions (single block), 1 result
        assert_impl_nregions_2::<LetInOp>();
        assert_impl_single_block::<LetInOp>();
        assert_impl_nresults_1::<LetInOp>();
    }
}

