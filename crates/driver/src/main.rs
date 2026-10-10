//! The `aetheryx` command line: a drop-in replacement for the `nix` command
//! (Nix 2.28). It accepts `nix`'s commands and options; the ones it cannot
//! run yet fail with an explicit error instead of doing something different
//! from Nix. `aetheryx debug` holds the compiler's own tools.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
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

mod settings;

#[derive(Parser)]
#[command(
    name = "aetheryx",
    version,
    disable_help_subcommand = true,
    about = "A Nix evaluator, as a drop-in replacement for the `nix` command"
)]
struct Cli {
    #[command(flatten)]
    global: GlobalOptions,
    #[command(subcommand)]
    command: Command,
}

/// `nix`'s options for every command.
#[derive(Args)]
struct GlobalOptions {
    /// Enable debug output.
    #[arg(long, global = true)]
    debug: bool,
    /// Set the format of log output.
    #[arg(long, global = true, value_name = "FORMAT")]
    log_format: Option<String>,
    /// Print full build logs on standard error.
    #[arg(short = 'L', long, global = true)]
    print_build_logs: bool,
    /// Decrease the logging verbosity level.
    #[arg(long, global = true)]
    quiet: bool,
    /// Increase the logging verbosity level.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Disable substituters and consider all previously downloaded files
    /// up-to-date.
    #[arg(long, global = true)]
    offline: bool,
    /// Consider all previously downloaded files out-of-date.
    #[arg(long, global = true)]
    refresh: bool,
    /// Set the Nix configuration setting NAME to VALUE. Settings can also be
    /// given as flags (`--NAME VALUE`, `--NAME` / `--no-NAME`).
    #[arg(
        long = "option",
        global = true,
        num_args = 2,
        value_names = ["NAME", "VALUE"],
        allow_hyphen_values = true
    )]
    option: Vec<String>,
}

