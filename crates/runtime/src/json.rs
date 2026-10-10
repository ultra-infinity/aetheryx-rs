//! Printing values as JSON the way `nix eval --json` does.

use std::fmt::Write;

use crate::error::{Error, Result};
use crate::ops::{Coercion, coerce_to_string};
use crate::value::{Data, Value};

/// Prints `value` as JSON, evaluated deeply. Unlike [`crate::print`], any
/// error fails the whole conversion.
///
/// - A set with `__toString` is the string it converts to, and a set with
///   `outPath` is its `outPath`.
/// - Paths are printed as they are, without copying them to the store.
/// - Functions cannot be converted.
pub fn print_json(value: &Value) -> Result<String> {
    let mut out = String::new();
    json(&mut out, value)?;
    Ok(out)
}

fn json(out: &mut String, value: &Value) -> Result<()> {
    crate::stack::check()?;
    let data = value.force()?;
    match &data {
        Data::Null => out.push_str("null"),
        Data::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Data::Int(i) => write!(out, "{i}").unwrap(),
        Data::Float(f) => out.push_str(&format_json_float(*f)),
        Data::String(s) | Data::Path(s) => quote_json(out, s),
        Data::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json(out, item)?;
            }
            out.push(']');
        }
        Data::Attrs(attrs) => {
            if attrs.contains_key("__toString") {
                let s = coerce_to_string(&data, Coercion::Interpolation, false)?;
                quote_json(out, &s);
            } else if let Some(out_path) = attrs.get("outPath") {
                json(out, out_path)?;
            } else {
                out.push('{');
                for (i, (name, value)) in attrs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    quote_json(out, name);
                    out.push(':');
                    json(out, value)?;
                }
                out.push('}');
            }
        }
        Data::Lambda(_) | Data::PrimOp(_) => {
            return Err(Error::new(format!(
                "cannot convert {} to JSON",
                data.type_name()
            )));
        }
    }
    Ok(())
}

/// Quotes a JSON string as Nix's JSON library does: `"`, `\` and control
/// characters are escaped (`\n`, `\u0001`, ...); everything else, including
/// non-ASCII text, is written as is.
fn quote_json(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Formats a float as Nix's JSON library does: the shortest digits that read
/// back as the same number, in decimal notation if the decimal exponent is in
/// (-4, 15] (`0.1`, `1.0`, `123456789.0`), otherwise in scientific notation
/// with a signed, at least two-digit exponent (`1e+16`, `1.5e-07`).
/// Non-finite numbers are `null`.
pub fn format_json_float(f: f64) -> String {
    if !f.is_finite() {
        return "null".into();
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if f == 0.0 {
        return format!("{sign}0.0");
    }
    // Rust's `{:e}` prints the shortest round-tripping digits: `1.5e-7`.
    let scientific = format!("{:e}", f.abs());
    let (mantissa, exp) = scientific.split_once('e').expect("`{:e}` has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exp: i32 = exp.parse().expect("`{:e}` exponents are integers");
    // The number is 0.DIGITS * 10^n.
    let (k, n) = (digits.len() as i32, exp + 1);
    let body = if k <= n && n <= 15 {
        format!("{digits}{}.0", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 15 {
        let (int, frac) = digits.split_at(n as usize);
        format!("{int}.{frac}")
    } else if -4 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let mantissa = match digits.split_at(1) {
            (first, "") => first.to_string(),
            (first, rest) => format!("{first}.{rest}"),
        };
        let exp = n - 1;
        let exp_sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{exp_sign}{:02}", exp.abs())
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primops::Builtins;
    use crate::value::{Attrs, LambdaInfo};

    // Expected strings are what `nix eval --json` (Nix 2.28) prints.

    #[test]
    fn floats() {
        for (f, expected) in [
            (0.1, "0.1"),
            (1.0, "1.0"),
            (1e300, "1e+300"),
            (0.0, "0.0"),
            (1.5e-7, "1.5e-07"),
            (123456789.0, "123456789.0"),
            (100.0, "100.0"),
            (1e16, "1e+16"),
            (2.5e15, "2.5e+15"),
        ] {
            assert_eq!(format_json_float(f), expected, "{f:e}");
        }
    }

    #[test]
    fn strings() {
        // nix eval --json --file s.nix, with s.nix: "a\"b\\c/d\n\t\r\x01\x1f\x7fé€😀"
        let s = Value::string("a\"b\\c/d\n\t\r\u{1}\u{1f}\u{7f}é€😀");
        assert_eq!(
            print_json(&s).unwrap(),
            "\"a\\\"b\\\\c/d\\n\\t\\r\\u0001\\u001f\u{7f}é€😀\""
        );
    }

    #[test]
    fn sets() {
        // { b = 1; a = "x"; c = { d = null; }; }
        let inner = Value::attrs(Attrs::from([("d".into(), Value::null())]));
        let set = Value::attrs(Attrs::from([
            ("b".into(), Value::int(1)),
            ("a".into(), Value::string("x")),
            ("c".into(), inner),
        ]));
        assert_eq!(
            print_json(&set).unwrap(),
            r#"{"a":"x","b":1,"c":{"d":null}}"#
        );
        // { outPath = "/x"; a = 1; }
        let drv_like = Value::attrs(Attrs::from([
            ("outPath".into(), Value::string("/x")),
            ("a".into(), Value::int(1)),
        ]));
        assert_eq!(print_json(&drv_like).unwrap(), r#""/x""#);
        let empty = Value::list(Vec::new());
        assert_eq!(print_json(&empty).unwrap(), "[]");
    }

    #[test]
    fn functions_cannot_be_converted() {
        let lambda = Value::lambda(
            std::rc::Rc::new(LambdaInfo {
                name: None,
                pos: "/w/f.nix:1:1".into(),
                formals: None,
            }),
            Ok,
        );
        let map = Builtins::new().get("map").unwrap();
        let add_1 = crate::ops::call(
            &Builtins::new().get("add").unwrap().force().unwrap(),
            Value::int(1),
        )
        .unwrap();
        for (value, expected) in [
            (lambda, "cannot convert a function to JSON"),
            (map, "cannot convert the built-in function 'map' to JSON"),
            (
                add_1,
                "cannot convert the partially applied built-in function 'add' to JSON",
            ),
        ] {
            assert_eq!(print_json(&value).unwrap_err().message, expected);
        }
    }
}
