//! `nix_mir`: the mid-level Nix IR, the input to code generation.
//!
//! The HIR ([`crate::hir`]) leaves everything a machine needs implicit. The
//! MIR makes it explicit, so that lowering to the LLVM dialect is a direct
//! translation:
//!
//! | `nix_hir` (implicit)                        | `nix_mir` (explicit)                      |
//! |---------------------------------------------|-------------------------------------------|
//! | any value may be unevaluated                | [`ThunkType`] vs. [`ValueType`], `force`  |
//! | `var` / `lookup` resolve names at runtime   | SSA values; `with_lookup` with namespaces |
//! | lambda regions capture outer values freely  | isolated regions with explicit captures   |
//! | `let`, `rec`, default formals are recursive | `letrec` groups                           |
//! | attribute sets are regions of bindings      | construction ops                          |
//!
//! # Lowering to the LLVM dialect
//! Every op has a direct translation:
//! - Both types become `llvm.ptr`, a handle to a runtime value. The
//!   thunk/value distinction only exists at compile time; `evaluated` is free.
//! - `thunk`, `lambda` and `letrec` regions are isolated from above: they only
//!   use their block arguments, which receive the op's operands (the
//!   captures). Each region is outlined into an `llvm.func` taking its
//!   captures through an environment pointer; the op becomes a runtime call
//!   creating the thunk/closure from the function pointer and environment.
//! - `if` regions are not isolated: they become a `llvm.cond_br` CFG whose
//!   join block receives the yielded value.
//! - Every other op is one runtime `llvm.call` (string constants become
//!   `llvm.global`s).
//!
//! Source locations are copied from the HIR ops they are lowered from, so that
//! fallible operations can report positions at runtime.
//!
//! As in the HIR, attribute keys are prefixed (`mir_literal`, ...) because
//! pliron requires them to be unique across all ops.

use pliron::basic_block::BasicBlock;
use pliron::builtin::attributes::StringAttr;
use pliron::builtin::op_interfaces::{
    AllOperandsOfType, AllResultsOfType, IsTerminatorInterface, IsolatedFromAboveInterface,
    NOpdsInterface, NRegionsInterface, NResultsInterface, OperandNOfType, ResultNOfType,
    SingleBlockRegionInterface,
};
use pliron::common_traits::Verify;
use pliron::context::{Context, Ptr};
use pliron::derive::{pliron_op, pliron_type};
use pliron::linked_list::ContainsLinkedList;
use pliron::location::Located;
use pliron::op::Op;
use pliron::operation::Operation;
use pliron::region::Region;
use pliron::result::Result;
use pliron::r#type::{TypeHandle, Typed};
use pliron::value::{DefiningEntity, Value};
use pliron::verify_err;

use crate::hir::{BinOp, FormalsSpec, InterpKind, Literal, NameList, UnOp};

/// A value that may still be unevaluated (a thunk), as passed around lazily.
#[pliron_type(name = "nix_mir.thunk", format, generate_get = true, verifier = "succ")]
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct ThunkType;

/// A value in weak head normal form: the result of forcing a [`ThunkType`].
#[pliron_type(name = "nix_mir.value", format, generate_get = true, verifier = "succ")]
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct ValueType;

/// The two MIR types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Thunk,
    Value,
}

impl Ty {
    pub fn handle(self, ctx: &Context) -> TypeHandle {
        match self {
            Ty::Thunk => ThunkType::get(ctx).into(),
            Ty::Value => ValueType::get(ctx).into(),
        }
    }

    /// The MIR type of `value`, if it has one.
    pub fn of(ctx: &Context, value: Value) -> Option<Ty> {
        let ty = value.get_type(ctx);
        [Ty::Thunk, Ty::Value]
            .into_iter()
            .find(|t| t.handle(ctx) == ty)
    }
}

// ─── Laziness ───────────────────────────────────────────────────────────────

