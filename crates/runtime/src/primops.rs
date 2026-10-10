//! The `builtins` attribute set and the primops in it.
//!
//! Every name in the [`builtins`] registry is present, so that
//! `builtins.attrNames builtins` and `builtins ? x` behave as in Nix. Primops
//! that are not implemented yet fail with "builtin '…' is not supported yet"
//! when called, and unsupported values when forced. Every implemented
//! behaviour and message was checked against Nix 2.28.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::{Error, Result};
use crate::ops::{
    self, Coercion, call, coerce_to_string, expect_attrs, expect_bool, expect_function, expect_int,
    expect_list, expect_string,
};
use crate::value::{Attrs, Data, LambdaInfo, PrimOp, Value};

/// The `builtins` set of one evaluation.
pub struct Builtins {
    set: Value,
}

/// The Nix version aetheryx is compatible with (`builtins.nixVersion`).
pub const NIX_VERSION: &str = "2.28.3";

impl Builtins {
    /// The `builtins` set; with `pure_eval`, without the impure builtins, as
    /// with Nix's `pure-eval` setting.
    pub fn new(pure_eval: bool) -> Self {
        let set = Value::undefined();
        let mut attrs = Attrs::new();
        for builtin in builtins::all() {
            if pure_eval && !builtin.in_pure_eval {
                continue;
            }
            let value = match builtin.name {
                "builtins" => set.clone(),
                // The search path, which is empty in pure evaluation.
                "nixPath" if pure_eval => Value::list(Vec::new()),
                name => builtin_value(name),
            };
            attrs.insert(builtin.name.into(), value);
        }
        let attrs = Value::attrs(attrs);
        let defined = attrs.clone();
        set.define(move || Ok(defined));
        Builtins { set }
    }

    /// The builtin `builtins.<name>`.
    pub fn get(&self, name: &str) -> Result<Value> {
        ops::select(&self.set.force()?, &Data::String(name.into()))
    }
}

fn builtin_value(name: &'static str) -> Value {
    match name {
        "true" => Value::bool(true),
        "false" => Value::bool(false),
        "null" => Value::null(),
        "storeDir" => Value::string("/nix/store"),
        "langVersion" => Value::int(6),
        "nixVersion" => Value::string(NIX_VERSION),
        _ => match primop(name) {
            Some(op) => Value::primop(op),
            None => match builtins::get(name).map(|b| b.kind) {
                Some(builtins::Kind::Value) => Value::thunk(move || Err(unsupported(name))),
                Some(builtins::Kind::NixDefined) => nix_defined(name),
                _ => Value::primop(unsupported_primop(name)),
            },
        },
    }
}

/// A builtin that Nix implements in Nix code, as a lambda at the position
/// Nix reports for it; calling it fails, as it is not supported yet.
fn nix_defined(name: &'static str) -> Value {
    let pos = match name {
        "derivation" => "<nix/derivation-internal.nix>:30:1",
        _ => unreachable!("'{name}' is not defined in Nix code"),
    };
    let info = LambdaInfo {
        name: None,
        pos: pos.into(),
        formals: None,
    };
    Value::lambda(Rc::new(info), move |_| Err(unsupported(name)))
}

fn unsupported(name: &str) -> Error {
    Error::new(format!("builtin '{name}' is not supported yet"))
}

/// A primop that is not implemented: calling it fails. Its arity is 1, so
/// that applying it is what fails.
fn unsupported_primop(name: &'static str) -> &'static PrimOp {
    thread_local! {
        static UNSUPPORTED: std::cell::RefCell<HashMap<&'static str, &'static PrimOp>> =
            std::cell::RefCell::new(HashMap::new());
    }
    UNSUPPORTED.with(|cache| {
        *cache.borrow_mut().entry(name).or_insert_with(|| {
            Box::leak(Box::new(PrimOp {
                name,
                arity: 1,
                func: None,
            }))
        })
    })
}

macro_rules! primops {
    ($($name:literal => $arity:literal, $func:ident;)*) => {
        fn primop(name: &str) -> Option<&'static PrimOp> {
            match name {
                $($name => {
                    static OP: PrimOp = PrimOp { name: $name, arity: $arity, func: Some($func) };
                    Some(&OP)
                })*
                _ => None,
            }
        }
    };
}

