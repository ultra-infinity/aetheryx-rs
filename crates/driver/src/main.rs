use std::{fs, path::{Path, PathBuf}};

use clap::{Parser, Subcommand};
use dialect::diagnostics::SourceFile;
use miette::{IntoDiagnostic, Result, WrapErr};
use pliron::{context::Context, location::erase_locations, op::Op, printable::Printable};
use rnix::{ast::Root, NodeOrToken, Parse, SyntaxNode, SyntaxToken};

#[derive(Parser)]
#[command(name = "aetheryx")]
#[command(about = "Pure Nix evualuation engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commmand,
}

#[derive(Subcommand)]
enum Commmand {
    /// Evaluate a Nix file (placeholder: lowers it to the Nix dialect).
    Eval {
        file: PathBuf,
        /// Print the Nix dialect IR before evaluating.
        #[arg(long)]
        dump_ir: bool,
        /// Don't print source locations in the IR.
        #[arg(long)]
        no_locations: bool,
    },
    /// Parse and dump the rnix AST for a Nix file.
    Ast {
        file: PathBuf,
    },
    /// Lower Nix files to the Nix dialect and print the IR.
    Ir {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Don't print source locations in the IR.
        #[arg(long)]
        no_locations: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    use Commmand::*;

    match cli.command {
        Eval { file, dump_ir, no_locations } => {
            let ctx = &mut Context::new();
            let ir = lower_file(ctx, &file, no_locations)?;
            if dump_ir {
                println!("{ir}");
            }
            eprintln!("note: evaluation is not implemented yet");
        }
        Ast { file } => {
            let (_, parse) = parse_file(&Context::new(), &file)?;
            print_node(&parse.syntax(), 0);
        }
        Ir { files, no_locations } => {
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

/// Parses `file`, lowers it to the Nix dialect and returns the printed IR.
fn lower_file(ctx: &mut Context, file: &Path, no_locations: bool) -> Result<String> {
    let (source, parse) = parse_file(ctx, file)?;
    let module = dialect::lower::lower_root(ctx, &source, &parse.tree())?;
    if no_locations {
        erase_locations(ctx, module.get_operation());
    }
    Ok(module.get_operation().disp(ctx).to_string())
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