#[pliron_op(
    name = "nix_mir.force",
    // Evaluates a thunk to weak head normal form.
    format = "$0 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<1>,
        OperandNOfType<0, ThunkType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    verifier = "succ"
)]
pub struct ForceOp;

#[pliron_op(
    name = "nix_mir.evaluated",
    // An evaluated value used where a thunk is expected. Free at runtime.
    format = "$0 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<1>,
        OperandNOfType<0, ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ThunkType>,
    ],
    verifier = "succ"
)]
pub struct EvaluatedOp;

#[pliron_op(
    name = "nix_mir.thunk",
    // A suspended computation. The region's block arguments receive the
    // operands (captures); it yields the value.
    format = "`(` operands(CharSpace(`,`)) `) : ` type($0) ` ` region($0)",
    interfaces = [
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        IsolatedFromAboveInterface,
        NResultsInterface<1>,
        ResultNOfType<0, ThunkType>,
    ]
)]
pub struct ThunkOp;

#[pliron_op(
    name = "nix_mir.letrec",
    // N mutually recursive thunks. Region i computes thunk i; the block
    // arguments of every region are the N thunks followed by the captures.
    format = "`(` operands(CharSpace(`,`)) `) : ` types(CharSpace(`,`)) ` ` regions(CharSpace(`,`))",
    interfaces = [
        SingleBlockRegionInterface,
        IsolatedFromAboveInterface,
        AllResultsOfType<ThunkType>,
    ]
)]
pub struct LetrecOp;

// ─── Functions ──────────────────────────────────────────────────────────────

#[pliron_op(
    name = "nix_mir.lambda",
    // A closure. The region's block arguments are the argument (a thunk)
    // followed by the captures; it yields the result. The optional name is
    // the binding the lambda is defined in, which Nix reports in errors.
    format = "opt_attr($mir_lambda_name, $StringAttr, label($name), delimiters(`[`, `]`)) `(` operands(CharSpace(`,`)) `) : ` type($0) ` ` region($0)",
    interfaces = [
        NRegionsInterface<1>,
        SingleBlockRegionInterface,
        IsolatedFromAboveInterface,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    attributes = (mir_lambda_name: StringAttr)
)]
pub struct LambdaOp;

#[pliron_op(
    name = "nix_mir.call",
    // Applies a function to an argument and evaluates the result.
    format = "$0 ` ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        OperandNOfType<0, ValueType>,
        OperandNOfType<1, ThunkType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    verifier = "succ"
)]
pub struct CallOp;

#[pliron_op(
    name = "nix_mir.formals",
    // Checks an evaluated argument against a `{ a, b ? x, ... }` pattern and
    // returns it.
    format = "$0 ` ` attr($mir_formals, $FormalsSpec) ` : ` type($0)",
    interfaces = [
        NOpdsInterface<1>,
        OperandNOfType<0, ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    attributes = (mir_formals: FormalsSpec),
    verifier = "succ"
)]
pub struct FormalsOp;

// ─── Values ─────────────────────────────────────────────────────────────────

#[pliron_op(
    name = "nix_mir.const",
    format = "attr($mir_literal, $Literal) ` : ` type($0)",
    interfaces = [
        NOpdsInterface<0>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    attributes = (mir_literal: Literal),
    verifier = "succ"
)]
pub struct ConstOp;

#[pliron_op(
    name = "nix_mir.list",
    format = "`[` operands(CharSpace(`,`)) `] : ` type($0)",
    interfaces = [
        AllOperandsOfType<ThunkType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    verifier = "succ"
)]
pub struct ListOp;

#[pliron_op(
    name = "nix_mir.attrs",
    // An attribute set with static keys; operand i is the value of key i.
    format = "attr($mir_attr_names, $NameList) ` (` operands(CharSpace(`,`)) `) : ` type($0)",
    interfaces = [
        AllOperandsOfType<ThunkType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    attributes = (mir_attr_names: NameList)
)]
pub struct AttrsOp;

