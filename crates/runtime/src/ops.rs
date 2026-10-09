//! The semantics of the `nix_mir` operations.
//!
//! Evaluation order and error messages follow Nix 2.28; every message here
//! was checked against `nix-instantiate`.

use std::cmp::Ordering;
use std::rc::Rc;

use crate::error::{Error, Result};
use crate::print::print_for_error;
use crate::value::{Attrs, Data, PrimOpApp, Value};

/// `expected <expected> but found <type>: <value>`.
pub fn type_error(expected: &str, found: &Data) -> Error {
    Error::new(format!(
        "expected {expected} but found {}: {}",
        found.type_name(),
        print_for_error(found)
    ))
}

pub fn expect_bool(data: &Data) -> Result<bool> {
    match data {
        Data::Bool(b) => Ok(*b),
        other => Err(type_error("a Boolean", other)),
    }
}

pub fn expect_int(data: &Data) -> Result<i64> {
    match data {
        Data::Int(i) => Ok(*i),
        other => Err(type_error("an integer", other)),
    }
}

/// An integer or float as a float.
pub fn expect_float(data: &Data) -> Result<f64> {
    match data {
        Data::Int(i) => Ok(*i as f64),
        Data::Float(f) => Ok(*f),
        other => Err(type_error("a float", other)),
    }
}

pub fn expect_string(data: &Data) -> Result<Rc<str>> {
    match data {
        Data::String(s) => Ok(s.clone()),
        other => Err(type_error("a string", other)),
    }
}

pub fn expect_list(data: &Data) -> Result<Rc<[Value]>> {
    match data {
        Data::List(items) => Ok(items.clone()),
        other => Err(type_error("a list", other)),
    }
}

pub fn expect_attrs(data: &Data) -> Result<Rc<Attrs>> {
    match data {
        Data::Attrs(attrs) => Ok(attrs.clone()),
        other => Err(type_error("a set", other)),
    }
}

/// Whether `data` can be called (a lambda, primop, or set with `__functor`).
pub fn is_function(data: &Data) -> bool {
    match data {
        Data::Lambda(_) | Data::PrimOp(_) => true,
        Data::Attrs(attrs) => attrs.contains_key("__functor"),
        _ => false,
    }
}

/// Fails unless `data` can be called, like Nix's `forceFunction`.
pub fn expect_function(data: &Data) -> Result<()> {
    if is_function(data) {
        Ok(())
    } else {
        Err(type_error("a function", data))
    }
}

// ─── Functions ──────────────────────────────────────────────────────────────

/// How deeply calls may nest, like Nix's default `max-call-depth`.
pub const MAX_CALL_DEPTH: usize = 10_000;