primops! {
    "abort" => 1, prim_abort;
    "add" => 2, prim_add;
    "all" => 2, prim_all;
    "any" => 2, prim_any;
    "attrNames" => 1, prim_attr_names;
    "attrValues" => 1, prim_attr_values;
    "concatLists" => 1, prim_concat_lists;
    "concatStringsSep" => 2, prim_concat_strings_sep;
    "div" => 2, prim_div;
    "elem" => 2, prim_elem;
    "elemAt" => 2, prim_elem_at;
    "filter" => 2, prim_filter;
    "foldl'" => 3, prim_foldl;
    "genList" => 2, prim_gen_list;
    "getAttr" => 2, prim_get_attr;
    "hasAttr" => 2, prim_has_attr;
    "head" => 1, prim_head;
    "isAttrs" => 1, prim_is_attrs;
    "isBool" => 1, prim_is_bool;
    "isFloat" => 1, prim_is_float;
    "isFunction" => 1, prim_is_function;
    "isInt" => 1, prim_is_int;
    "isList" => 1, prim_is_list;
    "isNull" => 1, prim_is_null;
    "isPath" => 1, prim_is_path;
    "isString" => 1, prim_is_string;
    "length" => 1, prim_length;
    "lessThan" => 2, prim_less_than;
    "listToAttrs" => 1, prim_list_to_attrs;
    "map" => 2, prim_map;
    "mapAttrs" => 2, prim_map_attrs;
    "mul" => 2, prim_mul;
    "removeAttrs" => 2, prim_remove_attrs;
    "seq" => 2, prim_seq;
    "sort" => 2, prim_sort;
    "stringLength" => 1, prim_string_length;
    "sub" => 2, prim_sub;
    "tail" => 1, prim_tail;
    "throw" => 1, prim_throw;
    "toString" => 1, prim_to_string;
    "typeOf" => 1, prim_type_of;
}

fn forced(v: &Value) -> Result<Data> {
    v.force()
}

/// Calls `f a` and evaluates the result.
fn apply(f: &Data, a: Value) -> Result<Data> {
    call(f, a)?.force()
}

fn apply2(f: &Data, a: Value, b: Value) -> Result<Data> {
    let partial = apply(f, a)?;
    apply(&partial, b)
}

fn coerce_message_arg(v: &Value) -> Result<String> {
    coerce_to_string(&forced(v)?, Coercion::Interpolation, false)
}

fn prim_abort(args: &[Value]) -> Result<Value> {
    let message = coerce_message_arg(&args[0])?;
    Err(Error::new(format!(
        "evaluation aborted with the following error message: '{message}'"
    )))
}

fn prim_throw(args: &[Value]) -> Result<Value> {
    Err(Error::new(coerce_message_arg(&args[0])?))
}

fn numeric(op: fn(&Data, &Data) -> Result<Data>, args: &[Value]) -> Result<Value> {
    let (a, b) = (forced(&args[0])?, forced(&args[1])?);
    let both_numbers = |d: &Data| matches!(d, Data::Int(_) | Data::Float(_));
    if !both_numbers(&a) || !both_numbers(&b) {
        // `builtins.add` only adds numbers, unlike `+`.
        let non_number = if both_numbers(&a) { &b } else { &a };
        if matches!(a, Data::Float(_)) || matches!(b, Data::Float(_)) {
            ops::expect_float(non_number)?;
        }
        expect_int(non_number)?;
    }
    Ok(Value::ready(op(&a, &b)?))
}

fn prim_add(args: &[Value]) -> Result<Value> {
    numeric(ops::add, args)
}

fn prim_sub(args: &[Value]) -> Result<Value> {
    numeric(ops::sub, args)
}

fn prim_mul(args: &[Value]) -> Result<Value> {
    numeric(ops::mul, args)
}

fn prim_div(args: &[Value]) -> Result<Value> {
    numeric(ops::div, args)
}

fn prim_less_than(args: &[Value]) -> Result<Value> {
    let lt = ops::less_than(&forced(&args[0])?, &forced(&args[1])?)?;
    Ok(Value::bool(lt))
}

fn predicate(args: &[Value], test: fn(&Data) -> bool) -> Result<Value> {
    Ok(Value::bool(test(&forced(&args[0])?)))
}

fn prim_is_attrs(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::Attrs(_)))
}

fn prim_is_bool(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::Bool(_)))
}

fn prim_is_float(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::Float(_)))
}

fn prim_is_function(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::Lambda(_) | Data::PrimOp(_)))
}

fn prim_is_int(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::Int(_)))
}

fn prim_is_list(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::List(_)))
}

fn prim_is_null(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::Null))
}

fn prim_is_path(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::Path(_)))
}

fn prim_is_string(args: &[Value]) -> Result<Value> {
    predicate(args, |d| matches!(d, Data::String(_)))
}

fn prim_type_of(args: &[Value]) -> Result<Value> {
    Ok(Value::string(forced(&args[0])?.type_of()))
}

