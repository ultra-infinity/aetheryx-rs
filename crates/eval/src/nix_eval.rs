//! The semantics of `nix eval`, for the installables it supports: a file
//! (`--file`) or an expression (`--expr`), selected by an attribute path.

use std::path::{Path, PathBuf};

use ir::ast_to_hir::LowerOptions;
use runtime::json::print_json;
use runtime::ops::{Coercion, auto_call, call, coerce_to_string};
use runtime::print::print_value;
use runtime::{Attrs, Data, Error, Value};

use crate::{EvalError, Program};

/// What `nix eval` evaluates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installable {
    /// `--file path`: a Nix file, or the `default.nix` of a directory.
    File(PathBuf),
    /// `--file -`: the expression read from standard input.
    Stdin(String),
    /// `--expr text`.
    Expr(String),
}

/// A value given with `--arg name expr` (an expression) or `--argstr name
/// string` (a string), for calling the functions along the attribute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoArg {
    Expr(String),
    String(String),
}

/// How the result is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// As a Nix value.
    Nix,
    /// `--json`.
    Json,
    /// `--raw`: a string, as it is.
    Raw,
}

/// A `nix eval` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalCommand {
    pub installable: Installable,
    /// The attribute path after the installable (`""` for none).
    pub attr_path: String,
    /// `--apply expr`: a function to apply to the result.
    pub apply: Option<String>,
    /// Later arguments with the same name replace earlier ones.
    pub auto_args: Vec<(String, AutoArg)>,
    pub format: OutputFormat,
    /// `--impure`: an `--expr` expression is evaluated with the impure
    /// builtins, as files always are.
    pub impure: bool,
}

/// Runs `cmd` with relative paths resolved against `cwd`, and returns what
/// `nix eval` prints on standard output: a Nix value or JSON (to which
/// `nix eval` adds a newline), or a raw string (to which it adds nothing).
///
/// Run this on a thread with a large stack (see [`crate::Session::value`]).
pub fn nix_eval(cmd: &EvalCommand, cwd: &Path, options: LowerOptions) -> Result<String, EvalError> {
    let attr_path = strip_outputs_spec(&cmd.attr_path).map_err(EvalError::Message)?;
    // Like Nix, an `--expr` expression is evaluated in pure mode.
    let pure_eval = matches!(cmd.installable, Installable::Expr(_)) && !cmd.impure;
    let mut program = Program::new(LowerOptions {
        pure_eval,
        ..options
    });
    let root = match &cmd.installable {
        Installable::File(path) => {
            let utf8 = |p: &Path| {
                p.to_str().map(str::to_owned).ok_or_else(|| {
                    EvalError::Message(format!("path '{}' is not valid UTF-8", p.display()))
                })
            };
            let given = utf8(path)?;
            // Like Nix, `<>` itself is an ordinary path.
            if given.len() > 2 && given.starts_with('<') && given.ends_with('>') {
                return Err(EvalError::Message(format!(
                    "the search path ('{given}') is not supported by aetheryx yet"
                )));
            }
            if is_url(&given) {
                return Err(EvalError::Message(format!(
                    "fetching '{given}' is not supported by aetheryx yet"
                )));
            }
            let path = ir::paths::abs_path(&given, &utf8(cwd)?);
            let (path, text) = read_nix_file(Path::new(&path))?;
            program.add_file(path, text, cwd)?
        }
        Installable::Stdin(text) => program.add_stdin(text.as_str(), cwd)?,
        Installable::Expr(text) => program.add_expr(text.as_str(), cwd)?,
    };
    let mut auto_args = Vec::new();
    for (name, arg) in &cmd.auto_args {
        let arg = match arg {
            AutoArg::Expr(text) => Some(program.add_expr(text.as_str(), cwd)?),
            AutoArg::String(_) => None,
        };
        auto_args.push((name, arg));
    }
    let apply = cmd
        .apply
        .as_ref()
        .map(|text| program.add_expr(text.as_str(), cwd))
        .transpose()?;

    let session = program.start();
    let auto_args: Attrs = cmd
        .auto_args
        .iter()
        .zip(auto_args)
        .map(|((name, arg), (_, id))| {
            let value = match (arg, id) {
                (AutoArg::String(s), _) => Value::string(s.as_str()),
                (AutoArg::Expr(_), Some(id)) => session.value(id),
                (AutoArg::Expr(_), None) => unreachable!("expressions are compiled"),
            };
            (name.as_str().into(), value)
        })
        .collect();
    let run = || -> Result<String, Error> {
        let mut value = find_along_attr_path(attr_path, &auto_args, session.value(root))?;
        if let Some(apply) = apply {
            value = call(&session.value(apply).force()?, value)?;
        }
        match cmd.format {
            OutputFormat::Nix => print_value(&value),
            OutputFormat::Json => print_json(&value),
            OutputFormat::Raw => coerce_to_string(&value.force()?, Coercion::Interpolation, true),
        }
    };
    run().map_err(|e| session.error(e))
}

