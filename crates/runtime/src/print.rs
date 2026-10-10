//! Printing values the way `nix eval` does.

use std::collections::HashSet;
use std::fmt::Write;

use crate::error::Result;
use crate::value::{Data, Value};

/// Prints `value` like `nix eval`: evaluated deeply. An error evaluating
/// `value` itself is returned; an error inside it is printed in its place,
/// as `«error: message»`.
pub fn print_value(value: &Value) -> Result<String> {
    let mut printer = Printer {
        out: String::new(),
        seen: HashSet::new(),
    };
    let data = value.force()?;
    printer.data(&data);
    Ok(printer.out)
}

/// Prints a value inside an error message the way Nix does: without forcing
/// anything (unevaluated parts print as `«thunk»`), and with at most
/// [`MAX_ERROR_ITEMS`] items per list or set.
pub fn print_for_error(data: &Data) -> String {
    let mut out = String::new();
    error_value(&mut out, data);
    out
}

/// How many list items or attributes error messages show.
pub const MAX_ERROR_ITEMS: usize = 10;

fn error_value(out: &mut String, data: &Data) {
    let nested = |out: &mut String, value: &Value| match value.peek() {
        Some(data) => error_value(out, &data),
        None => out.push_str("«thunk»"),
    };
    match data {
        Data::List(items) if items.is_empty() => out.push_str("[ ]"),
        Data::List(items) => {
            out.push('[');
            for item in items.iter().take(MAX_ERROR_ITEMS) {
                out.push(' ');
                nested(out, item);
            }
            if items.len() > MAX_ERROR_ITEMS {
                write!(out, " «{} items elided»", items.len() - MAX_ERROR_ITEMS).unwrap();
            }
            out.push_str(" ]");
        }
        Data::Attrs(attrs) if attrs.is_empty() => out.push_str("{ }"),
        Data::Attrs(attrs) => {
            out.push('{');
            for (name, value) in attrs.iter().take(MAX_ERROR_ITEMS) {
                write!(out, " {} = ", attr_name(name)).unwrap();
                nested(out, value);
                out.push(';');
            }
            if attrs.len() > MAX_ERROR_ITEMS {
                write!(
                    out,
                    " «{} attributes elided»",
                    attrs.len() - MAX_ERROR_ITEMS
                )
                .unwrap();
            }
            out.push_str(" }");
        }
        other => scalar(out, other),
    }
}

struct Printer {
    out: String,
    /// Non-empty lists and sets already printed; Nix prints `«repeated»`
    /// when it meets one again.
    seen: HashSet<*const ()>,
}

impl Printer {
    /// Prints a value nested in the one being printed: an error evaluating
    /// it is printed in its place.
    fn nested(&mut self, value: &Value) {
        match crate::stack::check().and_then(|()| value.force()) {
            Ok(data) => self.data(&data),
            Err(error) => write!(self.out, "«error: {}»", error.message).unwrap(),
        }
    }

    fn data(&mut self, data: &Data) {
        match data {
            Data::List(items) => {
                if items.is_empty() {
                    self.out.push_str("[ ]");
                    return;
                }
                if !self.seen.insert(items.as_ptr().cast()) {
                    self.out.push_str("«repeated»");
                    return;
                }
                self.out.push('[');
                for item in items.iter() {
                    self.out.push(' ');
                    self.nested(item);
                }
                self.out.push_str(" ]");
            }
            Data::Attrs(attrs) => {
                if attrs.is_empty() {
                    self.out.push_str("{ }");
                    return;
                }
                if !self.seen.insert(std::rc::Rc::as_ptr(attrs).cast()) {
                    self.out.push_str("«repeated»");
                    return;
                }
                self.out.push('{');
                for (name, value) in attrs.iter() {
                    self.out.push(' ');
                    self.out.push_str(&attr_name(name));
                    self.out.push_str(" = ");
                    self.nested(value);
                    self.out.push(';');
                }
                self.out.push_str(" }");
            }
            other => scalar(&mut self.out, other),
        }
    }
}

/// A value that is not a list or a set.
fn scalar(out: &mut String, data: &Data) {
    match data {
        Data::Null => out.push_str("null"),
        Data::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Data::Int(i) => write!(out, "{i}").unwrap(),
        Data::Float(f) => out.push_str(&format_float(*f)),
        Data::String(s) => out.push_str(&quote_string(s)),
        Data::Path(p) => out.push_str(p),
        Data::Lambda(lambda) => match &lambda.info.name {
            Some(name) => write!(out, "«lambda {name} @ {}»", lambda.info.pos).unwrap(),
            None => write!(out, "«lambda @ {}»", lambda.info.pos).unwrap(),
        },
        Data::PrimOp(app) if app.args.is_empty() => {
            write!(out, "«primop {}»", app.op.name).unwrap();
        }
        Data::PrimOp(app) => write!(out, "«partially applied primop {}»", app.op.name).unwrap(),
        Data::List(_) | Data::Attrs(_) => unreachable!("handled by the caller"),
    }
}