fn prim_to_string(args: &[Value]) -> Result<Value> {
    Ok(Value::ready(ops::to_string(&forced(&args[0])?)?))
}

fn prim_seq(args: &[Value]) -> Result<Value> {
    forced(&args[0])?;
    Ok(args[1].clone())
}

// ─── Lists ──────────────────────────────────────────────────────────────────

fn prim_length(args: &[Value]) -> Result<Value> {
    let len = expect_list(&forced(&args[0])?)?.len();
    Ok(Value::int(len as i64))
}

fn prim_head(args: &[Value]) -> Result<Value> {
    let list = expect_list(&forced(&args[0])?)?;
    list.first()
        .cloned()
        .ok_or_else(|| Error::new("'builtins.head' called on an empty list"))
}

fn prim_tail(args: &[Value]) -> Result<Value> {
    let list = expect_list(&forced(&args[0])?)?;
    if list.is_empty() {
        return Err(Error::new("'builtins.tail' called on an empty list"));
    }
    Ok(Value::list(&list[1..]))
}

fn prim_elem_at(args: &[Value]) -> Result<Value> {
    let list = expect_list(&forced(&args[0])?)?;
    let index = expect_int(&forced(&args[1])?)?;
    usize::try_from(index)
        .ok()
        .and_then(|i| list.get(i).cloned())
        .ok_or_else(|| {
            Error::new(format!(
                "'builtins.elemAt' called with index {index} on a list of size {}",
                list.len()
            ))
        })
}

fn prim_elem(args: &[Value]) -> Result<Value> {
    let x = forced(&args[0])?;
    for item in expect_list(&forced(&args[1])?)?.iter() {
        if ops::equal(&x, &item.force()?)? {
            return Ok(Value::bool(true));
        }
    }
    Ok(Value::bool(false))
}

fn prim_concat_lists(args: &[Value]) -> Result<Value> {
    let mut out = Vec::new();
    for list in expect_list(&forced(&args[0])?)?.iter() {
        out.extend(expect_list(&list.force()?)?.iter().cloned());
    }
    Ok(Value::list(out))
}

fn prim_map(args: &[Value]) -> Result<Value> {
    let list = expect_list(&forced(&args[1])?)?;
    if list.is_empty() {
        return Ok(Value::list(Vec::new()));
    }
    let f = forced(&args[0])?;
    expect_function(&f)?;
    let items: Vec<Value> = list
        .iter()
        .map(|item| {
            let (f, item) = (f.clone(), item.clone());
            Value::thunk(move || call(&f, item))
        })
        .collect();
    Ok(Value::list(items))
}

fn prim_filter(args: &[Value]) -> Result<Value> {
    let list = expect_list(&forced(&args[1])?)?;
    if list.is_empty() {
        return Ok(Value::list(Vec::new()));
    }
    let f = forced(&args[0])?;
    expect_function(&f)?;
    let mut kept = Vec::new();
    for item in list.iter() {
        if expect_bool(&apply(&f, item.clone())?)? {
            kept.push(item.clone());
        }
    }
    Ok(Value::list(kept))
}

fn prim_all(args: &[Value]) -> Result<Value> {
    let f = forced(&args[0])?;
    for item in expect_list(&forced(&args[1])?)?.iter() {
        if !expect_bool(&apply(&f, item.clone())?)? {
            return Ok(Value::bool(false));
        }
    }
    Ok(Value::bool(true))
}

fn prim_any(args: &[Value]) -> Result<Value> {
    let f = forced(&args[0])?;
    for item in expect_list(&forced(&args[1])?)?.iter() {
        if expect_bool(&apply(&f, item.clone())?)? {
            return Ok(Value::bool(true));
        }
    }
    Ok(Value::bool(false))
}

fn prim_foldl(args: &[Value]) -> Result<Value> {
    let f = forced(&args[0])?;
    expect_function(&f)?;
    let list = expect_list(&forced(&args[2])?)?;
    let mut acc = args[1].clone();
    for item in list.iter() {
        acc = Value::ready(apply2(&f, acc, item.clone())?);
    }
    acc.force()?;
    Ok(acc)
}

fn prim_gen_list(args: &[Value]) -> Result<Value> {
    let len = expect_int(&forced(&args[1])?)?;
    let Ok(len) = usize::try_from(len) else {
        return Err(Error::new(format!("cannot create list of size {len}")));
    };
    let f = forced(&args[0])?;
    let items = (0..len)
        .map(|i| {
            let f = f.clone();
            Value::thunk(move || call(&f, Value::int(i as i64)))
        })
        .collect::<Vec<_>>();
    Ok(Value::list(items))
}