/// Reads the Nix file at `path`, or the `default.nix` in it if it is a
/// directory, as Nix does; returns the path read and its contents.
fn read_nix_file(path: &Path) -> Result<(PathBuf, String), EvalError> {
    let failed = |action: &str, path: &Path, error: std::io::Error| {
        // Without Rust's " (os error N)" suffix, as Nix prints it.
        let reason = error.to_string();
        let reason = reason.split(" (os error").next().unwrap_or_default();
        EvalError::Message(format!("{action} '{}': {reason}", path.display()))
    };
    let metadata = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            EvalError::Message(format!("path '{}' does not exist", path.display()))
        }
        _ => failed("getting status of", path, e),
    })?;
    let path = if metadata.is_dir() {
        path.join("default.nix")
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&path).map_err(|e| failed("opening file", &path, e))?;
    Ok((path, text))
}

/// Whether `--file` names something Nix would fetch rather than read (a URL
/// it supports, a `channel:` or a `flake:`), as Nix's `isPseudoUrl` decides.
fn is_url(s: &str) -> bool {
    if s.starts_with("channel:") || s.starts_with("flake:") {
        return true;
    }
    s.split_once("://").is_some_and(|(scheme, _)| {
        matches!(
            scheme,
            "http" | "https" | "file" | "channel" | "git" | "s3" | "ssh"
        )
    })
}

/// Removes an outputs specifier (`^out,dev` or `^*`) from the end of an
/// installable, which does not change what is evaluated, as Nix's
/// `ExtendedOutputsSpec::parse`.
fn strip_outputs_spec(installable: &str) -> Result<&str, String> {
    let Some((prefix, spec)) = installable.rsplit_once('^') else {
        return Ok(installable);
    };
    // Output names follow store path names: `[0-9a-zA-Z+\-_?=][0-9a-zA-Z+\-._?=]*`.
    let is_name = |name: &str| {
        let ok = |c: char| c.is_ascii_alphanumeric() || "+-._?=".contains(c);
        name.chars().next().is_some_and(|c| ok(c) && c != '.') && name.chars().all(ok)
    };
    if spec == "*" || spec.split(',').all(is_name) {
        Ok(prefix)
    } else {
        Err(format!(
            "invalid extended outputs specifier '{installable}'"
        ))
    }
}

/// Splits an attribute path at dots, except in quoted parts: `a."b.c"` is
/// `["a", "b.c"]`, as Nix's `parseAttrPath`.
fn parse_attr_path(path: &str) -> Result<Vec<String>, Error> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in path.chars() {
        match c {
            '.' if !quoted => parts.push(std::mem::take(&mut current)),
            '"' => quoted = !quoted,
            c => current.push(c),
        }
    }
    if quoted {
        return Err(Error::new(format!(
            "missing closing quote in selection path '{path}'"
        )));
    }
    if !current.is_empty() {
        parts.push(current);
    }
    Ok(parts)
}