thread_local! {
    static CALL_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts one level of call nesting while alive.
struct CallDepthGuard;

impl CallDepthGuard {
    fn enter() -> Result<Self> {
        CALL_DEPTH.with(|depth| {
            if depth.get() >= MAX_CALL_DEPTH {
                return Err(Error::new("stack overflow; max-call-depth exceeded"));
            }
            depth.set(depth.get() + 1);
            Ok(CallDepthGuard)
        })
    }
}

impl Drop for CallDepthGuard {
    fn drop(&mut self) {
        CALL_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

/// Calls `func` with `arg`. The result may still be unevaluated.
///
/// Fails with Nix's "stack overflow; max-call-depth exceeded" when calls nest
/// more than [`MAX_CALL_DEPTH`] deep.
pub fn call(func: &Data, arg: Value) -> Result<Value> {
    let _depth = CallDepthGuard::enter()?;
    match func {
        Data::Lambda(lambda) => (lambda.body)(arg),
        Data::PrimOp(app) => {
            let mut args = app.args.clone();
            args.push(arg);
            if args.len() == app.op.arity {
                match app.op.func {
                    Some(func) => func(&args),
                    None => Err(Error::new(format!(
                        "builtin '{}' is not supported yet",
                        app.op.name
                    ))),
                }
            } else {
                Ok(Value::ready(Data::PrimOp(Rc::new(PrimOpApp {
                    op: app.op,
                    args,
                }))))
            }
        }
        Data::Attrs(attrs) if attrs.contains_key("__functor") => {
            // `f x` is `f.__functor f x`.
            let functor = attrs["__functor"].force()?;
            let partial = call(&functor, Value::ready(func.clone()))?.force()?;
            call(&partial, arg)
        }
        other => Err(Error::new(format!(
            "attempt to call something which is not a function but {}: {}",
            other.type_name(),
            print_for_error(other)
        ))),
    }
}

/// Checks the argument of a `{ a, b ? x, ... }` lambda.
///
/// Missing arguments are reported in declaration order. Nix reports
/// unexpected arguments in its internal symbol order, which is not
/// reproducible; this reports the first in name order.
pub fn check_formals(
    arg: &Data,
    formals: &[(String, bool)],
    ellipsis: bool,
    lambda_name: Option<&str>,
) -> Result<()> {
    let attrs = expect_attrs(arg)?;
    let name = lambda_name.unwrap_or("anonymous lambda");
    for (formal, has_default) in formals {
        if !has_default && !attrs.contains_key(formal.as_str()) {
            return Err(Error::new(format!(
                "function '{name}' called without required argument '{formal}'"
            )));
        }
    }
    if !ellipsis
        && let Some(unexpected) = attrs
            .keys()
            .find(|key| !formals.iter().any(|(f, _)| f.as_str() == &***key))
    {
        return Err(Error::new(format!(
            "function '{name}' called with unexpected argument '{unexpected}'"
        )));
    }
    Ok(())
}

// ─── Attribute sets ─────────────────────────────────────────────────────────

/// `set.key`, without evaluating the attribute.
pub fn select(set: &Data, key: &Data) -> Result<Value> {
    let attrs = expect_attrs(set)?;
    let key = expect_string(key)?;
    attrs
        .get(&*key)
        .cloned()
        .ok_or_else(|| Error::new(format!("attribute '{key}' missing")))
}

/// `set ? key`: false for anything that is not a set.
pub fn has_attr(set: &Data, key: &Data) -> Result<bool> {
    let key = expect_string(key)?;
    Ok(match set {
        Data::Attrs(attrs) => attrs.contains_key(&*key),
        _ => false,
    })
}

/// Adds a dynamic attribute: a `null` key leaves the set unchanged, an
/// existing key is an error.
///
/// Nix also reports where the attribute was first defined; the runtime does
/// not know that.
pub fn attrs_insert(set: &Data, key: &Data, value: Value) -> Result<Data> {
    let attrs = expect_attrs(set)?;
    let key = match key {
        Data::Null => return Ok(set.clone()),
        other => expect_string(other)?,
    };
    if attrs.contains_key(&*key) {
        return Err(Error::new(format!("attribute '{key}' already defined")));
    }
    let mut attrs = (*attrs).clone();
    attrs.insert(key, value);
    Ok(Data::Attrs(Rc::new(attrs)))
}

/// The attribute `name` from the `with` namespaces, innermost first.
pub fn with_lookup(name: &str, namespaces: &[Value]) -> Result<Value> {
    for namespace in namespaces {
        let attrs = expect_attrs(&namespace.force()?)?;
        if let Some(value) = attrs.get(name) {
            return Ok(value.clone());
        }
    }
    Err(Error::new(format!("undefined variable '{name}'")))
}

// ─── Arithmetic ─────────────────────────────────────────────────────────────

fn overflow(verb: &str, op: &str, a: i64, b: i64) -> Error {
    Error::new(format!("integer overflow in {verb} {a} {op} {b}"))
}

/// `a + b`: numbers add; otherwise the first operand decides between string
/// and path concatenation.
pub fn add(a: &Data, b: &Data) -> Result<Data> {
    match (a, b) {
        (Data::Int(x), Data::Int(y)) => x
            .checked_add(*y)
            .map(Data::Int)
            .ok_or_else(|| overflow("adding", "+", *x, *y)),
        (Data::Int(_) | Data::Float(_), Data::Int(_) | Data::Float(_)) => {
            Ok(Data::Float(expect_float(a)? + expect_float(b)?))
        }
        (Data::Int(_), other) => Err(Error::new(format!(
            "cannot add {} to an integer",
            other.type_name()
        ))),
        (Data::Float(_), other) => Err(Error::new(format!(
            "cannot add {} to a float",
            other.type_name()
        ))),
        (Data::Path(p), other) => {
            let suffix = coerce_to_string(other, Coercion::Interpolation, false)?;
            Ok(Data::Path(
                crate::paths::canon_path(&format!("{p}{suffix}")).into(),
            ))
        }
        (first, other) => {
            let prefix = coerce_to_string(first, Coercion::Interpolation, true)?;
            let suffix = coerce_to_string(other, Coercion::Interpolation, true)?;
            Ok(Data::String(format!("{prefix}{suffix}").into()))
        }
    }
}

/// `a - b` (and `-a` as `0 - a`).
pub fn sub(a: &Data, b: &Data) -> Result<Data> {
    if matches!(a, Data::Float(_)) || matches!(b, Data::Float(_)) {
        return Ok(Data::Float(expect_float(a)? - expect_float(b)?));
    }
    let (x, y) = (expect_int(a)?, expect_int(b)?);
    x.checked_sub(y)
        .map(Data::Int)
        .ok_or_else(|| overflow("subtracting", "-", x, y))
}

pub fn mul(a: &Data, b: &Data) -> Result<Data> {
    if matches!(a, Data::Float(_)) || matches!(b, Data::Float(_)) {
        return Ok(Data::Float(expect_float(a)? * expect_float(b)?));
    }
    let (x, y) = (expect_int(a)?, expect_int(b)?);
    x.checked_mul(y)
        .map(Data::Int)
        .ok_or_else(|| overflow("multiplying", "*", x, y))
}

/// `a / b`: the divisor is checked first, as in Nix.
pub fn div(a: &Data, b: &Data) -> Result<Data> {
    if expect_float(b)? == 0.0 {
        return Err(Error::new("division by zero"));
    }
    if matches!(a, Data::Float(_)) || matches!(b, Data::Float(_)) {
        return Ok(Data::Float(expect_float(a)? / expect_float(b)?));
    }
    let (x, y) = (expect_int(a)?, expect_int(b)?);
    x.checked_div(y)
        .map(Data::Int)
        .ok_or_else(|| overflow("dividing", "/", x, y))
}

/// `-a`, which Nix evaluates as `0 - a`.
pub fn neg(a: &Data) -> Result<Data> {
    sub(&Data::Int(0), a)
}

/// `!a`.
pub fn not(a: &Data) -> Result<Data> {
    Ok(Data::Bool(!expect_bool(a)?))
}

// ─── Lists and sets ─────────────────────────────────────────────────────────

/// `a ++ b`.
pub fn concat_lists(a: &Data, b: &Data) -> Result<Data> {
    let (a, b) = (expect_list(a)?, expect_list(b)?);
    Ok(Data::List(a.iter().chain(b.iter()).cloned().collect()))
}

/// `a // b`.
pub fn update(a: &Data, b: &Data) -> Result<Data> {
    let (a, b) = (expect_attrs(a)?, expect_attrs(b)?);
    let mut merged = (*a).clone();
    merged.extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
    Ok(Data::Attrs(Rc::new(merged)))
}

// ─── Comparison ─────────────────────────────────────────────────────────────

/// `a == b`. Functions are never equal, except that a list element or
/// attribute is equal to itself (Nix compares those by pointer first).
pub fn equal(a: &Data, b: &Data) -> Result<bool> {
    Ok(match (a, b) {
        (Data::Int(x), Data::Int(y)) => x == y,
        (Data::Int(_) | Data::Float(_), Data::Int(_) | Data::Float(_)) => {
            expect_float(a)? == expect_float(b)?
        }
        (Data::Null, Data::Null) => true,
        (Data::Bool(x), Data::Bool(y)) => x == y,
        (Data::String(x), Data::String(y)) | (Data::Path(x), Data::Path(y)) => x == y,
        (Data::List(xs), Data::List(ys)) => {
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for (x, y) in xs.iter().zip(ys.iter()) {
                if !values_equal(x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Data::Attrs(xs), Data::Attrs(ys)) => {
            if is_derivation(xs) && is_derivation(ys) {
                let (x, y) = (xs.get("outPath"), ys.get("outPath"));
                if let (Some(x), Some(y)) = (x, y) {
                    return values_equal(x, y);
                }
            }
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for ((kx, x), (ky, y)) in xs.iter().zip(ys.iter()) {
                if kx != ky || !values_equal(x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        _ => false,
    })
}

fn values_equal(x: &Value, y: &Value) -> Result<bool> {
    if x.ptr_eq(y) {
        return Ok(true);
    }
    equal(&x.force()?, &y.force()?)
}

fn is_derivation(attrs: &Attrs) -> bool {
    attrs
        .get("type")
        .and_then(Value::peek)
        .is_some_and(|t| matches!(&t, Data::String(s) if &**s == "derivation"))
}

/// `a < b`. (`a > b` is `b < a`, `a <= b` is `!(b < a)`, `a >= b` is
/// `!(a < b)`, which is also how Nix orders the operands in errors.)
pub fn less_than(a: &Data, b: &Data) -> Result<bool> {
    Ok(compare(a, b)? == Ordering::Less)
}

fn compare(a: &Data, b: &Data) -> Result<Ordering> {
    let incomparable = |same: bool| {
        let detail = if same {
            "; values of that type are incomparable"
        } else {
            ""
        };
        Error::new(format!(
            "cannot compare {} with {}{detail}",
            a.type_name(),
            b.type_name()
        ))
    };
    match (a, b) {
        (Data::Int(x), Data::Int(y)) => Ok(x.cmp(y)),
        (Data::Int(_) | Data::Float(_), Data::Int(_) | Data::Float(_)) => {
            let (x, y) = (expect_float(a)?, expect_float(b)?);
            Ok(x.partial_cmp(&y).unwrap_or(Ordering::Equal))
        }
        (Data::String(x), Data::String(y)) | (Data::Path(x), Data::Path(y)) => {
            Ok(x.as_bytes().cmp(y.as_bytes()))
        }
        (Data::List(xs), Data::List(ys)) => {
            for (x, y) in xs.iter().zip(ys.iter()) {
                if !values_equal(x, y)? {
                    return compare(&x.force()?, &y.force()?);
                }
            }
            Ok(xs.len().cmp(&ys.len()))
        }
        _ => Err(incomparable(
            std::mem::discriminant(a) == std::mem::discriminant(b),
        )),
    }
}

// ─── Strings ────────────────────────────────────────────────────────────────

/// How permissive a string coercion is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coercion {
    /// Interpolation and `+`: only strings, paths and sets with
    /// `__toString` / `outPath`.
    Interpolation,
    /// `toString`: also numbers, Booleans, `null` and lists.
    ToString,
}

/// Coerces `data` to a string. Paths are copied to the store when
/// `copy_to_store` is set, which this runtime does not support.
pub fn coerce_to_string(data: &Data, mode: Coercion, copy_to_store: bool) -> Result<String> {
    let cannot = || {
        Error::new(format!(
            "cannot coerce {} to a string: {}",
            data.type_name(),
            print_for_error(data)
        ))
    };
    match data {
        Data::String(s) => Ok(s.to_string()),
        Data::Path(p) if copy_to_store => Err(Error::new(format!(
            "copying path '{p}' to the store is not supported"
        ))),
        Data::Path(p) => Ok(p.to_string()),
        Data::Attrs(attrs) => {
            if let Some(to_string) = attrs.get("__toString") {
                let result = call(&to_string.force()?, Value::ready(data.clone()))?.force()?;
                return coerce_to_string(&result, mode, copy_to_store);
            }
            match attrs.get("outPath") {
                Some(out) => coerce_to_string(&out.force()?, mode, copy_to_store),
                None => Err(cannot()),
            }
        }
        _ if mode == Coercion::Interpolation => Err(cannot()),
        Data::Int(i) => Ok(i.to_string()),
        Data::Float(f) => Ok(format!("{f:.6}")),
        Data::Bool(true) => Ok("1".into()),
        Data::Bool(false) | Data::Null => Ok(String::new()),
        Data::List(items) => {
            let parts: Result<Vec<String>> = items
                .iter()
                .map(|item| coerce_to_string(&item.force()?, mode, copy_to_store))
                .collect();
            Ok(parts?.join(" "))
        }
        Data::Lambda(_) | Data::PrimOp(_) => Err(cannot()),
    }
}

/// `"a${b}c"`.
pub fn interpolate_string(parts: &[Data]) -> Result<Data> {
    let mut out = String::new();
    for part in parts {
        out.push_str(&coerce_to_string(part, Coercion::Interpolation, true)?);
    }
    Ok(Data::String(out.into()))
}

/// `./a/${b}`: the first part is the (resolved) path prefix; the result is
/// normalized.
pub fn interpolate_path(parts: &[Data]) -> Result<Data> {
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        let piece = match part {
            Data::Path(p) if i == 0 => p.to_string(),
            other => coerce_to_string(other, Coercion::Interpolation, false)?,
        };
        out.push_str(&piece);
    }
    Ok(Data::Path(crate::paths::canon_path(&out).into()))
}

/// `toString x`.
pub fn to_string(data: &Data) -> Result<Data> {
    Ok(Data::String(
        coerce_to_string(data, Coercion::ToString, false)?.into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every expected message is what Nix 2.28 reports for the same operands.

    fn int(i: i64) -> Data {
        Data::Int(i)
    }

    fn string(s: &str) -> Data {
        Data::String(s.into())
    }

    fn err<T: std::fmt::Debug>(r: Result<T>) -> String {
        r.expect_err("expected an error").message
    }

    fn list(items: Vec<Data>) -> Data {
        Data::List(items.into_iter().map(Value::ready).collect())
    }

    #[test]
    fn arithmetic_errors() {
        assert_eq!(
            err(add(&int(i64::MAX), &int(1))),
            "integer overflow in adding 9223372036854775807 + 1"
        );
        assert_eq!(
            err(sub(&int(-i64::MAX), &int(2))),
            "integer overflow in subtracting -9223372036854775807 - 2"
        );
        assert_eq!(
            err(mul(&int(i64::MAX), &int(2))),
            "integer overflow in multiplying 9223372036854775807 * 2"
        );
        assert_eq!(err(div(&int(1), &int(0))), "division by zero");
        assert_eq!(err(div(&Data::Float(1.0), &int(0))), "division by zero");
        assert_eq!(err(div(&int(1), &Data::Float(0.0))), "division by zero");
        assert_eq!(
            err(div(&int(i64::MIN), &int(-1))),
            "integer overflow in dividing -9223372036854775808 / -1"
        );
        assert_eq!(
            err(add(&int(1), &string("a"))),
            "cannot add a string to an integer"
        );
        assert_eq!(
            err(add(&Data::Float(1.5), &string("a"))),
            "cannot add a string to a float"
        );
        assert_eq!(
            err(add(&int(1), &Data::Null)),
            "cannot add null to an integer"
        );
        assert_eq!(
            err(add(&string("a"), &int(1))),
            "cannot coerce an integer to a string: 1"
        );
        assert_eq!(
            err(add(&Data::Null, &int(1))),
            "cannot coerce null to a string: null"
        );
        assert_eq!(
            err(sub(&string("a"), &int(1))),
            r#"expected an integer but found a string: "a""#
        );
        assert_eq!(
            err(neg(&string("x"))),
            r#"expected an integer but found a string: "x""#
        );
        assert_eq!(
            err(not(&Data::Null)),
            "expected a Boolean but found null: null"
        );
    }

    #[test]
    fn arithmetic_results() {
        assert!(matches!(div(&int(7), &int(2)), Ok(Data::Int(3))));
        assert!(matches!(div(&int(-7), &int(2)), Ok(Data::Int(-3))));
        assert!(matches!(div(&Data::Float(7.0), &int(2)), Ok(Data::Float(f)) if f == 3.5));
        assert!(matches!(add(&int(1), &Data::Float(1.5)), Ok(Data::Float(f)) if f == 2.5));
        // -0.0 is 0 - 0.0 = 0.0 in Nix.
        assert!(matches!(neg(&Data::Float(0.0)), Ok(Data::Float(f)) if f.to_bits() == 0));
        let joined = add(&string("a"), &string("b")).unwrap();
        assert!(matches!(joined, Data::String(s) if &*s == "ab"));
        let path = add(&Data::Path("/tmp/a".into()), &string("b")).unwrap();
        assert!(matches!(path, Data::Path(p) if &*p == "/tmp/ab"));
    }

    #[test]
    fn comparisons() {
        assert!(less_than(&int(1), &Data::Float(1.5)).unwrap());
        assert!(less_than(&string("a"), &string("b")).unwrap());
        assert!(!less_than(&list(vec![int(1), int(2)]), &list(vec![int(1)])).unwrap());
        assert_eq!(
            err(less_than(&string("a"), &int(1))),
            "cannot compare a string with an integer"
        );
        assert_eq!(
            err(less_than(&Data::Null, &Data::Null)),
            "cannot compare null with null; values of that type are incomparable"
        );
        assert_eq!(
            err(less_than(&list(vec![int(1)]), &list(vec![string("a")]))),
            "cannot compare an integer with a string"
        );
        assert!(equal(&int(1), &Data::Float(1.0)).unwrap());
        assert!(!equal(&int(1), &string("1")).unwrap());
        assert!(!equal(&string("/a"), &Data::Path("/a".into())).unwrap());
    }

    #[test]
    fn function_equality_uses_pointers_inside_containers() {
        let f = Value::lambda(None, Ok);
        let data = f.force().unwrap();
        assert!(!equal(&data, &data).unwrap());
        let a = Data::List(vec![f.clone()].into());
        let b = Data::List(vec![f].into());
        assert!(equal(&a, &b).unwrap());
    }

    #[test]
    fn coercions() {
        assert_eq!(
            err(interpolate_string(&[int(1)])),
            "cannot coerce an integer to a string: 1"
        );
        assert_eq!(
            err(interpolate_string(&[Data::Float(1.5)])),
            "cannot coerce a float to a string: 1.5"
        );
        assert_eq!(
            err(interpolate_string(&[Data::Bool(true)])),
            "cannot coerce a Boolean to a string: true"
        );
        assert_eq!(
            err(interpolate_string(&[list(vec![string("b")])])),
            r#"cannot coerce a list to a string: [ "b" ]"#
        );
        let to_s = |d: &Data| match to_string(d).unwrap() {
            Data::String(s) => s.to_string(),
            _ => unreachable!(),
        };
        assert_eq!(to_s(&Data::Float(1.5)), "1.500000");
        assert_eq!(to_s(&Data::Float(1e20)), "100000000000000000000.000000");
        assert_eq!(to_s(&Data::Bool(true)), "1");
        assert_eq!(to_s(&Data::Null), "");
        let nested = list(vec![
            int(1),
            list(vec![int(2)]),
            Data::Null,
            Data::Bool(true),
        ]);
        assert_eq!(to_s(&nested), "1 2  1");
    }

    #[test]
    fn attribute_errors() {
        let empty = Data::Attrs(Rc::new(Attrs::new()));
        assert_eq!(err(select(&empty, &string("b"))), "attribute 'b' missing");
        assert_eq!(
            err(select(&int(1), &string("a"))),
            "expected a set but found an integer: 1"
        );
        assert_eq!(
            err(select(&empty, &int(1))),
            "expected a string but found an integer: 1"
        );
        assert!(!has_attr(&int(1), &string("a")).unwrap());
        assert!(matches!(
            attrs_insert(&empty, &Data::Null, Value::int(1)),
            Ok(Data::Attrs(a)) if a.is_empty()
        ));
        assert_eq!(
            err(attrs_insert(&empty, &Data::Bool(true), Value::int(1))),
            "expected a string but found a Boolean: true"
        );
    }

    #[test]
    fn calls_and_formals() {
        assert_eq!(
            err(call(&int(1), Value::int(2))),
            "attempt to call something which is not a function but an integer: 1"
        );
        let formals = [("b".to_string(), false), ("a".to_string(), false)];
        let empty = Data::Attrs(Rc::new(Attrs::new()));
        assert_eq!(
            err(check_formals(&empty, &formals, false, None)),
            "function 'anonymous lambda' called without required argument 'b'"
        );
        assert_eq!(
            err(check_formals(&empty, &formals, false, Some("f"))),
            "function 'f' called without required argument 'b'"
        );
        assert_eq!(
            err(check_formals(&int(1), &formals, false, None)),
            "expected a set but found an integer: 1"
        );
    }

    #[test]
    fn error_values_show_thunks_and_elide() {
        let lazy = Data::List(vec![Value::thunk(|| Ok(Value::int(2)))].into());
        assert_eq!(
            err(interpolate_string(&[lazy])),
            "cannot coerce a list to a string: [ «thunk» ]"
        );
        let long = list((1..=12).map(int).collect());
        assert_eq!(
            err(interpolate_string(&[long])),
            "cannot coerce a list to a string: [ 1 2 3 4 5 6 7 8 9 10 «2 items elided» ]"
        );
    }
}
