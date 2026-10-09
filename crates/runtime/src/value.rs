//! The runtime value model.
//!
//! A [`Value`] is a single shared pointer, so that compiled code can pass it
//! around as an `llvm.ptr`. Like Nix's own values, it is either unevaluated
//! (a thunk) or evaluated; forcing a thunk overwrites it with its result, so
//! every holder of the pointer sees the evaluated value afterwards. This is
//! why `nix_mir.evaluated` (value to thunk) is free.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::error::{Error, Result};

/// A Nix value, evaluated or not.
#[derive(Clone)]
pub struct Value(Rc<RefCell<Repr>>);

enum Repr {
    /// Not evaluated yet.
    Pending(Box<dyn FnOnce() -> Result<Value>>),
    /// Being evaluated: forcing it again means infinite recursion.
    Blackhole,
    /// Evaluation failed; forcing it again fails the same way.
    Failed(Error),
    Ready(Data),
}

/// An evaluated value (weak head normal form).
#[derive(Clone)]
pub enum Data {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(Rc<str>),
    /// An absolute, normalized path.
    Path(Rc<str>),
    List(Rc<[Value]>),
    Attrs(Rc<Attrs>),
    Lambda(Rc<Lambda>),
    PrimOp(Rc<PrimOpApp>),
}

/// The attributes of a set, ordered by name as Nix orders them.
pub type Attrs = BTreeMap<Rc<str>, Value>;

/// A closure.
pub struct Lambda {
    /// The name Nix reports in errors: the attribute or `let` binding the
    /// lambda was defined in, if any.
    pub name: Option<Rc<str>>,
    pub body: Box<dyn Fn(Value) -> Result<Value>>,
}

/// The implementation of a primop, called with all its arguments.
pub type PrimOpFn = fn(&[Value]) -> Result<Value>;

/// A native function of the given arity.
pub struct PrimOp {
    pub name: &'static str,
    pub arity: usize,
    /// `None` for builtins that are not implemented yet.
    pub func: Option<PrimOpFn>,
}

/// A primop applied to fewer arguments than its arity.
pub struct PrimOpApp {
    pub op: &'static PrimOp,
    pub args: Vec<Value>,
}

impl Value {
    /// An evaluated value.
    pub fn ready(data: Data) -> Self {
        Value(Rc::new(RefCell::new(Repr::Ready(data))))
    }

    /// A thunk computing `f()` when first forced.
    pub fn thunk(f: impl FnOnce() -> Result<Value> + 'static) -> Self {
        Value(Rc::new(RefCell::new(Repr::Pending(Box::new(f)))))
    }

    /// A thunk to be defined later with [`Self::define`]; used for groups of
    /// mutually recursive thunks (`nix_mir.letrec`).
    pub fn undefined() -> Self {
        Value(Rc::new(RefCell::new(Repr::Blackhole)))
    }

    /// Sets the computation of a thunk created by [`Self::undefined`].
    pub fn define(&self, f: impl FnOnce() -> Result<Value> + 'static) {
        *self.0.borrow_mut() = Repr::Pending(Box::new(f));
    }

    pub fn null() -> Self {
        Self::ready(Data::Null)
    }

    pub fn bool(b: bool) -> Self {
        Self::ready(Data::Bool(b))
    }

    pub fn int(i: i64) -> Self {
        Self::ready(Data::Int(i))
    }

    pub fn float(f: f64) -> Self {
        Self::ready(Data::Float(f))
    }

    pub fn string(s: impl Into<Rc<str>>) -> Self {
        Self::ready(Data::String(s.into()))
    }

    pub fn path(p: impl Into<Rc<str>>) -> Self {
        Self::ready(Data::Path(p.into()))
    }

    pub fn list(items: impl Into<Rc<[Value]>>) -> Self {
        Self::ready(Data::List(items.into()))
    }

    pub fn attrs(attrs: Attrs) -> Self {
        Self::ready(Data::Attrs(Rc::new(attrs)))
    }

    pub fn lambda(name: Option<Rc<str>>, body: impl Fn(Value) -> Result<Value> + 'static) -> Self {
        Self::ready(Data::Lambda(Rc::new(Lambda {
            name,
            body: Box::new(body),
        })))
    }

    pub fn primop(op: &'static PrimOp) -> Self {
        Self::ready(Data::PrimOp(Rc::new(PrimOpApp {
            op,
            args: Vec::new(),
        })))
    }

    /// Evaluates the value to weak head normal form.
    pub fn force(&self) -> Result<Data> {
        if let Some(data) = self.peek() {
            return Ok(data);
        }
        crate::stack::check()?;
        let pending = {
            let mut repr = self.0.borrow_mut();
            match &*repr {
                Repr::Ready(data) => return Ok(data.clone()),
                Repr::Failed(error) => return Err(error.clone()),
                Repr::Blackhole => return Err(Error::new("infinite recursion encountered")),
                Repr::Pending(_) => {}
            }
            match std::mem::replace(&mut *repr, Repr::Blackhole) {
                Repr::Pending(f) => f,
                _ => unreachable!("checked above"),
            }
        };
        match pending().and_then(|value| value.force()) {
            Ok(data) => {
                *self.0.borrow_mut() = Repr::Ready(data.clone());
                Ok(data)
            }
            Err(error) => {
                *self.0.borrow_mut() = Repr::Failed(error.clone());
                Err(error)
            }
        }
    }

    /// The value if it is already evaluated, without evaluating it.
    pub fn peek(&self) -> Option<Data> {
        match &*self.0.borrow() {
            Repr::Ready(data) => Some(data.clone()),
            _ => None,
        }
    }

    /// Whether `self` and `other` are the same object.
    pub fn ptr_eq(&self, other: &Value) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0.borrow() {
            Repr::Pending(_) => write!(f, "<thunk>"),
            Repr::Blackhole => write!(f, "<blackhole>"),
            Repr::Failed(e) => write!(f, "<failed: {}>", e.message),
            Repr::Ready(data) => write!(f, "{}", data.type_name()),
        }
    }
}

impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::print::print_for_error(self))
    }
}

impl Data {
    /// How Nix names the type of this value in error messages.
    pub fn type_name(&self) -> String {
        match self {
            Data::Null => "null".into(),
            Data::Bool(_) => "a Boolean".into(),
            Data::Int(_) => "an integer".into(),
            Data::Float(_) => "a float".into(),
            Data::String(_) => "a string".into(),
            Data::Path(_) => "a path".into(),
            Data::List(_) => "a list".into(),
            Data::Attrs(_) => "a set".into(),
            Data::Lambda(_) => "a function".into(),
            Data::PrimOp(app) if app.args.is_empty() => {
                format!("the built-in function '{}'", app.op.name)
            }
            Data::PrimOp(app) => {
                format!("the partially applied built-in function '{}'", app.op.name)
            }
        }
    }

    /// The result of `builtins.typeOf`.
    pub fn type_of(&self) -> &'static str {
        match self {
            Data::Null => "null",
            Data::Bool(_) => "bool",
            Data::Int(_) => "int",
            Data::Float(_) => "float",
            Data::String(_) => "string",
            Data::Path(_) => "path",
            Data::List(_) => "list",
            Data::Attrs(_) => "set",
            Data::Lambda(_) | Data::PrimOp(_) => "lambda",
        }
    }
}