/// Declares the `nix` commands aetheryx does not support yet: each takes
/// any arguments and fails with an explicit error.
macro_rules! unsupported_commands {
    ($($(#[doc = $doc:literal])* $variant:ident => $name:literal,)*) => {
        #[derive(Subcommand)]
        enum Command {
            /// Evaluate a Nix expression.
            Eval(Box<EvalArgs>),
            /// aetheryx's own tools for inspecting the compiler's IR.
            #[command(subcommand)]
            Debug(DebugCommand),
            $(
                $(#[doc = $doc])*
                #[command(name = $name)]
                $variant {
                    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
                    args: Vec<String>,
                },
            )*
            /// Any other command: a deprecated alias, or not a command.
            #[command(external_subcommand)]
            Other(Vec<String>),
        }

        impl Command {
            /// The name of an unsupported command, if this is one.
            fn unsupported(&self) -> Option<&'static str> {
                match self {
                    $(Command::$variant { .. } => Some($name),)*
                    Command::Eval(_) | Command::Debug(_) | Command::Other(_) => None,
                }
            }
        }

        /// The commands `nix` suggests for a mistyped one.
        const NIX_COMMANDS: &[&str] = &["eval", $($name,)*];
    };
}

/// `nix`'s deprecated aliases, and what they stand for.
const DEPRECATED_ALIASES: [(&str, &str); 22] = [
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

unsupported_commands! {
    /// Show help about `nix` or a particular subcommand (not supported yet).
    Help => "help",
    /// Show help about store types and their settings (not supported yet).
    HelpStores => "help-stores",
    /// Run a shell in which the specified packages are available (not
    /// supported yet).
    Shell => "shell",
    /// Build a derivation or fetch a store path (not supported yet).
    Build => "build",
    /// Run a bash shell that provides the build environment of a derivation
    /// (not supported yet).
    Develop => "develop",
    /// Manage Nix flakes (not supported yet).
    Flake => "flake",
    /// Manage Nix profiles (not supported yet).
    Profile => "profile",
    /// Run a Nix application (not supported yet).
    Run => "run",
    /// Search for packages (not supported yet).
    Search => "search",
    /// Start an interactive environment for evaluating Nix expressions (not
    /// supported yet).
    Repl => "repl",
    /// Bundle an application so that it works outside of the Nix store (not
    /// supported yet).
    Bundle => "bundle",
    /// Copy paths between Nix stores (not supported yet).
    Copy => "copy",
    /// Open the Nix expression of a Nix package in $EDITOR (not supported
    /// yet).
    Edit => "edit",
    /// Reformat your code in the standard style (not supported yet).
    Fmt => "fmt",
    /// Show the build log of the specified packages or paths (not supported
    /// yet).
    Log => "log",
    /// Query information about store paths (not supported yet).
    PathInfo => "path-info",
    /// Manage the flake registry (not supported yet).
    Registry => "registry",
    /// Show why a package has another package in its closure (not supported
    /// yet).
    WhyDepends => "why-depends",
    /// Manipulate the Nix configuration (not supported yet).
    Config => "config",
    /// Daemon to perform store operations on behalf of non-root clients (not
    /// supported yet).
    Daemon => "daemon",
    /// Work with derivations, Nix's notion of a build plan (not supported
    /// yet).
    Derivation => "derivation",
    /// Manipulate the process environment (not supported yet).
    Env => "env",
    /// Compute and convert cryptographic hashes (not supported yet).
    Hash => "hash",
    /// Generate and convert Nix signing keys (not supported yet).
    Key => "key",
    /// Create or inspect NAR files (not supported yet).
    Nar => "nar",
    /// Print shell code that can be sourced by bash to reproduce the build
    /// environment of a derivation (not supported yet).
    PrintDevEnv => "print-dev-env",
    /// Manipulate a Nix realisation (not supported yet).
    Realisation => "realisation",
    /// Manipulate a Nix store (not supported yet).
    Store => "store",
    /// Upgrade Nix to the latest stable version (not supported yet).
    UpgradeNix => "upgrade-nix",
}

/// `nix eval`'s options.
#[derive(Args)]
struct EvalArgs {
    /// With --file or --expr, the attribute path to select; otherwise a flake
    /// output (not supported yet).
    installable: Option<String>,
    /// Interpret the installable as an attribute path relative to the Nix
    /// expression stored in FILE (or FILE/default.nix if it is a directory).
    #[arg(short, long, value_name = "FILE")]
    file: Option<PathBuf>,
    /// Interpret the installable as an attribute path relative to the Nix
    /// expression EXPR.
    #[arg(long, value_name = "EXPR", allow_hyphen_values = true)]
    expr: Option<String>,
    /// Apply the function EXPR to each argument.
    #[arg(long, value_name = "EXPR", allow_hyphen_values = true)]
    apply: Option<String>,
    /// Produce output in JSON format.
    #[arg(long)]
    json: bool,
    /// Print strings without quoting or escaping.
    #[arg(long)]
    raw: bool,
    /// Pass the value EXPR as the argument NAME to Nix functions.
    #[arg(long, num_args = 2, value_names = ["NAME", "EXPR"], allow_hyphen_values = true)]
    arg: Vec<String>,
    /// Pass the string STRING as the argument NAME to Nix functions.
    #[arg(long, num_args = 2, value_names = ["NAME", "STRING"], allow_hyphen_values = true)]
    argstr: Vec<String>,
    /// Pass the contents of file PATH as the argument NAME to Nix functions.
    #[arg(long, num_args = 2, value_names = ["NAME", "PATH"], allow_hyphen_values = true)]
    arg_from_file: Vec<String>,
    /// Pass the contents of stdin as the argument NAME to Nix functions.
    #[arg(long, value_name = "NAME")]
    arg_from_stdin: Option<String>,
    /// Do not instantiate each evaluated derivation (aetheryx never does).
    #[arg(long)]
    read_only: bool,
    /// Allow access to mutable paths and repositories.
    #[arg(long)]
    impure: bool,
    /// Add PATH to the search path (not supported yet).
    #[arg(short = 'I', long = "include", value_name = "PATH")]
    include: Vec<String>,
    /// Write a string or attribute set of strings to PATH (not supported
    /// yet).
    #[arg(long, value_name = "PATH")]
    write_to: Option<PathBuf>,
    /// Start an interactive environment if evaluation fails (not supported
    /// yet).
    #[arg(long)]
    debugger: bool,
    /// The Nix store to use for evaluations (not supported yet).
    #[arg(long, value_name = "STORE_URL")]
    eval_store: Option<String>,
    /// Fix corrupted or missing store paths.
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    lock_file: LockFileOptions,
}

/// `nix`'s flake lock file options, which have no effect without flakes.
#[derive(Args)]
struct LockFileOptions {
    #[arg(long, hide = true)]
    commit_lock_file: bool,
    #[arg(long, hide = true)]
    no_registries: bool,
    #[arg(long, hide = true)]
    no_update_lock_file: bool,
    #[arg(long, hide = true)]
    no_write_lock_file: bool,
    #[arg(long, hide = true)]
    recreate_lock_file: bool,
    #[arg(long, hide = true)]
    inputs_from: Option<String>,
    #[arg(long, hide = true)]
    output_lock_file: Option<String>,
    #[arg(long, hide = true)]
    reference_lock_file: Option<String>,
    #[arg(long, hide = true)]
    update_input: Vec<String>,
    #[arg(long, num_args = 2, hide = true)]
    override_input: Vec<String>,
    #[arg(long, num_args = 2, hide = true)]
    override_flake: Vec<String>,
}

#[derive(Subcommand)]
enum DebugCommand {
    /// Print the rnix syntax tree of a Nix file.
    Ast { file: PathBuf },
    /// Lower Nix files to `nix_hir` and print the IR.
    Hir {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Don't print source locations in the IR.
        #[arg(long)]
        no_locations: bool,
    },
    /// Lower Nix files to `nix_mir` and print the IR.
    Mir {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Don't print source locations in the IR.
        #[arg(long)]
        no_locations: bool,
    },
}

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
    let args = settings::expand_flags(std::env::args_os());
    let matches = match Cli::command().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) => return command_line_error(error),
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => return command_line_error(error),
    };
    let auto_arg_order = matches
        .subcommand_matches("eval")
        .map(auto_arg_order)
        .unwrap_or_default();
    let result = std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || run(cli, auto_arg_order))
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

