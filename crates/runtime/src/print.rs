//! Printing values the way `nix-instantiate --eval` does.

use std::collections::HashSet;
use std::fmt::Write;

use crate::error::Result;
use crate::value::{Data, Value};

/// Prints `value` like `nix-instantiate --eval --strict`: evaluated deeply.
pub fn print_strict(value: &Value) -> Result<String> {
    let mut printer = Printer {
        out: String::new(),
        seen: HashSet::new(),
    };
    printer.value(value)?;
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
        other => {
            let mut printer = Printer {
                out: String::new(),
                seen: HashSet::new(),
            };
            printer.scalar(other);
            out.push_str(&printer.out);
        }
    }
}

struct Printer {
    out: String,
    /// Non-empty lists and sets already printed; Nix prints `«repeated»`
    /// when it meets one again.
    seen: HashSet<*const ()>,
}

impl Printer {
    fn value(&mut self, value: &Value) -> Result<()> {
        let data = value.force()?;
        match &data {
            Data::List(items) => {
                if items.is_empty() {
                    self.out.push_str("[ ]");
                    return Ok(());
                }
                if !self.seen.insert(items.as_ptr().cast()) {
                    self.out.push_str("«repeated»");
                    return Ok(());
                }
                self.out.push('[');
                for item in items.iter() {
                    self.out.push(' ');
                    self.value(item)?;
                }
                self.out.push_str(" ]");
            }
            Data::Attrs(attrs) => {
                if attrs.is_empty() {
                    self.out.push_str("{ }");
                    return Ok(());
                }
                if !self.seen.insert(std::rc::Rc::as_ptr(attrs).cast()) {
                    self.out.push_str("«repeated»");
                    return Ok(());
                }
                self.out.push('{');
                for (name, value) in attrs.iter() {
                    self.out.push(' ');
                    self.out.push_str(&attr_name(name));
                    self.out.push_str(" = ");
                    self.value(value)?;
                    self.out.push(';');
                }
                self.out.push_str(" }");
            }
            other => self.scalar(other),
        }
        Ok(())
    }

    fn scalar(&mut self, data: &Data) {
        match data {
            Data::Null => self.out.push_str("null"),
            Data::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
            Data::Int(i) => write!(self.out, "{i}").unwrap(),
            Data::Float(f) => self.out.push_str(&format_float(*f)),
            Data::String(s) => self.out.push_str(&quote_string(s)),
            Data::Path(p) => self.out.push_str(p),
            Data::Lambda(_) => self.out.push_str("<LAMBDA>"),
            Data::PrimOp(app) if app.args.is_empty() => self.out.push_str("<PRIMOP>"),
            Data::PrimOp(_) => self.out.push_str("<PRIMOP-APP>"),
            Data::List(_) | Data::Attrs(_) => unreachable!("handled by the caller"),
        }
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
    use super::*;
    use crate::value::Attrs;

    // Expected strings are what Nix 2.28 prints.

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
        assert_eq!(print_strict(&outer).unwrap(), "[ [ 1 ] «repeated» ]");
        let empty = Value::list(Vec::new());
        let outer = Value::list(vec![empty.clone(), empty]);
        assert_eq!(print_strict(&outer).unwrap(), "[ [ ] [ ] ]");
        let mut attrs = Attrs::new();
        attrs.insert("b".into(), Value::int(2));
        attrs.insert("a".into(), Value::string("x"));
        assert_eq!(
            print_strict(&Value::attrs(attrs)).unwrap(),
            r#"{ a = "x"; b = 2; }"#
        );
    }
}
