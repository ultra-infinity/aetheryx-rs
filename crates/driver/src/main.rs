use std::{fs, path::PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "aetheryx")]
#[command(about = "Pure Nix evualuation engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commmand,
}

#[derive(Subcommand)]
enum Commmand {
    Eval {
        file: PathBuf,
        #[arg(long)]
        dump_ir: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    use Commmand::*;

    match cli.command {
        Eval { file, dump_ir } => {
            let source = fs::read_to_string(file).unwrap();
            match parser::parse_str(&source) {
                Ok(ast) => println!("{:?}", ast),
                Err(e) => {
                    eprintln!("Parse error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}