#[pliron_op(
    name = "nix_mir.attrs_insert",
    // set // { ${key} = value; }, for a dynamic key: a `null` key leaves the
    // set unchanged, an existing key is an error.
    format = "$0 ` [` $1 `] = ` $2 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<3>,
        OperandNOfType<0, ValueType>,
        OperandNOfType<1, ValueType>,
        OperandNOfType<2, ThunkType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    verifier = "succ"
)]
pub struct AttrsInsertOp;

#[pliron_op(
    name = "nix_mir.select",
    // set.key, without evaluating the attribute.
    format = "$0 ` . ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        AllOperandsOfType<ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ThunkType>,
    ],
    verifier = "succ"
)]
pub struct SelectOp;

#[pliron_op(
    name = "nix_mir.has_attr",
    format = "$0 ` ? ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        AllOperandsOfType<ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    verifier = "succ"
)]
pub struct HasAttrOp;

#[pliron_op(
    name = "nix_mir.with_lookup",
    // A name from the enclosing `with` namespaces, innermost first.
    format = "attr($mir_with_name, $StringAttr) ` (` operands(CharSpace(`,`)) `) : ` type($0)",
    interfaces = [
        AllOperandsOfType<ThunkType>,
        NResultsInterface<1>,
        ResultNOfType<0, ThunkType>,
    ],
    attributes = (mir_with_name: StringAttr),
    verifier = "succ"
)]
pub struct WithLookupOp;

// ─── Operators ──────────────────────────────────────────────────────────────

#[pliron_op(
    name = "nix_mir.binary",
    // A strict binary operator. `&&`, `||` and `->` are control flow in the
    // MIR (`if` + `check_bool`), so they are rejected here.
    format = "$0 ` ` attr($mir_binop, $BinOp) ` ` $1 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<2>,
        AllOperandsOfType<ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    attributes = (mir_binop: BinOp)
)]
pub struct BinaryOp;

#[pliron_op(
    name = "nix_mir.unary",
    format = "attr($mir_unop, $UnOp) ` ` $0 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<1>,
        OperandNOfType<0, ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    attributes = (mir_unop: UnOp),
    verifier = "succ"
)]
pub struct UnaryOp;

#[pliron_op(
    name = "nix_mir.check_bool",
    // Returns its operand, or fails with "expected a Boolean" if it is not one.
    format = "$0 ` : ` type($0)",
    interfaces = [
        NOpdsInterface<1>,
        OperandNOfType<0, ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    verifier = "succ"
)]
pub struct CheckBoolOp;

#[pliron_op(
    name = "nix_mir.interpolate",
    format = "attr($mir_interp_kind, $InterpKind) ` (` operands(CharSpace(`,`)) `) : ` type($0)",
    interfaces = [
        AllOperandsOfType<ValueType>,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ],
    attributes = (mir_interp_kind: InterpKind),
    verifier = "succ"
)]
pub struct InterpolateOp;

// ─── Control flow ───────────────────────────────────────────────────────────

#[pliron_op(
    name = "nix_mir.if",
    format = "$0 ` : ` type($0) ` then ` region($0) ` else ` region($1)",
    interfaces = [
        NOpdsInterface<1>,
        OperandNOfType<0, ValueType>,
        NRegionsInterface<2>,
        SingleBlockRegionInterface,
        NResultsInterface<1>,
        ResultNOfType<0, ValueType>,
    ]
)]
pub struct IfOp;

#[pliron_op(
    name = "nix_mir.assert",
    // Fails with "assertion failed" unless the operand is `true`.
    format = "$0",
    interfaces = [
        NOpdsInterface<1>,
        OperandNOfType<0, ValueType>,
        NResultsInterface<0>,
    ],
    verifier = "succ"
)]
pub struct AssertOp;

#[pliron_op(
    name = "nix_mir.yield",
    // Terminates a region with its value.
    format = "$0",
    interfaces = [
        IsTerminatorInterface,
        NOpdsInterface<1>,
        OperandNOfType<0, ValueType>,
        NResultsInterface<0>,
    ],
    verifier = "succ"
)]
pub struct YieldOp;

