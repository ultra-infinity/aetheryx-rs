//! The `aetheryx` command line: a drop-in replacement for the `nix` command
//! (Nix 2.28). It accepts `nix`'s commands and options, parsed as `nix`
//! parses them (see [`cli`]); the ones it cannot run yet fail with an
//! explicit error instead of doing something different from Nix.
//! `aetheryx debug` holds the compiler's own tools.

mod cli;
mod settings;

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use cli::{AutoArgSource, CliError, Command, CommandLine, DebugCommand, EvalArgs, ParseFailure};
use eval::{AutoArg, EvalCommand, Installable, OutputFormat};
use ir::ast_to_hir::{LowerOptions, lower_root};
use ir::diagnostics::{ErrorKind, SourceFile, span_of};
use ir::hir_to_mir::lower_module;
use ir::stack::DeepDrop;
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use pliron::builtin::ops::ModuleOp;
use pliron::context::Context;
use pliron::location::erase_locations;
use pliron::op::Op;
use pliron::printable::Printable;
use rnix::{NodeOrToken, Parse, Root, SyntaxNode, SyntaxToken};

/// The interpreter recurses as deeply as the Nix program does.
const STACK_SIZE: usize = 256 * 1024 * 1024;

/// Why a command failed.
enum Failure {
    /// The command line is wrong.
    Usage(String),
    Report(miette::Report),
}

impl<E: Into<miette::Report>> From<E> for Failure {
    fn from(error: E) -> Self {
        Failure::Report(error.into())
    }
}

fn main() -> ExitCode {
    // Like `nix`, exit quietly when the reader of the output goes away,
    // instead of failing to write to the closed pipe.
    // SAFETY: restoring the default action of SIGPIPE has no preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // Messages are printed as they are, as Nix does, not wrapped.
    let _ = miette::set_hook(Box::new(|_| {
        Box::new(miette::MietteHandlerOpts::new().wrap_lines(false).build())
    }));
    let args: Option<Vec<String>> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().ok())
        .collect();
    let Some(args) = args else {
        return usage_error("arguments must be valid UTF-8");
    };
    let line = match cli::parse(args) {
        Ok(line) => line,
        Err(ParseFailure { error, partial }) => {
            print_warnings(&partial);
            return match error {
                // Like `nix`, `--help` before a usage error shows the help.
                CliError::Usage(_) if partial.help => print_help(&partial),
                CliError::Usage(message) => usage_error(&message),
                CliError::Other(message) => {
                    eprintln!("error: {message}");
                    ExitCode::FAILURE
                }
            };
        }
    };
    print_warnings(&line);
    if line.version {
        println!("aetheryx {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if line.help {
        return print_help(&line);
    }
    if line.command.is_none() {
        return usage_error("no subcommand specified");
    }
    let result = std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || run(line))
        .expect("the evaluation thread can be started")
        .join()
        .expect("the evaluation thread panicked");
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => usage_error(&message),
        Err(Failure::Report(report)) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

fn print_warnings(line: &CommandLine) {
    for warning in &line.warnings {
        eprintln!("warning: {warning}");
    }
}

/// Shows the help of the command given, or of `aetheryx`.
fn print_help(line: &CommandLine) -> ExitCode {
    match line.command {
        Some(Command::Eval(_)) => print!("{}", cli::EVAL_HELP),
        _ => print!("{}", cli::HELP),
    }
    ExitCode::SUCCESS
}

/// Reports a usage error like `nix` does, and fails.
fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("Try 'aetheryx --help' for more information.");
    ExitCode::FAILURE
}

fn run(line: CommandLine) -> std::result::Result<(), Failure> {
    check_settings(&line.settings)?;
    match line.command.expect("checked by the caller") {
        Command::Eval(args) => eval_command(args),
        Command::Debug(Some(command)) => Ok(debug_command(command)?),
        Command::Debug(None) => Err(Failure::Usage("no subcommand specified".into())),
        Command::Unsupported(name) => {
            Err(miette!("'nix {name}' is not supported by aetheryx yet").into())
        }
        Command::DeprecatedAlias { name, target } => {
            eprintln!("warning: '{name}' is a deprecated alias for '{target}'");
            Err(miette!("'nix {name}' is not supported by aetheryx yet").into())
        }
    }
}