/// Reports a usage error like `nix` does, and fails.
fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("Try 'aetheryx --help' for more information.");
    ExitCode::FAILURE
}

/// Reports an error parsing the command line with `nix`'s wording where it
/// has one. Like `nix`, usage errors exit with 1; help and version with 0.
fn command_line_error(error: clap::Error) -> ExitCode {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let invalid_arg = match error.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(arg)) => Some(arg.clone()),
        _ => None,
    };
    match (error.kind(), invalid_arg) {
        (ErrorKind::UnknownArgument, Some(arg)) if arg.starts_with('-') => {
            usage_error(&format!("unrecognised flag '{arg}'"))
        }
        (ErrorKind::UnknownArgument, Some(arg)) => {
            usage_error(&format!("unexpected argument '{arg}'"))
        }
        (
            ErrorKind::InvalidValue
            | ErrorKind::WrongNumberOfValues
            | ErrorKind::TooFewValues
            | ErrorKind::NoEquals,
            Some(arg),
        ) => {
            // `arg` is the flag and its value names: `--arg <NAME> <EXPR>`.
            let flag = arg.split_whitespace().next().unwrap_or_default();
            let required = arg.matches('<').count();
            let given = match error.get(ContextKind::ActualNumValues) {
                Some(ContextValue::Number(n)) => *n as usize,
                _ => 0,
            };
            usage_error(&format!(
                "flag '{flag}' requires {required} argument(s), but only {given} were given"
            ))
        }
        (ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, _) => {
            usage_error("no subcommand specified")
        }
        _ => {
            let _ = error.print();
            if error.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

/// Where each `--arg`, `--argstr`, `--arg-from-file` and `--arg-from-stdin`
/// is on the command line, in order: later ones replace earlier ones with the
/// same name, whatever their kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoArgFlag {
    /// The nth `--arg`.
    Arg(usize),
    Argstr(usize),
    ArgFromFile(usize),
    ArgFromStdin,
}

fn auto_arg_order(eval: &clap::ArgMatches) -> Vec<AutoArgFlag> {
    // The index of the first value of each occurrence of the flag `id`.
    let positions = |id: &str, values_per_flag: usize| -> Vec<usize> {
        eval.indices_of(id)
            .map(|indices| indices.step_by(values_per_flag).collect())
            .unwrap_or_default()
    };
    let mut order: Vec<(usize, AutoArgFlag)> = Vec::new();
    for (id, values_per_flag, flag) in [
        ("arg", 2, AutoArgFlag::Arg as fn(usize) -> AutoArgFlag),
        ("argstr", 2, AutoArgFlag::Argstr),
        ("arg_from_file", 2, AutoArgFlag::ArgFromFile),
        ("arg_from_stdin", 1, |_| AutoArgFlag::ArgFromStdin),
    ] {
        for (n, position) in positions(id, values_per_flag).into_iter().enumerate() {
            order.push((position, flag(n)));
        }
    }
    order.sort_by_key(|&(position, _)| position);
    order.into_iter().map(|(_, flag)| flag).collect()
}

fn run(cli: Cli, auto_arg_order: Vec<AutoArgFlag>) -> std::result::Result<(), Failure> {
    check_settings(&cli.global)?;
    if let Some(name) = cli.command.unsupported() {
        return Err(miette!("'nix {name}' is not supported by aetheryx yet").into());
    }
    match cli.command {
        Command::Eval(args) => eval_command(*args, &auto_arg_order),
        Command::Debug(command) => Ok(debug_command(command)?),
        Command::Other(args) => other_command(&args[0]),
        _ => unreachable!("unsupported commands are handled above"),
    }
}

/// A command that is not one of `nix`'s main commands.
fn other_command(name: &str) -> std::result::Result<(), Failure> {
    if let Some((_, target)) = DEPRECATED_ALIASES.iter().find(|(alias, _)| *alias == name) {
        eprintln!("warning: '{name}' is a deprecated alias for '{target}'");
        return Err(miette!("'nix {name}' is not supported by aetheryx yet").into());
    }
    let mut message = format!("'{name}' is not a recognised command");
    // `nix` does not suggest the `shell` shorthand.
    let commands = NIX_COMMANDS.iter().copied().filter(|c| *c != "shell");
    let suggestions = eval::suggestions(commands, name);
    if !suggestions.is_empty() {
        message.push_str(&format!("\n       Did you mean {suggestions}?"));
    }
    Err(Failure::Usage(message))
}

/// Applies the `--option` settings: fails on one that would change the
/// result, and warns about unknown ones as Nix does.
fn check_settings(global: &GlobalOptions) -> Result<()> {
    for name in global.option.chunks(2).map(|pair| pair[0].as_str()) {
        match settings::verdict(name) {
            settings::Verdict::Accepted => {}
            settings::Verdict::Unknown => eprintln!("warning: unknown setting '{name}'"),
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

fn eval_command(
    args: EvalArgs,
    auto_arg_order: &[AutoArgFlag],
) -> std::result::Result<(), Failure> {
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
    let read_stdin = || -> Result<String> {
        let mut contents = String::new();
        std::io::stdin()
            .read_to_string(&mut contents)
            .into_diagnostic()?;
        Ok(contents)
    };
    let attr_path = args.installable.clone().unwrap_or_default();
    let installable = match (args.file, args.expr) {
        (Some(file), None) if file == Path::new("-") => Installable::Stdin(read_stdin()?),
        (Some(file), None) => Installable::File(file),
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
    let mut auto_args = Vec::new();
    for flag in auto_arg_order {
        let pair = |values: &[String], n: usize| (values[2 * n].clone(), values[2 * n + 1].clone());
        auto_args.push(match *flag {
            AutoArgFlag::Arg(n) => {
                let (name, expr) = pair(&args.arg, n);
                (name, AutoArg::Expr(expr))
            }
            AutoArgFlag::Argstr(n) => {
                let (name, string) = pair(&args.argstr, n);
                (name, AutoArg::String(string))
            }
            AutoArgFlag::ArgFromFile(n) => {
                let (name, path) = pair(&args.arg_from_file, n);
                let contents = fs::read_to_string(&path)
                    .map_err(|e| miette!("opening file '{path}': {}", io_reason(&e)))?;
                (name, AutoArg::String(contents))
            }
            AutoArgFlag::ArgFromStdin => {
                let name = args.arg_from_stdin.clone().expect("given");
                (name, AutoArg::String(read_stdin()?))
            }
        });
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
        DebugCommand::Ast { file } => {
            let (_, parse) = parse_file(&Context::new(), &file)?;
            print_node(&parse.syntax(), 0);
        }
        DebugCommand::Hir {
            files,
            no_locations,
        } => {
            let ctx = &mut Context::new();
            for file in &files {
                let module = lower_to_hir(ctx, file)?.1;
                println!("{}", print_module(ctx, module, no_locations));
            }
        }
        DebugCommand::Mir {
            files,
            no_locations,
        } => {
            let ctx = &mut Context::new();
            for file in &files {
                let (source, module) = lower_to_hir(ctx, file)?;
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