// ─── Construction ───────────────────────────────────────────────────────────

fn new_operation<T: Op>(
    ctx: &mut Context,
    results: &[Ty],
    operands: Vec<Value>,
    n_regions: usize,
) -> Ptr<Operation> {
    let results = results.iter().map(|t| t.handle(ctx)).collect();
    Operation::new(
        ctx,
        T::get_concrete_op_info(),
        results,
        operands,
        vec![],
        n_regions,
    )
}

/// Creates the block of region `idx` of `op` with arguments of the given types.
fn add_block(ctx: &mut Context, op: Ptr<Operation>, idx: usize, args: &[Ty]) -> Ptr<BasicBlock> {
    let args = args.iter().map(|t| t.handle(ctx)).collect();
    let block = BasicBlock::new(ctx, None, args);
    let region = op.deref(ctx).get_region(idx);
    block.insert_at_front(region, ctx);
    block
}

/// The single block of region `idx` of `op`.
fn region_block(ctx: &Context, op: Ptr<Operation>, idx: usize) -> Ptr<BasicBlock> {
    op.deref(ctx)
        .get_region(idx)
        .deref(ctx)
        .get_head()
        .expect("MIR regions have exactly one block")
}

/// Adds `value` as a capture of the isolated `op` (a `thunk`, `lambda` or
/// `letrec`): a new operand, and a new argument of every region's block.
/// Returns the arguments, one per region.
pub(crate) fn push_capture(ctx: &Context, op: Ptr<Operation>, value: Value) -> Vec<Value> {
    Operation::push_operand(op, ctx, value);
    let ty = value.get_type(ctx);
    let regions: Vec<_> = op.deref(ctx).regions().collect();
    regions
        .into_iter()
        .map(|region| {
            let block = region
                .deref(ctx)
                .get_head()
                .expect("MIR regions have exactly one block");
            let idx = BasicBlock::push_argument(block, ctx, ty);
            block.deref(ctx).get_argument(idx)
        })
        .collect()
}

/// The single result of a value-producing op.
pub fn result_of(ctx: &Context, op: &impl Op) -> Value {
    op.get_operation().deref(ctx).get_result(0)
}

macro_rules! simple_constructor {
    ($op:ident, $results:expr, ($($arg:ident),*)) => {
        impl $op {
            pub fn new(ctx: &mut Context, $($arg: Value),*) -> Self {
                $op { op: new_operation::<Self>(ctx, &$results, vec![$($arg),*], 0) }
            }
        }
    };
}

simple_constructor!(ForceOp, [Ty::Value], (thunk));
simple_constructor!(EvaluatedOp, [Ty::Thunk], (value));
simple_constructor!(CallOp, [Ty::Value], (func, arg));
simple_constructor!(AttrsInsertOp, [Ty::Value], (set, key, value));
simple_constructor!(SelectOp, [Ty::Thunk], (set, key));
simple_constructor!(HasAttrOp, [Ty::Value], (set, key));
simple_constructor!(CheckBoolOp, [Ty::Value], (value));
simple_constructor!(AssertOp, [], (cond));
simple_constructor!(YieldOp, [], (value));

impl ThunkOp {
    /// A thunk without captures; add them with [`Self::push_capture`].
    pub fn new(ctx: &mut Context) -> Self {
        let op = new_operation::<Self>(ctx, &[Ty::Thunk], vec![], 1);
        add_block(ctx, op, 0, &[]);
        ThunkOp { op }
    }

    pub fn body(&self, ctx: &Context) -> Ptr<BasicBlock> {
        region_block(ctx, self.op, 0)
    }

    /// Captures `value`; returns the block argument to use inside the body.
    pub fn push_capture(&self, ctx: &Context, value: Value) -> Value {
        push_capture(ctx, self.op, value)[0]
    }
}

