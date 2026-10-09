//! Tests of the AST to HIR lowering. Expected results come from Nix 2.28.

use miette::{Diagnostic, SourceSpan};
use pliron::builtin::attributes::ATTR_KEY_GIVEN_NAMES;
use pliron::irfmt::parsers::spaced;
use pliron::linked_list::ContainsLinkedList;
use pliron::operation::{Operation, verify_operation};
use pliron::parsable::parse_from_str;
use pliron::printable::Printable;

use super::*;
use crate::diagnostics::span_of;

/// `sub/test.nix` relative to `/work`, so it resolves to `/work/sub/test.nix`.
fn test_file(ctx: &Context, src: &str) -> SourceFile {
    SourceFile::with_cwd(ctx, "sub/test.nix", src, std::path::Path::new("/work")).unwrap()
}

fn try_lower(src: &str) -> (Context, std::result::Result<ModuleOp, SourceError>) {
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

fn lower(src: &str) -> String {
    let (ctx, res) = try_lower(src);
    res.unwrap().get_operation().disp(&ctx).to_string()
}

/// Source text of every op in `@main`'s entry block, in order.
fn entry_op_texts(src: &str) -> Vec<String> {
    let (ctx, res) = try_lower(src);
    let module = res.unwrap();
    let func = module
        .get_body(&ctx, 0)
        .deref(&ctx)
        .iter(&ctx)
        .next()
        .unwrap();
    let entry = func
        .deref(&ctx)
        .get_region(0)
        .deref(&ctx)
        .get_head()
        .unwrap();
    entry
        .deref(&ctx)
        .iter(&ctx)
        .map(|op| {
            let span = span_of(&op.deref(&ctx).loc()).expect("op has a span");
            src[span.offset()..span.offset() + span.len()].to_string()
        })
        .collect()
}

/// The structure of `op` and everything nested in it, in pre-order: each op's
/// name, source span and attributes. Value and block names are left out
/// because pliron renames them when parsing.
fn structure(ctx: &Context, op: Ptr<Operation>) -> Vec<String> {
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

/// Lowers `src`, prints the IR, and parses it back with pliron's parser. The
/// result must verify and have the same structure. This exercises the
/// textual format of every op and attribute that `src` produces.
fn assert_round_trips(src: &str) {
    let (mut ctx, res) = try_lower(src);
    let original = res.unwrap().get_operation();
    let printed = original.disp(&ctx).to_string();
    let parsed = match parse_from_str(spaced(Operation::top_level_parser()), &mut ctx, &printed) {
        Ok(op) => op,
        Err(e) => panic!("cannot parse back:\n{printed}\n{}", e.disp(&ctx)),
    };
    if let Err(e) = verify_operation(parsed, &ctx) {
        panic!("parsed IR does not verify: {}", e.disp(&ctx));
    }
    assert_eq!(
        structure(&ctx, parsed),
        structure(&ctx, original),
        "{printed}"
    );
}

#[test]
fn number_literals_are_typed() {
    let ir = lower("[ 007 1.5e3 .5 9223372036854775807 ]");
    for expected in [
        "Int(7)",
        "Float(1500.0)",
        "Float(0.5)",
        "Int(9223372036854775807)",
    ] {
        assert!(ir.contains(expected), "missing {expected} in:\n{ir}");
    }
}

#[test]
fn out_of_range_numbers_are_errors() {
    // Nix: `invalid integer '9223372036854775808'`, `invalid float '1.0e-400'`.
    for (src, expected, span) in [
        (
            "1 + 9223372036854775808",
            "invalid integer '9223372036854775808'",
            4..23,
        ),
        ("[ 1.0e-400 ]", "invalid float '1.0e-400'", 2..10),
        ("1.8e308", "invalid float '1.8e308'", 0..7),
    ] {
        let Err(err) = try_lower(src).1 else {
            panic!("{src}: expected an error")
        };
        assert_eq!(err.to_string(), expected);
        assert_eq!(err.span(), Some(SourceSpan::from(span)));
    }
}

#[test]
fn printed_ir_parses_back() {
    assert_round_trips("1 + 2 * 3");
    assert_round_trips("[ 0.1 1.0e308 1.0e-307 ]");
    assert_round_trips(r#"{ a, b ? 2, ... }@args: let c = a + b; in { inherit c; d = "x${c}"; }"#);
}

#[test]
fn lowers_arithmetic() {
    let ir = lower("1 + 2 * 3");
    assert!(ir.contains("nix_hir.binary"), "{ir}");
    assert!(ir.contains("Mul"), "{ir}");
    assert!(ir.contains("nix_hir.yield"), "{ir}");
}

#[test]
fn ops_are_located_at_their_source() {
    assert_eq!(
        entry_op_texts("1 + 2 * 3"),
        ["1", "2", "3", "2 * 3", "1 + 2 * 3", "1 + 2 * 3"]
    );
}

#[test]
fn locations_are_printed() {
    let ir = lower("{\n  a = 1;\n}");
    assert!(ir.contains("nix_loc.span 4..10"), "{ir}");
    assert!(
        ir.contains("\"/work/sub/test.nix\": line: 2, column: 3"),
        "{ir}"
    );
}

#[test]
fn lambda_params_are_ssa() {
    let ir = lower("x: y: x + y");
    assert!(!ir.contains("nix_hir.lookup"), "{ir}");
    assert!(!ir.contains("nix_hir.var"), "{ir}");
}

#[test]
fn let_bindings_are_vars() {
    let ir = lower("let a = 1; b = a; in b");
    assert!(ir.contains("nix_hir.let_in"), "{ir}");
    assert!(ir.contains("nix_hir.var \"a\""), "{ir}");
    assert!(ir.contains("nix_hir.var \"b\""), "{ir}");
}

#[test]
fn let_shadows_lambda_param() {
    let ir = lower("x: let x = 1; in x");
    assert!(ir.contains("nix_hir.var \"x\""), "{ir}");
}

#[test]
fn with_names_are_lookups() {
    let ir = lower("with builtins; length [ 1 2 ]");
    // `builtins` is a global; `length` is not, so it comes from the `with`.
    assert!(ir.contains("nix_builtins.ref \"builtins\""), "{ir}");
    assert!(ir.contains("nix_hir.lookup \"length\""), "{ir}");
    assert!(ir.contains("nix_hir.with"), "{ir}");
}

// Expected results are what Nix 2.28 does for the same expressions.
#[test]
fn global_builtins_take_precedence_over_with() {
    // Nix: `with { map = 1; }; map` is <PRIMOP>.
    let ir = lower("with { map = 1; }; map");
    assert!(ir.contains("nix_builtins.ref \"map\""), "{ir}");
    assert!(!ir.contains("nix_hir.lookup"), "{ir}");
    // Nix: `with { length = 1; }; length` is 1 (`length` is not global).
    assert!(lower("with { length = 1; }; length").contains("nix_hir.lookup \"length\""));
    // Nix: `with { true = 1; }; true` is true.
    assert!(lower("with { true = 1; }; true").contains("Bool(true)"));
}

#[test]
fn lexical_bindings_shadow_builtins() {
    // Nix: `let map = 1; in map` is 1.
    assert!(lower("let map = 1; in map").contains("nix_hir.var \"map\""));
    assert!(!lower("map: map").contains("nix_builtins"));
}

#[test]
fn prefixed_builtins_resolve_to_their_registry_name() {
    assert!(lower("__add 1 2").contains("nix_builtins.ref \"add\""));
    assert!(lower("<nixpkgs>").contains("nix_builtins.ref \"findFile\""));
}

fn undefined_variable(src: &str) -> String {
    match try_lower(src).1 {
        Err(SourceError {
            kind: ErrorKind::UndefinedVariable(name),
            ..
        }) => name,
        Err(e) => panic!("{src}: unexpected error: {e}"),
        Ok(_) => panic!("{src}: expected an undefined variable error"),
    }
}

#[test]
fn unbound_names_outside_with_are_errors() {
    // Nix: `nix-instantiate --parse -E 'add'` fails with undefined variable 'add'.
    assert_eq!(undefined_variable("add 1 2"), "add");
    assert_eq!(undefined_variable("__map"), "__map");
    // A `with` only helps when it encloses the reference.
    assert_eq!(undefined_variable("(with {}; 1) + x"), "x");
    // `inherit x;` looks `x` up like a variable.
    assert_eq!(undefined_variable("{ inherit x; }"), "x");
    assert_eq!(undefined_variable("let inherit x; in x"), "x");
    // ...but inside a `with` it is a dynamic lookup.
    assert!(lower("x: with x; foo").contains("nix_hir.lookup \"foo\""));
}

#[test]
fn undefined_variable_points_at_the_identifier() {
    let (_, res) = try_lower("1 + nope");
    let Err(err) = res else {
        panic!("expected an error")
    };
    assert_eq!(err.span(), Some(SourceSpan::from(4..8)));
}

#[test]
fn attr_paths_are_merged() {
    let ir = lower("{ a.b = 1; a.c = 2; a = { d = 3; }; }");
    assert_eq!(ir.matches("nix_hir.bind \"a\"").count(), 1, "{ir}");
    for name in ["b", "c", "d"] {
        assert!(ir.contains(&format!("nix_hir.bind \"{name}\"")), "{ir}");
    }
}

#[test]
fn duplicate_attr_points_at_both_definitions() {
    let (_, res) = try_lower("{ a = 1; a = 2; }");
    let Err(err) = res else {
        panic!("expected an error")
    };
    assert!(matches!(&err.kind, ErrorKind::DuplicateAttr(n) if n == "a"));
    let labels: Vec<SourceSpan> = err.labels().unwrap().map(|l| *l.inner()).collect();
    assert_eq!(labels, [SourceSpan::from(9..10), SourceSpan::from(2..3)]);
}

fn duplicate_name(src: &str) -> String {
    match try_lower(src).1 {
        Err(SourceError {
            kind: ErrorKind::DuplicateAttr(name),
            ..
        }) => name,
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("expected a duplicate attribute error for {src}"),
    }
}

// Expected names are the ones Nix 2.28 reports for the same input.
#[test]
fn duplicate_attr_names_use_full_path() {
    assert_eq!(duplicate_name("{ a.b = 1; a.c = 2; a.b = 3; }"), "a.b");
    assert_eq!(duplicate_name("{ a = { b = 1; }; a = { b = 2; }; }"), "a.b");
    assert_eq!(
        duplicate_name("{ a.b = { c = 1; }; a.b = { c = 2; }; }"),
        "a.b.c"
    );
    assert_eq!(
        duplicate_name("{ x = { a.b = 1; }; x.a = { b = 2; }; }"),
        "x.a.b"
    );
    assert_eq!(duplicate_name("{ a = 1; a.b = 2; }"), "a");
    assert_eq!(duplicate_name("{ a = rec { x = 1; }; a.x = 2; }"), "a.x");
    // A duplicate inside a single literal is reported relative to it.
    assert_eq!(duplicate_name("{ x = { a = 1; a = 2; }; x.b = 3; }"), "a");
}

#[test]
fn merged_set_takes_rec_from_first_definition() {
    // Nix: { a = { x = 1; y = 1; }; } -- `x` in `a.y` is the rec binding.
    let ir = lower("let x = 9; in { a = rec { x = 1; }; a.y = x; }");
    assert!(ir.contains("nix_hir.rec_attr_set"), "{ir}");
    assert!(ir.contains("nix_hir.var \"x\""), "{ir}");

    // Nix, applied to 9: { a = { x = 1; x2 = 2; z = 9; }; } -- the later
    // `rec` is ignored, so `x2` in `z` is the lambda parameter.
    let ir = lower("x2: { a = { x = 1; }; a = rec { x2 = 2; z = x2; }; }");
    assert!(!ir.contains("nix_hir.rec_attr_set"), "{ir}");
    assert!(!ir.contains("nix_hir.var"), "{ir}");
}

#[test]
fn duplicate_formals_are_errors() {
    // All four are rejected by Nix 2.28 with "duplicate formal function argument 'a'".
    for src in [
        "{ a, b, a }: a",
        "{ a, a ? 1 }: a",
        "a@{ a }: a",
        "{ a }@a: a",
    ] {
        match try_lower(src).1 {
            Err(SourceError {
                kind: ErrorKind::DuplicateFormal(name),
                ..
            }) => {
                assert_eq!(name, "a", "{src}")
            }
            Err(e) => panic!("{src}: unexpected error: {e}"),
            Ok(_) => panic!("{src}: expected a duplicate formal error"),
        }
    }
}

// Expected values are what `nix-instantiate --parse` (Nix 2.28) prints for
// the same literals in /work/sub, with HOME=/home/u.
#[test]
fn path_literals_are_resolved_like_nix() {
    for (src, expected) in [
        ("./a", "Path(\"/work/sub/a\")"),
        ("./.", "Path(\"/work/sub\")"),
        ("./..", "Path(\"/work\")"),
        ("./a/../b", "Path(\"/work/sub/b\")"),
        ("../../../x", "Path(\"/x\")"),
        ("/etc/../etc/hosts", "Path(\"/etc/hosts\")"),
        ("~/foo/../bar", "Path(\"/home/u/foo/../bar\")"),
    ] {
        let ir = lower(src);
        assert!(
            ir.contains(expected),
            "{src}: expected {expected} in:\n{ir}"
        );
    }
}

#[test]
fn interpolated_paths_resolve_only_the_leading_literal() {
    // nix-instantiate --parse -E 'x: ./a/${x}/c/../d' => (/work/sub/a/ + x + "/c/../d")
    let ir = lower("x: ./a/${x}/c/../d");
    assert!(ir.contains("Path(\"/work/sub/a/\")"), "{ir}");
    assert!(ir.contains("String(\"/c/../d\")"), "{ir}");
    // nix-instantiate --parse -E 'x: ./../../${x}' in /work/sub => (// + x)
    assert!(lower("x: ./../../${x}").contains("Path(\"//\")"));
    assert!(lower("x: ~/a/${x}").contains("Path(\"/home/u/a/\")"));
}

#[test]
fn cur_pos_file_is_absolute() {
    let ir = lower("__curPos");
    assert!(ir.contains("String(\"/work/sub/test.nix\")"), "{ir}");
}

#[test]
fn home_path_without_home_dir_is_an_error() {
    let mut ctx = Context::new();
    let src = "~/x";
    let file = test_file(&ctx, src);
    let parse = ast::Root::parse(src);
    let res = lower_root_with(&mut ctx, &file, &parse.tree(), &LowerOptions::default());
    let Err(err) = res else {
        panic!("expected an error")
    };
    assert!(matches!(&err.kind, ErrorKind::NoHomeDir(p) if p == "~/x"));
}

#[test]
fn defaults_can_refer_to_later_formals() {
    // Nix: ({ a ? b, b ? 1 }: a) {} == 1
    let ir = lower("{ a ? b, b ? 1 }: a");
    assert!(ir.contains("nix_hir.let_in"), "{ir}");
    assert!(ir.contains("nix_hir.var \"b\""), "{ir}");
    assert!(ir.contains("nix_hir.var \"a\""), "{ir}");
    assert!(!ir.contains("nix_hir.lookup"), "{ir}");

    // Nix: ({ a ? b + 1, b ? c * 10, c ? 1 }: a) {} == 11
    let ir = lower("{ a ? b + 1, b ? c * 10, c ? 1 }: a");
    assert!(ir.contains("nix_hir.var \"c\""), "{ir}");
    assert!(!ir.contains("nix_hir.lookup"), "{ir}");
}

#[test]
fn formals_shadow_outer_names() {
    // Nix: (x: { x ? 1, a ? x }: a) 7 {} == 1, so `x` is the formal.
    let ir = lower("x: { x ? 1, a ? x }: a");
    assert!(ir.contains("nix_hir.var \"x\""), "{ir}");
}

#[test]
fn plain_formals_stay_ssa() {
    let ir = lower("{ a, b }: a + b");
    assert!(!ir.contains("nix_hir.let_in"), "{ir}");
    assert!(!ir.contains("nix_hir.var"), "{ir}");
    // A default referring to a plain formal uses the SSA value.
    let ir = lower("{ a, b ? a }: b");
    assert!(!ir.contains("nix_hir.var \"a\""), "{ir}");
    assert!(ir.contains("nix_hir.var \"b\""), "{ir}");
}

#[test]
fn dynamic_let_binding_is_an_error() {
    let (_, res) = try_lower(r#"let ${"a" + "b"} = 1; in 2"#);
    let Err(err) = res else {
        panic!("expected an error")
    };
    assert!(matches!(err.kind, ErrorKind::DynamicAttrNotAllowed("let")));
    assert_eq!(err.span(), Some(SourceSpan::from(4..16)));
}

#[test]
fn parse_errors_are_located() {
    let ctx = Context::new();
    let src = "{ a = 1 }";
    let file = test_file(&ctx, src);
    let errs = file
        .check_parse(ast::Root::parse(src).errors())
        .unwrap_err();
    // Only the real error, not rnix's follow-up end-of-file errors.
    assert_eq!(errs.errors.len(), 1, "{errs:?}");
    assert_eq!(errs.errors[0].span(), Some(SourceSpan::from(8..9)));
}

#[test]
fn lowers_kitchen_sink() {
    let ir = lower(
        r#"
        { lib ? null, pkgs, ... }@args:
        let
          inherit (pkgs) hello;
          name = "hi-${hello.pname or "x"}";
          rec' = rec { a = 1; b = a; };
        in
        assert args ? pkgs;
        {
          inherit name;
          p = ./foo/${name};
          s = <nixpkgs>;
          ok = !(rec'.b == 1) || -1 < 2;
          l = [ 1 2.5 ] ++ [ "x" ];
          m = { x = 1; } // { y = 2; };
          f = 1 |> (x: x);
          u = https://example.com;
          pos = __curPos;
          ${name} = if true then null else false;
        }
        "#,
    );
    for needle in [
        "nix_hir.formals",
        "nix_hir.rec_attr_set",
        "nix_hir.interpolate",
        "nix_hir.assert",
        "nix_hir.has_attr",
        "nix_hir.dyn_bind",
        "nix_hir.apply",
        "nix_hir.if",
    ] {
        assert!(ir.contains(needle), "missing {needle} in:\n{ir}");
    }
}

/// The `.nix` files in `samples/<dir>` at the root of the repository.
fn samples(dir: &str) -> Vec<(String, String)> {
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

#[test]
fn every_valid_sample_lowers_and_round_trips() {
    for (name, src) in samples("valid") {
        eprintln!("{name}");
        assert_round_trips(&src);
    }
}

#[test]
fn every_invalid_sample_is_rejected() {
    for (name, src) in samples("invalid") {
        let mut ctx = Context::new();
        let file = test_file(&ctx, &src);
        let parse = ast::Root::parse(&src);
        let rejected = file.check_parse(parse.errors()).is_err()
            || lower_root_with(&mut ctx, &file, &parse.tree(), &LowerOptions::default()).is_err();
        assert!(rejected, "{name} was accepted");
    }
}
