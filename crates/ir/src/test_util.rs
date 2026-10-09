//! Helpers shared by the tests of this crate.

use pliron::builtin::attributes::ATTR_KEY_GIVEN_NAMES;
use pliron::builtin::ops::ModuleOp;
use pliron::context::{Context, Ptr};
use pliron::irfmt::parsers::spaced;
use pliron::linked_list::ContainsLinkedList;
use pliron::location::Located;
use pliron::operation::{Operation, verify_operation};
use pliron::parsable::parse_from_str;
use pliron::printable::Printable;
use rnix::ast;

use crate::ast_to_hir::{LowerOptions, lower_root_with};
use crate::diagnostics::{SourceError, SourceFile, span_of};

/// `sub/test.nix` relative to `/work`, so it resolves to `/work/sub/test.nix`.
pub fn test_file(ctx: &Context, src: &str) -> SourceFile {
    SourceFile::with_cwd(ctx, "sub/test.nix", src, std::path::Path::new("/work")).unwrap()
}

/// Parses `src` (which must parse) and lowers it to HIR, with `$HOME` set to
/// `/home/u`.
pub fn try_lower_hir(src: &str) -> (Context, Result<ModuleOp, SourceError>) {
    let mut ctx = Context::new();
    let file = test_file(&ctx, src);
    let parse = ast::Root::parse(src);
    file.check_parse(parse.errors()).unwrap();
    let options = LowerOptions {
        home_dir: Some("/home/u".to_string()),
    };
    let res = lower_root_with(&mut ctx, &file, &parse.tree(), &options);
    (ctx, res)
}

/// The structure of `op` and everything nested in it, in pre-order: each op's
/// name, source span and attributes. Value and block names are left out
/// because pliron renames them when parsing.
pub fn structure(ctx: &Context, op: Ptr<Operation>) -> Vec<String> {
    let opr = op.deref(ctx);
    let mut attrs: Vec<String> = opr
        .attributes
        .0
        .iter()
        .filter(|(key, _)| **key != ATTR_KEY_GIVEN_NAMES)
        .map(|(key, value)| format!("{key}={}", value.disp(ctx)))
        .collect();
    attrs.sort();
    let mut out = vec![format!(
        "{} {:?} {attrs:?}",
        Operation::get_opid(op, ctx),
        span_of(&opr.loc())
    )];
    for region in opr.regions() {
        for block in region.deref(ctx).iter(ctx) {
            for nested in block.deref(ctx).iter(ctx) {
                out.extend(structure(ctx, nested));
            }
        }
    }
    out
}

/// Prints `op`, parses it back with pliron's parser, and checks that the
/// result verifies and has the same structure. This exercises the textual
/// format of every op and attribute in `op`.
pub fn assert_round_trips(ctx: &mut Context, op: Ptr<Operation>) {
    let printed = op.disp(ctx).to_string();
    let parsed = match parse_from_str(spaced(Operation::top_level_parser()), ctx, &printed) {
        Ok(parsed) => parsed,
        Err(e) => panic!("cannot parse back:\n{printed}\n{}", e.disp(ctx)),
    };
    if let Err(e) = verify_operation(parsed, ctx) {
        panic!("parsed IR does not verify: {}", e.disp(ctx));
    }
    assert_eq!(structure(ctx, parsed), structure(ctx, op), "{printed}");
}

/// The `.nix` files in `samples/<dir>` at the root of the repository, as
/// `(file name, text)`, sorted by name.
pub fn samples(dir: &str) -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(dir);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "nix"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&path).unwrap())
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no samples in {}", dir.display());
    files
}