impl LetrecOp {
    /// A group of `n` thunks without captures.
    pub fn new(ctx: &mut Context, n: usize) -> Self {
        let op = new_operation::<Self>(ctx, &vec![Ty::Thunk; n], vec![], n);
        for i in 0..n {
            add_block(ctx, op, i, &vec![Ty::Thunk; n]);
        }
        LetrecOp { op }
    }

    /// The number of thunks in the group.
    pub fn len(&self, ctx: &Context) -> usize {
        self.op.deref(ctx).get_num_results()
    }

    pub fn is_empty(&self, ctx: &Context) -> bool {
        self.len(ctx) == 0
    }

    /// The body computing thunk `i`.
    pub fn body(&self, ctx: &Context, i: usize) -> Ptr<BasicBlock> {
        region_block(ctx, self.op, i)
    }

    /// Captures `value`; returns the block argument of each body, in order.
    pub fn push_capture(&self, ctx: &Context, value: Value) -> Vec<Value> {
        push_capture(ctx, self.op, value)
    }
}

impl LambdaOp {
    /// A lambda without captures. Its body's first argument is the argument.
    pub fn new(ctx: &mut Context) -> Self {
        let op = new_operation::<Self>(ctx, &[Ty::Value], vec![], 1);
        add_block(ctx, op, 0, &[Ty::Thunk]);
        LambdaOp { op }
    }

    pub fn body(&self, ctx: &Context) -> Ptr<BasicBlock> {
        region_block(ctx, self.op, 0)
    }

    /// The argument, inside the body.
    pub fn argument(&self, ctx: &Context) -> Value {
        self.body(ctx).deref(ctx).get_argument(0)
    }

    /// Names the lambda after the binding it is defined in.
    pub fn set_name(&self, ctx: &Context, name: &str) {
        self.set_attr_mir_lambda_name(ctx, StringAttr::new(name.to_string()));
    }

    /// The name of the binding the lambda is defined in, if any.
    pub fn name(&self, ctx: &Context) -> Option<String> {
        self.get_attr_mir_lambda_name(ctx)
            .map(|name| name.as_str().to_string())
    }

    /// Captures `value`; returns the block argument to use inside the body.
    pub fn push_capture(&self, ctx: &Context, value: Value) -> Value {
        push_capture(ctx, self.op, value)[0]
    }
}

impl FormalsOp {
    pub fn new(ctx: &mut Context, arg: Value, spec: FormalsSpec) -> Self {
        let op = FormalsOp {
            op: new_operation::<Self>(ctx, &[Ty::Value], vec![arg], 0),
        };
        op.set_attr_mir_formals(ctx, spec);
        op
    }
}

impl ConstOp {
    pub fn new(ctx: &mut Context, literal: Literal) -> Self {
        let op = ConstOp {
            op: new_operation::<Self>(ctx, &[Ty::Value], vec![], 0),
        };
        op.set_attr_mir_literal(ctx, literal);
        op
    }
}

impl ListOp {
    pub fn new(ctx: &mut Context, elems: Vec<Value>) -> Self {
        ListOp {
            op: new_operation::<Self>(ctx, &[Ty::Value], elems, 0),
        }
    }
}

impl AttrsOp {
    /// `{ names[i] = values[i]; ... }`.
    pub fn new(ctx: &mut Context, names: Vec<String>, values: Vec<Value>) -> Self {
        let op = AttrsOp {
            op: new_operation::<Self>(ctx, &[Ty::Value], values, 0),
        };
        op.set_attr_mir_attr_names(ctx, NameList(names));
        op
    }
}

impl WithLookupOp {
    /// Looks up `name` in `namespaces`, innermost first.
    pub fn new(ctx: &mut Context, name: &str, namespaces: Vec<Value>) -> Self {
        let op = WithLookupOp {
            op: new_operation::<Self>(ctx, &[Ty::Thunk], namespaces, 0),
        };
        op.set_attr_mir_with_name(ctx, StringAttr::new(name.to_string()));
        op
    }
}