/// A stable merge sort with a fallible comparator, like `std::stable_sort`.
fn prim_sort(args: &[Value]) -> Result<Value> {
    let list = expect_list(&forced(&args[1])?)?;
    if list.is_empty() {
        return Ok(Value::list(Vec::new()));
    }
    let less = forced(&args[0])?;
    let mut items: Vec<Value> = list.to_vec();
    merge_sort(&mut items, &mut |a, b| {
        expect_bool(&apply2(&less, a.clone(), b.clone())?)
    })?;
    Ok(Value::list(items))
}

fn merge_sort(
    items: &mut Vec<Value>,
    less: &mut impl FnMut(&Value, &Value) -> Result<bool>,
) -> Result<()> {
    if items.len() <= 1 {
        return Ok(());
    }
    let mut right = items.split_off(items.len() / 2);
    merge_sort(items, less)?;
    merge_sort(&mut right, less)?;
    let left = std::mem::take(items);
    let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
    while let (Some(a), Some(b)) = (l.peek(), r.peek()) {
        // Take from the right only if it is strictly less: stability.
        if less(b, a)? {
            items.push(r.next().unwrap());
        } else {
            items.push(l.next().unwrap());
        }
    }
    items.extend(l);
    items.extend(r);
    Ok(())
}

// ─── Strings ────────────────────────────────────────────────────────────────

fn prim_string_length(args: &[Value]) -> Result<Value> {
    let s = coerce_to_string(&forced(&args[0])?, Coercion::Interpolation, false)?;
    Ok(Value::int(s.len() as i64))
}

fn prim_concat_strings_sep(args: &[Value]) -> Result<Value> {
    let sep = expect_string(&forced(&args[0])?)?;
    let parts: Result<Vec<String>> = expect_list(&forced(&args[1])?)?
        .iter()
        .map(|item| coerce_to_string(&item.force()?, Coercion::Interpolation, true))
        .collect();
    Ok(Value::string(parts?.join(&sep)))
}

// ─── Attribute sets ─────────────────────────────────────────────────────────

fn prim_attr_names(args: &[Value]) -> Result<Value> {
    let attrs = expect_attrs(&forced(&args[0])?)?;
    Ok(Value::list(
        attrs
            .keys()
            .map(|k| Value::string(k.clone()))
            .collect::<Vec<_>>(),
    ))
}

fn prim_attr_values(args: &[Value]) -> Result<Value> {
    let attrs = expect_attrs(&forced(&args[0])?)?;
    Ok(Value::list(attrs.values().cloned().collect::<Vec<_>>()))
}

fn prim_has_attr(args: &[Value]) -> Result<Value> {
    let name = expect_string(&forced(&args[0])?)?;
    let attrs = expect_attrs(&forced(&args[1])?)?;
    Ok(Value::bool(attrs.contains_key(&*name)))
}

fn prim_get_attr(args: &[Value]) -> Result<Value> {
    ops::select(&forced(&args[1])?, &forced(&args[0])?)
}

fn prim_remove_attrs(args: &[Value]) -> Result<Value> {
    let mut attrs = (*expect_attrs(&forced(&args[0])?)?).clone();
    for name in expect_list(&forced(&args[1])?)?.iter() {
        attrs.remove(&*expect_string(&name.force()?)?);
    }
    Ok(Value::attrs(attrs))
}

fn prim_list_to_attrs(args: &[Value]) -> Result<Value> {
    let mut attrs = Attrs::new();
    for item in expect_list(&forced(&args[0])?)?.iter() {
        let item = item.force()?;
        let name = expect_string(&ops::select(&item, &Data::String("name".into()))?.force()?)?;
        let value = ops::select(&item, &Data::String("value".into()))?;
        // The first definition of a name wins.
        attrs.entry(name).or_insert(value);
    }
    Ok(Value::attrs(attrs))
}

