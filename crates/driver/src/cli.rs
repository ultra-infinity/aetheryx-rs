//! The command line, parsed the way `nix` parses it (a port of Nix 2.28's
//! `Args::parseCmdline`):
//!
//! - Arguments are read left to right. A compound short flag is split
//!   (`-vL` is `-v -L`, `-Ifoo` is `-I foo`), and `--` ends the flags.
//! - A flag takes exactly its number of arguments, whatever they look like:
//!   in `--expr -1` and `--argstr x --show-trace`, the values are values.
//! - The global flags (`--option`, settings, logging, `--help`,
//!   `--version`) are accepted anywhere; the command's own flags after it.
//! - The first positional argument selects the command; the command takes
//!   the rest.
//! - Everything is parsed before anything runs, so `--version extra` fails
//!   on `extra`.

use std::collections::VecDeque;

use crate::settings::{self, SettingFlag};

/// A command line error, reported with `nix`'s wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// A usage error (`nix` adds "Try 'nix --help' for more information.").
    Usage(String),
    /// Another error, such as an invalid setting value.
    Other(String),
}

/// A command line that could not be parsed: the error, and what was parsed
/// before it (`--help` before a usage error shows the help, as in `nix`).
#[derive(Debug)]
pub struct ParseFailure {
    pub error: CliError,
    pub partial: Box<CommandLine>,
}

/// A parsed command line.
#[derive(Debug, Default)]
pub struct CommandLine {
    /// The settings given with `--option NAME VALUE` or as flags, in order.
    pub settings: Vec<(String, String)>,
    /// Warnings to print, which `nix` prints while parsing.
    pub warnings: Vec<String>,
    pub help: bool,
    pub version: bool,
    pub command: Option<Command>,
}

#[derive(Debug)]
pub enum Command {
    Eval(EvalArgs),
    /// `aetheryx debug TOOL ...`; the tool is not chosen yet if `None`.
    Debug(Option<DebugCommand>),
    /// A `nix` command aetheryx does not support yet; its arguments are
    /// ignored.
    Unsupported(&'static str),
    /// One of `nix`'s deprecated aliases, with what it stands for.
    DeprecatedAlias {
        name: &'static str,
        target: &'static str,
    },
}

/// `nix eval`'s arguments.
#[derive(Debug, Default)]
pub struct EvalArgs {
    pub installable: Option<String>,
    pub file: Option<String>,
    pub expr: Option<String>,
    pub apply: Option<String>,
    pub json: bool,
    pub raw: bool,
    pub impure: bool,
    pub debugger: bool,
    pub include: Vec<String>,
    pub write_to: Option<String>,
    pub eval_store: Option<String>,
    /// `--inputs-from FLAKE`, which needs flakes.
    pub inputs_from: Option<String>,
    /// `--arg`, `--argstr`, `--arg-from-file`, `--arg-from-stdin`, in
    /// order.
    pub auto_args: Vec<(String, AutoArgSource)>,
}

/// Where the value of an argument for a function comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoArgSource {
    /// `--arg NAME EXPR`.
    Expr(String),
    /// `--argstr NAME STRING`.
    String(String),
    /// `--arg-from-file NAME PATH`.
    File(String),
    /// `--arg-from-stdin NAME`.
    Stdin,
}

#[derive(Debug)]
pub enum DebugCommand {
    Ast {
        files: Vec<String>,
    },
    Hir {
        files: Vec<String>,
        no_locations: bool,
    },
    Mir {
        files: Vec<String>,
        no_locations: bool,
    },
}

/// The `nix` commands aetheryx does not support yet.
pub const UNSUPPORTED_COMMANDS: [&str; 29] = [
    "build",
    "bundle",
    "config",
    "copy",
    "daemon",
    "derivation",
    "develop",
    "edit",
    "env",
    "flake",
    "fmt",
    "hash",
    "help",
    "help-stores",
    "key",
    "log",
    "nar",
    "path-info",
    "print-dev-env",
    "profile",
    "realisation",
    "registry",
    "repl",
    "run",
    "search",
    "shell",
    "store",
    "upgrade-nix",
    "why-depends",
];

