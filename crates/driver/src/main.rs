use std::{fs, path::PathBuf};

use clap::{Parser, Subcommand};
use anyhow::{Context, Result};
use rnix::{ast::Root, NodeOrToken, SyntaxNode, SyntaxToken};

#[derive(Parser)]
#[command(name = "aetheryx")]
#[command(about = "Pure Nix evualuation engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commmand,
}

#[derive(Subcommand)]
enum Commmand {
    /// Evaluate a Nix file (placeholder).
    Eval {
        file: PathBuf,
        #[arg(long)]
        dump_ir: bool,
    },
    /// Parse and dump the rnix AST for a Nix file.
    Ast {
        file: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    use Commmand::*;

    match cli.command {
        Eval { file, dump_ir: _ } => {
            // For now, just parse to ensure it's valid.
            parse_and_maybe_dump_ast(&file, false)?;
        }
        Ast { file } => {
            parse_and_maybe_dump_ast(&file, true)?;
        }
    }
    Ok(())
}

fn parse_and_maybe_dump_ast(file: &PathBuf, dump: bool) -> Result<()> {
    let src = fs::read_to_string(file).with_context(|| format!("failed to read {}", file.display()))?;
    let parse = Root::parse(&src);
    for err in parse.errors() {
        eprintln!("parse error: {}", err);
    }
    if dump {
        let syntax = parse.syntax();
        print_node(&syntax, 0);
    }
    Ok(())
}

fn print_node(node: &SyntaxNode, indent: usize) {
    println!("{:indent$}{:?}", "", node.kind(), indent = indent * 2);
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(n) => print_node(&n, indent + 1),
            NodeOrToken::Token(t) => print_token(&t, indent + 1),
        }
    }
}

fn print_token(tok: &SyntaxToken, indent: usize) {
    let mut text = tok.text().to_string().replace('\n', "\\n").replace('\t', "\\t");
    if text.len() > 40 {
        text.truncate(37);
        text.push_str("...");
    }
    println!(
        "{:indent$}{:?} {:?}",
        "",
        tok.kind(),
        text,
        indent = indent * 2
    );
}