/// Formats a float like C++'s default stream output (`%g` with 6 significant
/// digits), which is what Nix prints: `1500`, `0.1`, `1.23457e+29`, `1e-05`.
pub fn format_float(f: f64) -> String {
    if f.is_nan() {
        return if f.is_sign_negative() { "-nan" } else { "nan" }.into();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.into();
    }
    const PRECISION: i32 = 6;
    // Round to PRECISION significant digits to find the decimal exponent.
    let scientific = format!("{:.*e}", (PRECISION - 1) as usize, f);
    let (mantissa, exp) = scientific.split_once('e').expect("`{:e}` has an exponent");
    let exp: i32 = exp.parse().expect("`{:e}` exponents are integers");
    if !(-4..PRECISION).contains(&exp) {
        let mantissa = trim_fraction(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (PRECISION - 1 - exp).max(0) as usize;
        trim_fraction(&format!("{f:.decimals$}")).to_string()
    }
}

/// Removes trailing zeros after a decimal point, and the point itself.
fn trim_fraction(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// Quotes a string as a Nix string literal.
pub fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An attribute name as Nix prints it: bare if it is an identifier that is
/// not a keyword, quoted otherwise.
pub fn attr_name(name: &str) -> String {
    const KEYWORDS: [&str; 9] = [
        "if", "then", "else", "assert", "with", "let", "in", "rec", "inherit",
    ];
    let mut chars = name.chars();
    let is_identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '-'));
    if is_identifier && !KEYWORDS.contains(&name) {
        name.to_string()
    } else {
        quote_string(name)
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::error::Error;
    use crate::value::{Attrs, LambdaInfo};

    // Expected strings are what `nix eval` (Nix 2.28) prints.

    #[test]
    fn floats() {
        let cases = [
            (1500.0, "1500"),
            (2.75, "2.75"),
            (0.5, "0.5"),
            (1e308, "1e+308"),
            (1.2345678901234567e29, "1.23457e+29"),
            (2.2250738585072014e-308, "2.22507e-308"),
            (0.1 + 0.2, "0.3"),
            (1e-5, "1e-05"),
            (123456.0, "123456"),
            (1234567.0, "1.23457e+06"),
            (0.0001, "0.0001"),
            (100000.0, "100000"),
            (1000000.0, "1e+06"),
            (2e10, "2e+10"),
            (1e-10, "1e-10"),
            (1.5e300, "1.5e+300"),
            (std::f64::consts::PI, "3.14159"),
            (0.0, "0"),
        ];
        for (f, expected) in cases {
            assert_eq!(format_float(f), expected, "{f:e}");
        }
    }

    #[test]
    fn strings() {
        assert_eq!(quote_string("a\nb\t\"q\"\\"), r#""a\nb\t\"q\"\\""#);
        assert_eq!(quote_string("a${b}"), r#""a\${b}""#);
        assert_eq!(quote_string("$${a}"), r#""$\${a}""#);
        assert_eq!(quote_string("é"), "\"é\"");
    }

    #[test]
    fn attribute_names() {
        for (name, expected) in [
            ("-a", "\"-a\""),
            ("_", "_"),
            ("__x", "__x"),
            ("a'b", "a'b"),
            ("a-b", "a-b"),
            ("if", "\"if\""),
            ("inherit", "\"inherit\""),
            ("or", "or"),
            ("rec", "\"rec\""),
            ("true", "true"),
            ("é", "\"é\""),
            ("", "\"\""),
            ("1a", "\"1a\""),
        ] {
            assert_eq!(attr_name(name), expected);
        }
    }

    #[test]
    fn repeated_values() {
        let list = Value::list(vec![Value::int(1)]);
        let outer = Value::list(vec![list.clone(), list]);
        assert_eq!(print_value(&outer).unwrap(), "[ [ 1 ] «repeated» ]");
        let empty = Value::list(Vec::new());
        let outer = Value::list(vec![empty.clone(), empty]);
        assert_eq!(print_value(&outer).unwrap(), "[ [ ] [ ] ]");
        let mut attrs = Attrs::new();
        attrs.insert("b".into(), Value::int(2));
        attrs.insert("a".into(), Value::string("x"));
        assert_eq!(
            print_value(&Value::attrs(attrs)).unwrap(),
            r#"{ a = "x"; b = 2; }"#
        );
    }

    #[test]
    fn errors_inside_values_are_printed_in_place() {
        // nix eval --expr '{ a = { b = throw "deep"; }; c = 1; }'
        let failing = Value::thunk(|| Err(Error::new("deep")));
        let inner = Value::attrs(Attrs::from([("b".into(), failing)]));
        let outer = Value::attrs(Attrs::from([
            ("a".into(), inner),
            ("c".into(), Value::int(1)),
        ]));
        assert_eq!(
            print_value(&outer).unwrap(),
            "{ a = { b = «error: deep»; }; c = 1; }"
        );
        // An error evaluating the value itself is an error.
        let top = Value::thunk(|| Err(Error::new("top")));
        assert_eq!(print_value(&top).unwrap_err().message, "top");
    }

    #[test]
    fn functions() {
        let info = |name: Option<&str>| LambdaInfo {
            name: name.map(Rc::from),
            pos: "/w/f.nix:1:7".into(),
            formals: None,
        };
        let named = Value::lambda(Rc::new(info(Some("f"))), Ok);
        let anonymous = Value::lambda(Rc::new(info(None)), Ok);
        let list = Value::list(vec![named, anonymous]);
        assert_eq!(
            print_value(&list).unwrap(),
            "[ «lambda f @ /w/f.nix:1:7» «lambda @ /w/f.nix:1:7» ]"
        );
    }
}