/// Fails on a setting that would change the result.
fn check_settings(settings: &[(String, String)]) -> Result<()> {
    for (name, _) in settings {
        match settings::verdict(name) {
            // The parser warned about unknown settings and left them out.
            settings::Verdict::Accepted | settings::Verdict::Unknown => {}
            settings::Verdict::Unsupported => {
                return Err(miette!(
                    "the setting '{name}' is not supported by aetheryx yet"
                ));
            }
        }
    }
    Ok(())
}

/// The reason in an I/O error, as Nix prints it (without Rust's
/// " (os error N)").
fn io_reason(error: &std::io::Error) -> String {
    let reason = error.to_string();
    reason
        .split(" (os error")
        .next()
        .unwrap_or_default()
        .to_string()
}

fn read_stdin() -> Result<String> {
    let mut contents = String::new();
    std::io::stdin()
        .read_to_string(&mut contents)
        .into_diagnostic()?;
    Ok(contents)
}

/// Reads the file `path` given with `--arg-from-file`, with Nix's messages.
fn read_arg_file(path: &str) -> Result<String> {
    let mut file =
        fs::File::open(path).map_err(|e| miette!("opening file '{path}': {}", io_reason(&e)))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|e| miette!("reading from file: {}", io_reason(&e)))?;
    Ok(contents)
}

fn eval_command(args: EvalArgs) -> std::result::Result<(), Failure> {
    if args.file.is_some() && args.expr.is_some() {
        return Err(Failure::Usage("'--file' and '--expr' are exclusive".into()));
    }
    if args.raw && args.json {
        return Err(Failure::Usage(
            "--raw and --json are mutually exclusive".into(),
        ));
    }
    let unsupported = |what: &str| Err(miette!("{what} is not supported by aetheryx yet").into());
    if !args.include.is_empty() {
        return unsupported("the search path (-I)");
    }
    if args.write_to.is_some() {
        return unsupported("--write-to");
    }
    if args.debugger {
        return unsupported("--debugger");
    }
    if args.eval_store.is_some() {
        return unsupported("--eval-store");
    }
    if args.inputs_from.is_some() {
        return unsupported("--inputs-from (flakes)");
    }
    let attr_path = args.installable.clone().unwrap_or_default();
    let installable = match (args.file, args.expr) {
        (Some(file), None) if file == "-" => Installable::Stdin(read_stdin()?),
        (Some(file), None) => Installable::File(file.into()),
        (None, Some(expr)) => Installable::Expr(expr),
        (None, None) => {
            let installable = args.installable.as_deref().unwrap_or(".");
            return Err(miette!(
                "evaluating flakes ('{installable}') is not supported by aetheryx yet; \
                 use --file or --expr"
            )
            .into());
        }
        (Some(_), Some(_)) => unreachable!("--file with --expr is rejected above"),
    };
    // Like Nix, a later argument replaces an earlier one with the same name,
    // which is then not read at all.
    let mut sources: Vec<(String, AutoArgSource)> = Vec::new();
    for (name, source) in args.auto_args {
        sources.retain(|(other, _)| *other != name);
        sources.push((name, source));
    }
    let mut auto_args = Vec::new();
    for (name, source) in sources {
        let arg = match source {
            AutoArgSource::Expr(expr) => AutoArg::Expr(expr),
            AutoArgSource::String(string) => AutoArg::String(string),
            AutoArgSource::File(path) => AutoArg::String(read_arg_file(&path)?),
            AutoArgSource::Stdin => AutoArg::String(read_stdin()?),
        };
        auto_args.push((name, arg));
    }
    let format = match (args.json, args.raw) {
        (true, _) => OutputFormat::Json,
        (_, true) => OutputFormat::Raw,
        _ => OutputFormat::Nix,
    };
    let command = EvalCommand {
        installable,
        attr_path,
        apply: args.apply,
        auto_args,
        format,
        impure: args.impure,
    };
    let cwd = std::env::current_dir().into_diagnostic()?;
    let output = eval::nix_eval(&command, &cwd, LowerOptions::from_env())?;
    let mut stdout = std::io::stdout().lock();
    // A raw string is printed as it is; anything else on a line of its own.
    let newline = if format == OutputFormat::Raw {
        ""
    } else {
        "\n"
    };
    write!(stdout, "{output}{newline}").into_diagnostic()?;
    stdout.flush().into_diagnostic()?;
    Ok(())
}