/// `nix`'s deprecated aliases, and what they stand for.
pub const DEPRECATED_ALIASES: [(&str, &str); 22] = [
    ("add-to-store", "store add-path"),
    ("cat-nar", "nar cat"),
    ("cat-store", "store cat"),
    ("copy-sigs", "store copy-sigs"),
    ("dev-shell", "develop"),
    ("diff-closures", "store diff-closures"),
    ("doctor", "config check"),
    ("dump-path", "store dump-path"),
    ("hash-file", "hash file"),
    ("hash-path", "hash path"),
    ("ls-nar", "nar ls"),
    ("ls-store", "store ls"),
    ("make-content-addressable", "store make-content-addressed"),
    ("optimise-store", "store optimise"),
    ("ping-store", "store info"),
    ("show-config", "config show"),
    ("show-derivation", "derivation show"),
    ("sign-paths", "store sign"),
    ("to-base16", "hash to-base16"),
    ("to-base32", "hash to-base32"),
    ("to-base64", "hash to-base64"),
    ("verify", "store verify"),
];

/// Parses `args` (without the program name).
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<CommandLine, ParseFailure> {
    let mut line = CommandLine::default();
    match line.parse(args.into_iter().collect()) {
        Ok(()) => Ok(line),
        Err(error) => Err(ParseFailure {
            error,
            partial: Box::new(line),
        }),
    }
}

/// Splits a compound short flag (`-qlf` is `-q -l -f`, `-j3` is `-j 3`):
/// returns the first flag and puts the rest back in front of `args`.
fn expand_compound(arg: String, args: &mut VecDeque<String>) -> String {
    let bytes = arg.as_bytes();
    if !(bytes.len() > 2 && bytes[0] == b'-' && bytes[1] != b'-' && bytes[1].is_ascii_alphabetic())
    {
        return arg;
    }
    let mut rest = Vec::new();
    for (j, b) in bytes.iter().enumerate().skip(2) {
        if b.is_ascii_alphabetic() {
            rest.push(format!("-{}", *b as char));
        } else {
            // `arg[..j]` is ASCII, so `j` is a character boundary.
            rest.push(arg[j..].to_string());
            break;
        }
    }
    for flag in rest.into_iter().rev() {
        args.push_front(flag);
    }
    format!("-{}", bytes[1] as char)
}

/// Takes the `n` arguments of `flag`.
fn take(flag: &str, n: usize, args: &mut VecDeque<String>) -> Result<Vec<String>, CliError> {
    if args.len() < n {
        return Err(CliError::Usage(format!(
            "flag '{flag}' requires {n} argument(s), but only {} were given",
            args.len()
        )));
    }
    Ok(args.drain(..n).collect())
}

impl CommandLine {
    fn parse(&mut self, mut args: VecDeque<String>) -> Result<(), CliError> {
        let mut dash_dash = false;
        while let Some(mut arg) = args.pop_front() {
            if !dash_dash {
                arg = expand_compound(arg, &mut args);
            }
            if !dash_dash && arg == "--" {
                dash_dash = true;
            } else if !dash_dash && arg.starts_with('-') {
                if !self.global_flag(&arg, &mut args)? && !self.command_flag(&arg, &mut args)? {
                    return Err(CliError::Usage(format!("unrecognised flag '{arg}'")));
                }
            } else {
                self.positional(arg)?;
            }
        }
        Ok(())
    }