impl BinaryOp {
    pub fn new(ctx: &mut Context, binop: BinOp, lhs: Value, rhs: Value) -> Self {
        let op = BinaryOp {
            op: new_operation::<Self>(ctx, &[Ty::Value], vec![lhs, rhs], 0),
        };
        op.set_attr_mir_binop(ctx, binop);
        op
    }
}

impl UnaryOp {
    pub fn new(ctx: &mut Context, unop: UnOp, operand: Value) -> Self {
        let op = UnaryOp {
            op: new_operation::<Self>(ctx, &[Ty::Value], vec![operand], 0),
        };
        op.set_attr_mir_unop(ctx, unop);
        op
    }
}

impl InterpolateOp {
    pub fn new(ctx: &mut Context, kind: InterpKind, parts: Vec<Value>) -> Self {
        let op = InterpolateOp {
            op: new_operation::<Self>(ctx, &[Ty::Value], parts, 0),
        };
        op.set_attr_mir_interp_kind(ctx, kind);
        op
    }
}

impl IfOp {
    pub fn new(ctx: &mut Context, cond: Value) -> Self {
        let op = new_operation::<Self>(ctx, &[Ty::Value], vec![cond], 2);
        add_block(ctx, op, 0, &[]);
        add_block(ctx, op, 1, &[]);
        IfOp { op }
    }

    pub fn then_block(&self, ctx: &Context) -> Ptr<BasicBlock> {
        region_block(ctx, self.op, 0)
    }

    pub fn else_block(&self, ctx: &Context) -> Ptr<BasicBlock> {
        region_block(ctx, self.op, 1)
    }
}

// ─── Verification ───────────────────────────────────────────────────────────

/// Whether `block` is (transitively) nested in `region`.
fn block_is_in(ctx: &Context, block: Ptr<BasicBlock>, region: Ptr<Region>) -> bool {
    let mut block = block;
    loop {
        let Some(parent) = block.deref(ctx).get_parent_region() else {
            return false;
        };
        if parent == region {
            return true;
        }
        let Some(next) = parent.deref(ctx).get_parent_block(ctx) else {
            return false;
        };
        block = next;
    }
}

/// The block in which `value` is defined.
fn defining_block(ctx: &Context, value: Value) -> Option<Ptr<BasicBlock>> {
    match value.defining_entity() {
        DefiningEntity::Op(op) => op.deref(ctx).get_parent_block(),
        DefiningEntity::Block(block) => Some(block),
    }
}

/// Checks that no op nested in `region` uses a value defined outside it.
///
/// pliron's `IsolatedFromAboveInterface` does not check this itself.
fn verify_isolated(ctx: &Context, region: Ptr<Region>) -> Result<()> {
    fn walk(ctx: &Context, region: Ptr<Region>, inner: Ptr<Region>) -> Result<()> {
        for block in inner.deref(ctx).iter(ctx) {
            for nested in block.deref(ctx).iter(ctx) {
                let nested_op = nested.deref(ctx);
                for operand in nested_op.operands() {
                    let inside =
                        defining_block(ctx, operand).is_some_and(|b| block_is_in(ctx, b, region));
                    if !inside {
                        return verify_err!(
                            nested_op.loc(),
                            "{} uses a value defined outside its isolated region; \
                             it must be captured",
                            Operation::get_opid(nested, ctx)
                        );
                    }
                }
                for nested_region in nested_op.regions() {
                    walk(ctx, region, nested_region)?;
                }
            }
        }
        Ok(())
    }
    walk(ctx, region, region)
}

/// Checks that each region of `op` ends in a `nix_mir.yield`.
fn verify_yields(ctx: &Context, op: &impl Op) -> Result<()> {
    for region in op.get_operation().deref(ctx).regions() {
        let yields = region
            .deref(ctx)
            .get_head()
            .and_then(|b| b.deref(ctx).get_terminator(ctx))
            .is_some_and(|t| Operation::get_op::<YieldOp>(t, ctx).is_some());
        if !yields {
            return verify_err!(op.loc(ctx), "region must end in nix_mir.yield");
        }
    }
    Ok(())
}