fn debug_command(command: DebugCommand) -> Result<()> {
    match command {
        DebugCommand::Ast { files } => {
            let [file] = files.as_slice() else {
                return Err(miette!("`aetheryx debug ast` takes one file"));
            };
            let (_, parse) = parse_file(&Context::new(), Path::new(file))?;
            print_node(&parse.syntax(), 0);
        }
        DebugCommand::Hir {
            files,
            no_locations,
        } => {
            if files.is_empty() {
                return Err(miette!("`aetheryx debug hir` takes one or more files"));
            }
            let ctx = &mut Context::new();
            for file in &files {
                let module = lower_to_hir(ctx, Path::new(file))?.1;
                println!("{}", print_module(ctx, module, no_locations));
            }
        }
        DebugCommand::Mir {
            files,
            no_locations,
        } => {
            if files.is_empty() {
                return Err(miette!("`aetheryx debug mir` takes one or more files"));
            }
            let ctx = &mut Context::new();
            for file in &files {
                let (source, module) = lower_to_hir(ctx, Path::new(file))?;
                lower_module(ctx, module).map_err(|e| {
                    let span = span_of(&e.loc).map(|s| s.offset()..s.offset() + s.len());
                    source.error_at(ErrorKind::Verify(e.err.to_string()), span)
                })?;
                println!("{}", print_module(ctx, module, no_locations));
            }
        }
    }
    Ok(())
}

/// Reads `file`.
fn read_file(ctx: &Context, file: &Path) -> Result<SourceFile> {
    let text = fs::read_to_string(file)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {}", file.display()))?;
    SourceFile::new(ctx, file, text)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to resolve {}", file.display()))
}

/// Reads and parses `file`, failing on any parse error.
fn parse_file(ctx: &Context, file: &Path) -> Result<(SourceFile, DeepDrop<Parse<Root>>)> {
    let source = read_file(ctx, file)?;
    let parse = ir::stack::parse(source.text());
    source.check_parse(parse.errors())?;
    Ok((source, parse))
}

/// Parses `file` and lowers it to `nix_hir`.
fn lower_to_hir(ctx: &mut Context, file: &Path) -> Result<(SourceFile, ModuleOp)> {
    let (source, parse) = parse_file(ctx, file)?;
    let module = lower_root(ctx, &source, &parse.tree())?;
    Ok((source, module))
}

fn print_module(ctx: &Context, module: ModuleOp, no_locations: bool) -> String {
    if no_locations {
        erase_locations(ctx, module.get_operation());
    }
    module.get_operation().disp(ctx).to_string()
}

fn print_node(node: &SyntaxNode, depth: usize) {
    ir::stack::grow(|| print_node_here(node, depth));
}

fn print_node_here(node: &SyntaxNode, depth: usize) {
    println!("{:indent$}{:?}", "", node.kind(), indent = depth * 2);
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(n) => print_node(&n, depth + 1),
            NodeOrToken::Token(t) => print_token(&t, depth + 1),
        }
    }
}

fn print_token(token: &SyntaxToken, depth: usize) {
    const MAX_CHARS: usize = 40;
    let escaped = token.text().replace('\n', "\\n").replace('\t', "\\t");
    // Truncate by characters: a byte index could split a UTF-8 character.
    let text = if escaped.chars().count() > MAX_CHARS {
        let prefix: String = escaped.chars().take(MAX_CHARS - 3).collect();
        format!("{prefix}...")
    } else {
        escaped
    };
    println!(
        "{:indent$}{:?} {:?}",
        "",
        token.kind(),
        text,
        indent = depth * 2
    );
}
