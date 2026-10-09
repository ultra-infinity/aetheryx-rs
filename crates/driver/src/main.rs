//! The `aetheryx` command line driver.

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use ir::ast_to_hir::{LowerOptions, lower_root};
use ir::diagnostics::{ErrorKind, SourceFile, span_of};
use ir::hir_to_mir::lower_module;
use ir::stack::DeepDrop;
use miette::{IntoDiagnostic, Result, WrapErr};
use pliron::builtin::ops::ModuleOp;
use pliron::context::Context;
use pliron::location::erase_locations;
use pliron::op::Op;
use pliron::printable::Printable;
use rnix::{NodeOrToken, Parse, Root, SyntaxNode, SyntaxToken};

#[derive(Parser)]
#[command(name = "aetheryx", about = "A Nix evaluation engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Evaluate a Nix file and print the result like `nix-instantiate --eval --strict`.
    Eval { file: PathBuf },
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || run(cli))
        .into_diagnostic()?
        .join()
        .expect("the evaluation thread panicked")
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Eval { file } => {
            let ctx = Context::new();
            let source = read_file(&ctx, &file)?;
            let printed = eval::evaluate(ctx, &source, &LowerOptions::from_env())?;
            println!("{printed}");
        }
        Command::Ast { file } => {
            let (_, parse) = parse_file(&Context::new(), &file)?;
            print_node(&parse.syntax(), 0);
        }
        Command::Hir {
            files,
            no_locations,
        } => {
            let ctx = &mut Context::new();
            for file in &files {
                let module = lower_to_hir(ctx, file)?.1;
                println!("{}", print_module(ctx, module, no_locations));
            }
        }
        Command::Mir {
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