/// Checks that the block arguments of every region of `op` have the types
/// `prefix` followed by the types of the op's operands (the captures).
fn verify_block_args(ctx: &Context, op: &impl Op, prefix: &[Ty]) -> Result<()> {
    let opr = op.get_operation().deref(ctx);
    let expected: Vec<TypeHandle> = prefix
        .iter()
        .map(|t| t.handle(ctx))
        .chain(opr.operands().map(|v| v.get_type(ctx)))
        .collect();
    for region in opr.regions() {
        let Some(block) = region.deref(ctx).get_head() else {
            return verify_err!(op.loc(ctx), "region has no block");
        };
        let actual: Vec<TypeHandle> = block
            .deref(ctx)
            .arguments()
            .map(|v| v.get_type(ctx))
            .collect();
        if actual != expected {
            return verify_err!(
                op.loc(ctx),
                "block arguments do not match the op's values and captures"
            );
        }
    }
    Ok(())
}

/// The checks shared by the isolated ops; `prefix` are the block arguments
/// that precede the captures.
fn verify_closure(ctx: &Context, op: &impl Op, prefix: &[Ty]) -> Result<()> {
    let opr = op.get_operation().deref(ctx);
    if opr.operands().any(|v| Ty::of(ctx, v).is_none()) {
        return verify_err!(op.loc(ctx), "captures must be MIR values");
    }
    verify_block_args(ctx, op, prefix)?;
    verify_yields(ctx, op)?;
    opr.regions()
        .try_for_each(|region| verify_isolated(ctx, region))
}

impl Verify for ThunkOp {
    fn verify(&self, ctx: &Context) -> Result<()> {
        verify_closure(ctx, self, &[])
    }
}

impl Verify for LambdaOp {
    fn verify(&self, ctx: &Context) -> Result<()> {
        verify_closure(ctx, self, &[Ty::Thunk])
    }
}

impl Verify for LetrecOp {
    fn verify(&self, ctx: &Context) -> Result<()> {
        let n = self.len(ctx);
        if n == 0 || self.op.deref(ctx).num_regions() != n {
            return verify_err!(
                self.loc(ctx),
                "nix_mir.letrec needs one region per result, and at least one"
            );
        }
        verify_closure(ctx, self, &vec![Ty::Thunk; n])
    }
}

impl Verify for IfOp {
    fn verify(&self, ctx: &Context) -> Result<()> {
        let has_args = self.op.deref(ctx).regions().any(|region| {
            region
                .deref(ctx)
                .get_head()
                .is_some_and(|block| block.deref(ctx).get_num_arguments() > 0)
        });
        if has_args {
            return verify_err!(self.loc(ctx), "nix_mir.if branches take no arguments");
        }
        verify_yields(ctx, self)
    }
}

impl Verify for AttrsOp {
    fn verify(&self, ctx: &Context) -> Result<()> {
        let Some(names) = self.get_attr_mir_attr_names(ctx).map(|n| n.0.clone()) else {
            return verify_err!(self.loc(ctx), "nix_mir.attrs has no names");
        };
        if names.len() != self.op.deref(ctx).get_num_operands() {
            return verify_err!(self.loc(ctx), "nix_mir.attrs needs one value per name");
        }
        let mut sorted = names;
        let len = sorted.len();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != len {
            return verify_err!(self.loc(ctx), "nix_mir.attrs has duplicate names");
        }
        Ok(())
    }
}

impl Verify for BinaryOp {
    fn verify(&self, ctx: &Context) -> Result<()> {
        match self.get_attr_mir_binop(ctx).map(|op| *op) {
            Some(BinOp::And | BinOp::Or | BinOp::Imply) => verify_err!(
                self.loc(ctx),
                "&&, || and -> are control flow in nix_mir (if + check_bool)"
            ),
            Some(_) => Ok(()),
            None => verify_err!(self.loc(ctx), "nix_mir.binary has no operator"),
        }
    }
}

