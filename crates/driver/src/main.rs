//! The `aetheryx` command line driver.

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use ir::ast_to_hir::lower_root;
use ir::diagnostics::SourceFile;
use miette::{IntoDiagnostic, Result, WrapErr};
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
    /// Evaluate a Nix file (not implemented yet: only lowers it to `nix_hir`).
    Eval {
        file: PathBuf,
        /// Print the `nix_hir` IR.
        #[arg(long)]
        dump_hir: bool,
        /// Don't print source locations in the IR.
        #[arg(long)]
        no_locations: bool,
    },
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
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Eval {
            file,
            dump_hir,
            no_locations,
        } => {
            let hir = lower_file(&mut Context::new(), &file, no_locations)?;
            if dump_hir {
                println!("{hir}");
            }
            eprintln!("note: evaluation is not implemented yet");
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
                println!("{}", lower_file(ctx, file, no_locations)?);
            }
        }
    }
    Ok(())
}

/// Reads and parses `file`, failing on any parse error.
fn parse_file(ctx: &Context, file: &Path) -> Result<(SourceFile, Parse<Root>)> {
    let text = fs::read_to_string(file)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {}", file.display()))?;
    let parse = Root::parse(&text);
    let source = SourceFile::new(ctx, file, text)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to resolve {}", file.display()))?;
    source.check_parse(parse.errors())?;
    Ok((source, parse))
}

/// Parses `file`, lowers it to `nix_hir` and returns the printed IR.
fn lower_file(ctx: &mut Context, file: &Path, no_locations: bool) -> Result<String> {
    let (source, parse) = parse_file(ctx, file)?;
    let module = lower_root(ctx, &source, &parse.tree())?;
    if no_locations {
        erase_locations(ctx, module.get_operation());
    }
    Ok(module.get_operation().disp(ctx).to_string())
}

fn print_node(node: &SyntaxNode, depth: usize) {
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