fn prim_map_attrs(args: &[Value]) -> Result<Value> {
    let attrs = expect_attrs(&forced(&args[1])?)?;
    let f = args[0].clone();
    let mapped = attrs
        .iter()
        .map(|(name, value)| {
            let (f, name_value, value) = (f.clone(), Value::string(name.clone()), value.clone());
            let mapped = Value::thunk(move || {
                let partial = call(&f.force()?, name_value)?.force()?;
                call(&partial, value)
            });
            (Rc::clone(name), mapped)
        })
        .collect();
    Ok(Value::attrs(mapped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::print_value;

    // Expected outputs are what Nix 2.28 prints for the same calls.

    fn builtins() -> Builtins {
        Builtins::new(false)
    }

    fn apply_builtin(name: &str, args: Vec<Value>) -> Result<String> {
        let b = builtins();
        let mut f = b.get(name)?;
        for arg in args {
            f = call(&f.force()?, arg)?;
        }
        print_value(&f)
    }

    fn ints(xs: &[i64]) -> Value {
        Value::list(xs.iter().map(|&i| Value::int(i)).collect::<Vec<_>>())
    }

    #[test]
    fn every_registry_name_is_present() {
        let names = apply_builtin("attrNames", vec![builtins().get("builtins").unwrap()]);
        assert_eq!(names.unwrap().matches('"').count() / 2, 118);
    }

    #[test]
    fn list_primops() {
        assert_eq!(
            apply_builtin("length", vec![ints(&[1, 2, 3])]).unwrap(),
            "3"
        );
        assert_eq!(
            apply_builtin("head", vec![ints(&[])]).unwrap_err().message,
            "'builtins.head' called on an empty list"
        );
        assert_eq!(
            apply_builtin("elemAt", vec![ints(&[1]), Value::int(-1)])
                .unwrap_err()
                .message,
            "'builtins.elemAt' called with index -1 on a list of size 1"
        );
        assert_eq!(
            apply_builtin(
                "concatLists",
                vec![Value::list(vec![ints(&[1]), ints(&[2])])]
            )
            .unwrap(),
            "[ 1 2 ]"
        );
        assert_eq!(
            apply_builtin(
                "genList",
                vec![builtins().get("toString").unwrap(), Value::int(-1)]
            )
            .unwrap_err()
            .message,
            "cannot create list of size -1"
        );
        let less = builtins().get("lessThan").unwrap();
        assert_eq!(
            apply_builtin("sort", vec![less, ints(&[3, 1, 2])]).unwrap(),
            "[ 1 2 3 ]"
        );
        assert_eq!(
            apply_builtin("map", vec![Value::int(1), ints(&[])]).unwrap(),
            "[ ]"
        );
        assert_eq!(
            apply_builtin("map", vec![Value::int(1), ints(&[1])])
                .unwrap_err()
                .message,
            "expected a function but found an integer: 1"
        );
    }

    #[test]
    fn errors_and_strings() {
        assert_eq!(
            apply_builtin("throw", vec![Value::string("msg")])
                .unwrap_err()
                .message,
            "msg"
        );
        assert_eq!(
            apply_builtin("abort", vec![Value::string("msg")])
                .unwrap_err()
                .message,
            "evaluation aborted with the following error message: 'msg'"
        );
        assert_eq!(
            apply_builtin("throw", vec![Value::int(1)])
                .unwrap_err()
                .message,
            "cannot coerce an integer to a string: 1"
        );
        assert_eq!(
            apply_builtin("stringLength", vec![Value::string("héllo")]).unwrap(),
            "6"
        );
        assert_eq!(
            apply_builtin("add", vec![Value::string("a"), Value::string("b")])
                .unwrap_err()
                .message,
            r#"expected an integer but found a string: "a""#
        );
        assert_eq!(
            apply_builtin("add", vec![Value::float(1.5), Value::int(1)]).unwrap(),
            "2.5"
        );
        assert_eq!(
            apply_builtin("toString", vec![Value::float(1.5)]).unwrap(),
            r#""1.500000""#
        );
        assert_eq!(
            apply_builtin("typeOf", vec![Value::float(1.0)]).unwrap(),
            r#""float""#
        );
    }

    #[test]
    fn attribute_primops() {
        let mut attrs = Attrs::new();
        attrs.insert("b".into(), Value::int(1));
        attrs.insert("a".into(), Value::int(2));
        let set = Value::attrs(attrs);
        assert_eq!(
            apply_builtin("attrNames", vec![set.clone()]).unwrap(),
            r#"[ "a" "b" ]"#
        );
        assert_eq!(
            apply_builtin("attrValues", vec![set.clone()]).unwrap(),
            "[ 2 1 ]"
        );
        assert_eq!(
            apply_builtin("getAttr", vec![Value::string("c"), set])
                .unwrap_err()
                .message,
            "attribute 'c' missing"
        );
    }

    #[test]
    fn unsupported_builtins_fail_when_used() {
        assert_eq!(
            apply_builtin("fromJSON", vec![Value::string("1")])
                .unwrap_err()
                .message,
            "builtin 'fromJSON' is not supported yet"
        );
        assert_eq!(
            apply_builtin("currentSystem", vec![]).unwrap_err().message,
            "builtin 'currentSystem' is not supported yet"
        );
        assert_eq!(
            apply_builtin("storeDir", vec![]).unwrap(),
            r#""/nix/store""#
        );
    }
}