#[cfg(test)]
mod tests {
    use pliron::builtin::ops::ModuleOp;
    use pliron::identifier::Identifier;
    use pliron::op::verify_op;
    use pliron::printable::Printable;

    use super::*;

    #[test]
    fn types_print_with_the_dialect_prefix() {
        let ctx = &Context::new();
        // pliron prints a trailing space after a type without parameters.
        let printed = |ty: Ty| ty.handle(ctx).disp(ctx).to_string();
        assert_eq!(printed(Ty::Thunk).trim_end(), "nix_mir.thunk");
        assert_eq!(printed(Ty::Value).trim_end(), "nix_mir.value");
    }

    /// A module whose body `f` fills.
    fn module(ctx: &mut Context, f: impl FnOnce(&mut Context, Ptr<BasicBlock>)) -> ModuleOp {
        let module = ModuleOp::new(ctx, Identifier::try_from("m").unwrap());
        let body = module.get_body(ctx, 0);
        f(ctx, body);
        module
    }

    fn append(ctx: &Context, block: Ptr<BasicBlock>, op: &impl Op) {
        op.get_operation().insert_at_back(block, ctx);
    }

    #[test]
    fn captured_values_verify() {
        let ctx = &mut Context::new();
        let m = module(ctx, |ctx, body| {
            let one = ConstOp::new(ctx, Literal::Int(1));
            append(ctx, body, &one);
            let thunk = ThunkOp::new(ctx);
            append(ctx, body, &thunk);
            let captured = thunk.push_capture(ctx, result_of(ctx, &one));
            let ret = YieldOp::new(ctx, captured);
            append(ctx, thunk.body(ctx), &ret);
        });
        let res = verify_op(&m, ctx);
        assert!(res.is_ok(), "{}", m.get_operation().disp(ctx));
    }

    #[test]
    fn implicit_captures_are_rejected() {
        let ctx = &mut Context::new();
        let m = module(ctx, |ctx, body| {
            let one = ConstOp::new(ctx, Literal::Int(1));
            append(ctx, body, &one);
            let thunk = ThunkOp::new(ctx);
            append(ctx, body, &thunk);
            // Uses `one` directly instead of capturing it.
            let ret = YieldOp::new(ctx, result_of(ctx, &one));
            append(ctx, thunk.body(ctx), &ret);
        });
        let err = verify_op(&m, ctx).expect_err("an implicit capture must not verify");
        assert!(err.disp(ctx).to_string().contains("must be captured"));
    }

    #[test]
    fn lazy_boolean_operators_are_rejected() {
        let ctx = &mut Context::new();
        let m = module(ctx, |ctx, body| {
            let t = ConstOp::new(ctx, Literal::Bool(true));
            append(ctx, body, &t);
            let v = result_of(ctx, &t);
            let and = BinaryOp::new(ctx, BinOp::And, v, v);
            append(ctx, body, &and);
        });
        assert!(verify_op(&m, ctx).is_err());
    }

    #[test]
    fn letrec_bodies_see_the_group() {
        let ctx = &mut Context::new();
        let m = module(ctx, |ctx, body| {
            let group = LetrecOp::new(ctx, 2);
            append(ctx, body, &group);
            for i in 0..2 {
                let block = group.body(ctx, i);
                // Thunk i forces thunk 1 - i.
                let other = block.deref(ctx).get_argument(1 - i);
                let force = ForceOp::new(ctx, other);
                append(ctx, block, &force);
                let ret = YieldOp::new(ctx, result_of(ctx, &force));
                append(ctx, block, &ret);
            }
        });
        let res = verify_op(&m, ctx);
        assert!(res.is_ok(), "{}", m.get_operation().disp(ctx));
    }
}
