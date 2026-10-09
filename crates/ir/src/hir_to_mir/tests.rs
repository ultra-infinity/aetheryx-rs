//! Tests of the HIR to MIR lowering.

use pliron::printable::Printable;

use super::*;
use crate::test_util::{assert_round_trips, samples, try_lower_hir};

/// Lowers `src` to MIR; the module must verify.
fn lower(src: &str) -> (Context, ModuleOp) {
    let (mut ctx, hir) = try_lower_hir(src);
    let hir = hir.unwrap();
    match lower_module(&mut ctx, hir) {
        Ok(mir) => (ctx, mir),
        Err(e) => panic!("MIR does not verify: {}", e.disp(&ctx)),
    }
}

fn mir_text(src: &str) -> String {
    let (ctx, module) = lower(src);
    module.get_operation().disp(&ctx).to_string()
}

fn count(text: &str, op: &str) -> usize {
    text.matches(&format!("= {op} ")).count() + text.matches(&format!("\n{op} ")).count()
}

#[test]
fn every_valid_sample_lowers_verifies_and_round_trips() {
    for (name, src) in samples("valid") {
        eprintln!("{name}");
        let (mut ctx, module) = lower(&src);
        assert_round_trips(&mut ctx, module.get_operation());
    }
}

#[test]
fn strict_operands_are_computed_inline() {
    // Nix evaluates operands of arithmetic directly, without thunks.
    let mir = mir_text("1 + 2 * 3");
    assert_eq!(count(&mir, "nix_mir.thunk"), 0, "{mir}");
    assert_eq!(count(&mir, "nix_mir.binary"), 2, "{mir}");
}

#[test]
fn lazy_positions_get_thunks() {
    // Nix: `(x: 1) (throw "a")` is 1, so the argument must not be evaluated.
    let mir = mir_text(r#"(x: 1) (throw "a")"#);
    assert_eq!(count(&mir, "nix_mir.thunk"), 1, "{mir}");
    assert!(mir.contains("nix_mir.call"), "{mir}");
    // List elements are lazy too, but constants are never thunked.
    let mir = mir_text("[ (1 + 1) 2 ]");
    assert_eq!(count(&mir, "nix_mir.thunk"), 1, "{mir}");
    assert_eq!(count(&mir, "nix_mir.evaluated"), 1, "{mir}");
}

#[test]
fn recursive_bindings_form_one_group() {
    let mir = mir_text("let a = 1; b = a + 1; in b");
    assert_eq!(count(&mir, "nix_mir.letrec"), 1, "{mir}");
    assert!(mir.contains(": nix_mir.thunk , nix_mir.thunk "), "{mir}");
    // `rec { }` too, and its dynamic keys see the recursive names.
    let mir = mir_text(r#"rec { a = "b"; ${a} = 1; }"#);
    assert_eq!(count(&mir, "nix_mir.letrec"), 1, "{mir}");
    assert_eq!(count(&mir, "nix_mir.attrs_insert"), 1, "{mir}");
}

#[test]
fn with_lookups_get_the_namespaces_innermost_first() {
    let mir = mir_text("with { a = 1; }; with { b = 2; }; a + b");
    assert_eq!(count(&mir, "nix_mir.with_lookup"), 2, "{mir}");
    // Both lookups search both namespaces.
    assert_eq!(
        mir.matches("nix_mir.with_lookup \"a\" (").count(),
        1,
        "{mir}"
    );
    let line = mir
        .lines()
        .find(|l| l.contains("with_lookup \"a\""))
        .unwrap();
    assert_eq!(line.matches(", ").count(), 1, "{line}");
}

#[test]
fn formals_are_checked_even_if_unused() {
    // Nix: `({ a }: 1) { }` fails, so the check must not be lazy.
    let mir = mir_text("{ a }: 1");
    assert!(mir.contains("nix_mir.formals"), "{mir}");
    assert!(mir.contains("{ a }"), "{mir}");
}

#[test]
fn boolean_operators_short_circuit() {
    // Nix: `false && throw "x"` is false and `true && 5` is a type error.
    let mir = mir_text(r#"false && throw "x""#);
    assert!(mir.contains("nix_mir.if"), "{mir}");
    assert!(mir.contains("nix_mir.check_bool"), "{mir}");
    assert!(!mir.contains(" And "), "{mir}");
    // The right side is computed in its branch, not thunked.
    assert_eq!(count(&mir, "nix_mir.thunk"), 0, "{mir}");
}

#[test]
fn lambdas_are_named_after_their_binding() {
    // Nix reports `function 'f' called without required argument 'a'`.
    let mir = mir_text("let f = { a }: a; in f");
    assert!(mir.contains("nix_mir.lambda [name : \"f\"]"), "{mir}");
    assert!(!mir_text("{ a }: a").contains("[name"));
}

#[test]
fn closures_capture_explicitly() {
    let mir = mir_text("x: y: x");
    // The inner lambda captures the outer argument.
    let inner = mir
        .lines()
        .filter(|l| l.contains("nix_mir.lambda"))
        .nth(1)
        .unwrap();
    assert!(!inner.contains("()"), "{mir}");
}