    /// Handles a global flag; returns whether `flag` is one.
    fn global_flag(&mut self, flag: &str, args: &mut VecDeque<String>) -> Result<bool, CliError> {
        match flag {
            "--debug" | "--quiet" | "--offline" | "--refresh" | "--print-build-logs" | "-L"
            | "--verbose" | "-v" => {}
            "--log-format" => {
                let format = take(flag, 1, args)?.remove(0);
                if !matches!(
                    format.as_str(),
                    "raw" | "raw-with-logs" | "internal-json" | "bar" | "bar-with-logs"
                ) {
                    return Err(CliError::Other(format!(
                        "option 'log-format' has an invalid value '{format}'"
                    )));
                }
            }
            "--option" => {
                let mut pair = take(flag, 2, args)?.into_iter();
                let (name, value) = (pair.next().unwrap(), pair.next().unwrap());
                // Like `nix`, a bad `--option` is only a warning, and is not
                // applied.
                if settings::verdict(&name) == settings::Verdict::Unknown {
                    self.warnings.push(format!("unknown setting '{name}'"));
                    return Ok(true);
                }
                match settings::validate(&name, &value) {
                    Ok(warnings) => {
                        self.warnings.extend(warnings);
                        self.settings.push((name, value));
                    }
                    Err(invalid) => self.warnings.push(format!("error: {}", invalid.message)),
                }
            }
            "--help" => self.help = true,
            "--version" => self.version = true,
            _ => match flag.strip_prefix("--").and_then(settings::flag) {
                Some(SettingFlag::Fixed { setting, value }) => {
                    self.settings.push((setting, value.to_string()));
                }
                Some(SettingFlag::WithValue { setting }) => {
                    let value = take(flag, 1, args)?.remove(0);
                    match settings::validate(&setting, &value) {
                        Ok(warnings) => self.warnings.extend(warnings),
                        Err(invalid) if invalid.usage => {
                            return Err(CliError::Usage(invalid.message));
                        }
                        Err(invalid) => return Err(CliError::Other(invalid.message)),
                    }
                    self.settings.push((setting, value));
                }
                None => return Ok(false),
            },
        }
        Ok(true)
    }

    /// Handles a flag of the selected command; returns whether `flag` is
    /// one.
    fn command_flag(&mut self, flag: &str, args: &mut VecDeque<String>) -> Result<bool, CliError> {
        match &mut self.command {
            Some(Command::Eval(_)) if flag == "--no-registries" => {
                self.warnings
                    .push("'--no-registries' is deprecated; use '--no-use-registries'".into());
                Ok(true)
            }
            Some(Command::Eval(eval)) => eval.flag(flag, args),
            Some(Command::Debug(Some(DebugCommand::Hir { no_locations, .. })))
            | Some(Command::Debug(Some(DebugCommand::Mir { no_locations, .. })))
                if flag == "--no-locations" =>
            {
                *no_locations = true;
                Ok(true)
            }
            // The flags of a command that is not supported are ignored.
            Some(Command::Unsupported(_) | Command::DeprecatedAlias { .. }) => Ok(true),
            _ => Ok(false),
        }
    }

    fn positional(&mut self, arg: String) -> Result<(), CliError> {
        match &mut self.command {
            None => self.command = Some(select_command(&arg)?),
            Some(Command::Eval(eval)) => {
                if eval.installable.is_some() {
                    return Err(CliError::Usage(format!("unexpected argument '{arg}'")));
                }
                eval.installable = Some(arg);
            }
            Some(Command::Debug(tool @ None)) => {
                *tool = Some(match arg.as_str() {
                    "ast" => DebugCommand::Ast { files: Vec::new() },
                    "hir" => DebugCommand::Hir {
                        files: Vec::new(),
                        no_locations: false,
                    },
                    "mir" => DebugCommand::Mir {
                        files: Vec::new(),
                        no_locations: false,
                    },
                    _ => {
                        return Err(CliError::Usage(format!(
                            "'{arg}' is not a recognised debug command (ast, hir or mir)"
                        )));
                    }
                });
            }
            Some(Command::Debug(Some(
                DebugCommand::Ast { files }
                | DebugCommand::Hir { files, .. }
                | DebugCommand::Mir { files, .. },
            ))) => files.push(arg),
            Some(Command::Unsupported(_) | Command::DeprecatedAlias { .. }) => {}
        }
        Ok(())
    }
}

/// The command named `name`, or `nix`'s error with its suggestions.
fn select_command(name: &str) -> Result<Command, CliError> {
    match name {
        "eval" => return Ok(Command::Eval(EvalArgs::default())),
        "debug" => return Ok(Command::Debug(None)),
        _ => {}
    }
    if let Some(&command) = UNSUPPORTED_COMMANDS.iter().find(|c| **c == name) {
        return Ok(Command::Unsupported(command));
    }
    if let Some(&(name, target)) = DEPRECATED_ALIASES.iter().find(|(alias, _)| *alias == name) {
        return Ok(Command::DeprecatedAlias { name, target });
    }
    let mut message = format!("'{name}' is not a recognised command");
    // `nix` suggests its commands, but not the `shell` shorthand.
    let commands = std::iter::once("eval").chain(
        UNSUPPORTED_COMMANDS
            .iter()
            .copied()
            .filter(|c| *c != "shell"),
    );
    let suggestions = eval::suggestions(commands, name);
    if !suggestions.is_empty() {
        message.push_str(&format!("\n       Did you mean {suggestions}?"));
    }
    Err(CliError::Usage(message))
}