/// Selects `path` in `root`, as Nix's `findAlongAttrPath`: before each
/// step, the value is called with `auto_args` if it is a function that takes
/// them (see [`auto_call`]); a step that is a number (that fits in 32 bits,
/// as Nix parses it) indexes a list.
fn find_along_attr_path(path: &str, auto_args: &Attrs, root: Value) -> Result<Value, Error> {
    let mut value = root;
    for part in parse_attr_path(path)? {
        let index = part
            .parse::<u32>()
            .ok()
            .filter(|_| part.bytes().all(|b| b.is_ascii_digit()))
            .map(|i| i as usize);
        value = auto_call(&value.force()?, auto_args)?;
        let data = value.force()?;
        value = match (index, &data) {
            (None, Data::Attrs(attrs)) => {
                if part.is_empty() {
                    return Err(Error::new(format!(
                        "empty attribute name in selection path '{path}'"
                    )));
                }
                match attrs.get(part.as_str()) {
                    Some(attr) => attr.clone(),
                    None => {
                        let mut message =
                            format!("attribute '{part}' in selection path '{path}' not found");
                        let suggestions = suggestions(attrs.keys().map(|k| &**k), &part);
                        if !suggestions.is_empty() {
                            message.push_str(&format!("\nDid you mean {suggestions}?"));
                        }
                        return Err(Error::new(message));
                    }
                }
            }
            (None, other) => {
                return Err(Error::new(format!(
                    "the expression selected by the selection path '{path}' should be a set \
                     but is {}",
                    other.type_name()
                )));
            }
            (Some(index), Data::List(items)) => match items.get(index) {
                Some(item) => item.clone(),
                None => {
                    return Err(Error::new(format!(
                        "list index {index} in selection path '{path}' is out of range"
                    )));
                }
            },
            (Some(_), other) => {
                return Err(Error::new(format!(
                    "the expression selected by the selection path '{path}' should be a list \
                     but is {}",
                    other.type_name()
                )));
            }
        };
    }
    Ok(value)
}

/// Nix's "Did you mean ...?" suggestions: the (at most 5) names closest to
/// `query`, at an edit distance of at most 2, closest first: `a`,
/// `one of a or b`, `one of a, b or c`. Empty if there are none.
pub fn suggestions<'a>(names: impl Iterator<Item = &'a str>, query: &str) -> String {
    let mut close: Vec<(usize, &str)> = names
        .map(|name| (levenshtein(query, name), name))
        .filter(|&(distance, _)| distance <= 2)
        .collect();
    close.sort();
    close.truncate(5);
    let names: Vec<&str> = close.into_iter().map(|(_, name)| name).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("one of {} or {last}", rest.join(", ")),
    }
}

/// The edit distance between `a` and `b`, in bytes, as Nix computes it.
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if ca == cb {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_paths_split_at_unquoted_dots() {
        assert_eq!(parse_attr_path("").unwrap(), Vec::<String>::new());
        assert_eq!(parse_attr_path("a.b").unwrap(), ["a", "b"]);
        assert_eq!(parse_attr_path("a.\"b.c\"").unwrap(), ["a", "b.c"]);
        assert_eq!(parse_attr_path("\"a b\"").unwrap(), ["a b"]);
        assert_eq!(parse_attr_path("a..b").unwrap(), ["a", "", "b"]);
        assert_eq!(
            parse_attr_path("a.\"b").unwrap_err().message,
            "missing closing quote in selection path 'a.\"b'"
        );
    }

    #[test]
    fn outputs_specifiers_are_removed() {
        // `nix eval --expr '{ a = 1; }' a^out,dev` and `a^*` print 1.
        assert_eq!(strip_outputs_spec("a^out,dev"), Ok("a"));
        assert_eq!(strip_outputs_spec("a^*"), Ok("a"));
        assert_eq!(strip_outputs_spec("a.b"), Ok("a.b"));
        for invalid in ["a^", "\"a^b\""] {
            assert_eq!(
                strip_outputs_spec(invalid),
                Err(format!("invalid extended outputs specifier '{invalid}'"))
            );
        }
    }

    #[test]
    fn urls_are_recognised_like_nix() {
        for url in [
            "https://example.com/x.nix",
            "http://x",
            "channel:nixos-unstable",
            "flake:nixpkgs",
            "git://x",
        ] {
            assert!(is_url(url), "{url}");
        }
        for path in ["a.nix", "./x://y", "ftp://x", "/abs/path"] {
            assert!(!is_url(path), "{path}");
        }
    }

    #[test]
    fn suggestions_are_the_closest_names() {
        // What `nix eval --expr SET ATTR` suggests after "Did you mean".
        let cases: [(&[&str], &str, &str); 5] = [
            (&["a"], "b", "a"),
            (&["abc", "xyz"], "b", "abc"),
            (&["ab", "aa", "zz"], "a", "one of aa, ab or zz"),
            (
                &["a1", "a2", "a3", "a4", "a5", "a6"],
                "a",
                "one of a1, a2, a3, a4 or a5",
            ),
            (&["foo"], "barbaz", ""),
        ];
        for (names, query, expected) in cases {
            assert_eq!(suggestions(names.iter().copied(), query), expected);
        }
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }
}
