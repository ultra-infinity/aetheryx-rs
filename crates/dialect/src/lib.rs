//! Nix dialect data structures built on pliron, with parsing powered by rnix.
//!
//! This module defines enums and structs representing a Nix dialect for
//! pliron IR. It intentionally includes no trait implementations; consumers can
//! implement the pliron traits and lowering as they see fit.
use pliron::value::Value;
use pliron::region::Region;

// Dialect handle (no trait impl here; consumers may implement pliron::Dialect)
#[derive(Debug, Clone, Default)]
pub struct NixDialect;

// Operators and literals

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    String(String),
    Path(String),
    Uri(String),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttrKey(pub Vec<String>);

// Operation data structures (no trait impls provided)

pub struct ConstOp {
    pub lit: Literal,
    pub result: Value,
}

pub struct ListOp {
    pub elems: Vec<Value>,
    pub result: Value,
}

pub struct AttrSetOp {
    pub keys: Vec<AttrKey>,
    pub values: Vec<Value>,
    pub result: Value,
}

pub struct LambdaOp {
    pub param: String,
    pub body: Region,
    pub result: Value,
}

pub struct ApplyOp {
    pub callee: Value,
    pub arg: Value,
    pub result: Value,
}

pub struct IfOp {
    pub cond: Value,
    pub then_region: Region,
    pub else_region: Region,
    pub result: Value,
}

pub struct SelectOp {
    pub base: Value,
    pub path: AttrKey,
    pub default: Option<Value>,
    pub result: Value,
}

pub struct UnaryOp {
    pub op: UnOp,
    pub operand: Value,
    pub result: Value,
}

pub struct BinaryOp {
    pub op: BinOp,
    pub lhs: Value,
    pub rhs: Value,
    pub result: Value,
}

pub struct WithOp {
    pub base: Value,
    pub body: Region,
    pub result: Value,
}

pub struct LetInOp {
    pub bindings: Region,
    pub body: Region,
    pub result: Value,
}