impl EvalArgs {
    /// Handles a flag of `nix eval`; returns whether `flag` is one.
    fn flag(&mut self, flag: &str, args: &mut VecDeque<String>) -> Result<bool, CliError> {
        let one = |args: &mut VecDeque<String>| take(flag, 1, args).map(|mut v| v.remove(0));
        match flag {
            "--file" | "-f" => self.file = Some(one(args)?),
            "--expr" => self.expr = Some(one(args)?),
            "--apply" => self.apply = Some(one(args)?),
            "--json" => self.json = true,
            "--raw" => self.raw = true,
            "--impure" => self.impure = true,
            "--debugger" => self.debugger = true,
            "--include" | "-I" => self.include.push(one(args)?),
            "--write-to" => self.write_to = Some(one(args)?),
            "--eval-store" => self.eval_store = Some(one(args)?),
            "--arg" | "--argstr" | "--arg-from-file" => {
                let mut pair = take(flag, 2, args)?.into_iter();
                let (name, value) = (pair.next().unwrap(), pair.next().unwrap());
                let source = match flag {
                    "--arg" => AutoArgSource::Expr(value),
                    "--argstr" => AutoArgSource::String(value),
                    _ => AutoArgSource::File(value),
                };
                self.auto_args.push((name, source));
            }
            "--arg-from-stdin" => {
                let name = one(args)?;
                self.auto_args.push((name, AutoArgSource::Stdin));
            }
            // No effect on what aetheryx evaluates: it never instantiates
            // derivations or repairs the store, and has no flakes.
            "--read-only"
            | "--repair"
            | "--commit-lock-file"
            | "--no-update-lock-file"
            | "--no-write-lock-file"
            | "--recreate-lock-file" => {}
            "--inputs-from" => self.inputs_from = Some(one(args)?),
            "--output-lock-file" | "--reference-lock-file" | "--update-input" => {
                one(args)?;
            }
            "--override-input" | "--override-flake" => {
                take(flag, 2, args)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `aetheryx --help`.
pub const HELP: &str = "\
aetheryx: a Nix evaluator, as a drop-in replacement for the `nix` command (Nix 2.28)

Usage: aetheryx [OPTION...] COMMAND [ARG...]

Commands:
  eval      Evaluate a Nix expression (`aetheryx eval --help`).
  debug     Inspect aetheryx's compiler: `debug ast|hir|mir [--no-locations] FILE...`.
  The other `nix` commands (build, develop, flake, run, shell, ...) are
  recognised but not supported yet.

Options:
  --help                 Show this help.
  --version              Show aetheryx's version.
  -L, --print-build-logs, -v, --verbose, --quiet, --debug, --log-format FORMAT,
  --offline, --refresh   Accepted, as `nix` does.
  --option NAME VALUE    Set a Nix setting. Settings are also accepted as flags:
                         --NAME VALUE, --NAME / --no-NAME, --extra-NAME VALUE.
";

/// `aetheryx eval --help`.
pub const EVAL_HELP: &str = "\
Usage: aetheryx eval [OPTION...] [INSTALLABLE]

Evaluate a Nix expression and print its value, as `nix eval` does.

Options:
  -f, --file FILE              Evaluate FILE (FILE/default.nix for a directory, `-` for
                               standard input); INSTALLABLE is an attribute path in it.
  --expr EXPR                  Evaluate EXPR (in pure mode unless --impure).
  --apply EXPR                 Apply the function EXPR to the result.
  --json                       Print the result as JSON.
  --raw                        Print the result, a string, without quotes.
  --arg NAME EXPR              Pass EXPR as the argument NAME to functions.
  --argstr NAME STRING         Pass STRING as the argument NAME to functions.
  --arg-from-file NAME PATH    Pass the contents of PATH as the argument NAME.
  --arg-from-stdin NAME        Pass standard input as the argument NAME.
  --impure                     Evaluate --expr with the impure builtins.

Flakes, -I, --write-to, --debugger and --eval-store are not supported yet.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<CommandLine, CliError> {
        parse(args.iter().map(|a| a.to_string())).map_err(|failure| failure.error)
    }

    fn eval_args(args: &[&str]) -> EvalArgs {
        match parse_args(args).unwrap().command {
            Some(Command::Eval(eval)) => eval,
            other => panic!("{other:?}"),
        }
    }

    fn error(args: &[&str]) -> String {
        match parse_args(args).unwrap_err() {
            CliError::Usage(message) | CliError::Other(message) => message,
        }
    }

    #[test]
    fn values_are_taken_as_they_are() {
        let eval = eval_args(&["eval", "--expr", "-1", "--argstr", "x", "--show-trace"]);
        assert_eq!(eval.expr.as_deref(), Some("-1"));
        assert_eq!(
            eval.auto_args,
            [(
                "x".to_string(),
                AutoArgSource::String("--show-trace".into())
            )]
        );
    }

    #[test]
    fn dash_dash_ends_the_flags() {
        let eval = eval_args(&["eval", "--expr", "{a=1;}", "--", "a"]);
        assert_eq!(eval.installable.as_deref(), Some("a"));
        let eval = eval_args(&["eval", "--expr", "{a=1;}", "--", "--bogus"]);
        assert_eq!(eval.installable.as_deref(), Some("--bogus"));
    }

    #[test]
    fn compound_short_flags_are_split() {
        // As in `nix`: `-I3` is `-I 3` and `-f-` is `-f -`, but every letter
        // is a flag of its own, so `-Ifoo=bar` is `-I -f -o -o =bar`.
        assert_eq!(eval_args(&["eval", "-I3", "--expr", "1"]).include, ["3"]);
        assert_eq!(eval_args(&["eval", "-f-"]).file.as_deref(), Some("-"));
        assert_eq!(
            error(&["eval", "-Ifoo=bar", "--expr", "1"]),
            "unrecognised flag '-o'"
        );
        let line = parse_args(&["-vL", "eval", "--expr", "1"]).unwrap();
        assert!(matches!(line.command, Some(Command::Eval(_))));
    }

    #[test]
    fn settings_are_collected_in_order() {
        let line = parse_args(&[
            "--show-trace",
            "eval",
            "--max-jobs",
            "2",
            "--no-sandbox",
            "--option",
            "cores",
            "3",
            "--expr",
            "1",
        ])
        .unwrap();
        let pairs = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        assert_eq!(
            line.settings,
            pairs(&[
                ("show-trace", "true"),
                ("max-jobs", "2"),
                ("sandbox", "false"),
                ("cores", "3")
            ])
        );
    }

    #[test]
    fn errors_use_nix_wording() {
        // Each expected message is what `nix` 2.28 prints.
        assert_eq!(error(&["eval", "--bogus"]), "unrecognised flag '--bogus'");
        assert_eq!(
            error(&["eval", "--expr", "1", "a", "b"]),
            "unexpected argument 'b'"
        );
        assert_eq!(
            error(&["eval", "-f"]),
            "flag '-f' requires 1 argument(s), but only 0 were given"
        );
        assert_eq!(
            error(&["eval", "--expr", "1", "-I"]),
            "flag '-I' requires 1 argument(s), but only 0 were given"
        );
        assert_eq!(
            error(&["eval", "--expr", "1", "--max-jobs"]),
            "flag '--max-jobs' requires 1 argument(s), but only 0 were given"
        );
        assert_eq!(
            error(&["eval", "--arg", "x"]),
            "flag '--arg' requires 2 argument(s), but only 1 were given"
        );
        assert_eq!(
            error(&["--version", "extra"]),
            "'extra' is not a recognised command"
        );
        assert_eq!(
            error(&["foo"]),
            "'foo' is not a recognised command\n       Did you mean one of fmt or log?"
        );
        assert_eq!(
            error(&["shel"]),
            "'shel' is not a recognised command\n       Did you mean help?"
        );
        assert_eq!(
            error(&["eval", "--extra-cores", "2"]),
            "unrecognised flag '--extra-cores'"
        );
    }
}
