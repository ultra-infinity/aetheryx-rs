//! Tests of the HIR to MIR lowering.

use pliron::builtin::op_interfaces::SingleBlockRegionInterface;
use pliron::irfmt::parsers::spaced;
use pliron::op::{op_impls, op_impls_static};
use pliron::operation::verify_operation;
use pliron::parsable::parse_from_str;
use pliron::printable::Printable;
use pliron::r#type::type_cast;
use pliron::value::Value;

use super::*;
use crate::builtins_dialect::RefOp;
use crate::mir::{self, Ty};
use crate::test_util::{assert_round_trips, samples, try_lower_hir};

/// Lowers `src` to MIR; the module must verify.
fn lower(src: &str) -> (Context, ModuleOp) {
    let (mut ctx, module) = try_lower_hir(src);
    let module = module.unwrap();
    match lower_module(&mut ctx, module) {
        Ok(()) => (ctx, module),
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

// ─── Single passes ──────────────────────────────────────────────────────────

/// Parses `@main`'s body `body` (MIR text) into a module.
fn parse_main(ctx: &mut Context, body: &str) -> Ptr<Operation> {
    let text = format!(
        "builtin.module @m {{ ^b(): builtin.func @main: builtin.function <() -> (nix_mir.value)> \
         {{ ^e(): {body} }} }}"
    );
    let parser = spaced(Operation::top_level_parser());
    parse_from_str(parser, ctx, &text).unwrap_or_else(|e| panic!("{text}\n{}", e.disp(ctx)))
}

fn run(pass: &mut impl Pass, ctx: &mut Context, op: Ptr<Operation>) -> Result<()> {
    pass.run(op, ctx, &mut AnalysisManager::default())
        .map(|_| ())
}

/// A thunk `v0` of `1`, forced by `uses`; `@main` yields `v9`.
fn thunk_forced_by(uses: &str) -> String {
    format!(
        "v0 = nix_mir.thunk () : nix_mir.thunk {{ ^t(): \
             v1 = nix_mir.const Int(1) : nix_mir.value; nix_mir.yield v1 }}; \
         {uses} nix_mir.yield v9"
    )
}

/// An empty `@main` of type `() -> (nix_mir.value)`, and its body.
/// (pliron cannot parse implicit captures, which the passes before
/// `convert-closures` produce, so such IR is built directly.)
fn empty_main(ctx: &mut Context) -> (Ptr<Operation>, Ptr<BasicBlock>) {
    let module = ModuleOp::new(ctx, "m".try_into().unwrap());
    let value_ty = Ty::Value.handle(ctx);
    let func_ty = FunctionType::get(ctx, vec![], vec![value_ty]);
    let main = FuncOp::new(ctx, "main".try_into().unwrap(), func_ty);
    module.append_operation(ctx, main.get_operation(), 0);
    (module.get_operation(), main.get_entry_block(ctx))
}

/// Appends `op` to `block` and returns its result, if it has one.
fn append(ctx: &Context, block: Ptr<BasicBlock>, op: &impl Op) -> Option<Value> {
    let op = op.get_operation();
    op.insert_at_back(block, ctx);
    let opr = op.deref(ctx);
    (opr.get_num_results() == 1).then(|| opr.get_result(0))
}

/// Appends a thunk of `1` to `block`.
fn thunk_of_one(ctx: &mut Context, block: Ptr<BasicBlock>) -> Value {
    let thunk = mir::ThunkOp::new(ctx);
    let one = mir::ConstOp::new(ctx, hir::Literal::Int(1));
    let one = append(ctx, thunk.body(ctx), &one).unwrap();
    let op = mir::YieldOp::new(ctx, one);
    append(ctx, thunk.body(ctx), &op);
    append(ctx, block, &thunk).unwrap()
}

#[test]
fn elision_never_moves_computation_into_a_lambda() {
    // The lambda may run many times; the thunk must still run at most once.
    let ctx = &mut Context::new();
    let (module, body) = empty_main(ctx);
    let thunk = thunk_of_one(ctx, body);
    let lambda = mir::LambdaOp::new(ctx);
    let force = mir::ForceOp::new(ctx, thunk);
    let forced = append(ctx, lambda.body(ctx), &force).unwrap();
    let op = mir::YieldOp::new(ctx, forced);
    append(ctx, lambda.body(ctx), &op);
    let lambda = append(ctx, body, &lambda).unwrap();
    let op = mir::YieldOp::new(ctx, lambda);
    append(ctx, body, &op);

    run(&mut ElideThunks, ctx, module).unwrap();
    let mir = module.disp(ctx).to_string();
    assert_eq!(count(&mir, "nix_mir.thunk"), 1, "{mir}");
    assert_eq!(count(&mir, "nix_mir.force"), 1, "{mir}");
}

#[test]
fn elision_computes_a_thunk_at_its_only_force() {
    // Forced on one branch only: computed on that branch.
    let ctx = &mut Context::new();
    let module = parse_main(
        ctx,
        &thunk_forced_by(
            "v4 = nix_mir.const Bool(true) : nix_mir.value; \
             v9 = nix_mir.if v4 : nix_mir.value then { ^a(): \
                 v5 = nix_mir.force v0 : nix_mir.value; nix_mir.yield v5 } \
             else { ^c(): v6 = nix_mir.const Int(2) : nix_mir.value; nix_mir.yield v6 };",
        ),
    );
    run(&mut ElideThunks, ctx, module).unwrap();
    let mir = module.disp(ctx).to_string();
    assert_eq!(count(&mir, "nix_mir.thunk"), 0, "{mir}");
    assert_eq!(count(&mir, "nix_mir.force"), 0, "{mir}");
    let then_branch = &mir[mir.find("then").unwrap()..mir.find("else").unwrap()];
    assert!(then_branch.contains("Int(1)"), "{mir}");
}

#[test]
fn thunks_forced_twice_stay_suspended() {
    let ctx = &mut Context::new();
    let module = parse_main(
        ctx,
        &thunk_forced_by(
            "v2 = nix_mir.force v0 : nix_mir.value; v3 = nix_mir.force v0 : nix_mir.value; \
             v9 = nix_mir.binary v2 Add v3 : nix_mir.value;",
        ),
    );
    run(&mut ElideThunks, ctx, module).unwrap();
    let mir = module.disp(ctx).to_string();
    assert_eq!(count(&mir, "nix_mir.thunk"), 1, "{mir}");
    assert_eq!(count(&mir, "nix_mir.force"), 2, "{mir}");
}

#[test]
fn captures_go_through_every_enclosing_closure() {
    // `x` is used in the innermost lambda, so the middle one captures it too.
    let mir = mir_text("x: y: z: x");
    let lambdas: Vec<_> = mir
        .lines()
        .filter(|l| l.contains("nix_mir.lambda"))
        .collect();
    assert_eq!(lambdas.len(), 3, "{mir}");
    assert!(lambdas[0].contains("lambda ()"), "{mir}");
    assert!(!lambdas[1].contains("lambda ()"), "{mir}");
    assert!(!lambdas[2].contains("lambda ()"), "{mir}");
}

#[test]
fn closure_conversion_makes_regions_isolated() {
    // A thunk using a value defined outside it, which does not verify.
    let ctx = &mut Context::new();
    let (module, body) = empty_main(ctx);
    let one = mir::ConstOp::new(ctx, hir::Literal::Int(1));
    let one = append(ctx, body, &one).unwrap();
    let thunk = mir::ThunkOp::new(ctx);
    let op = mir::YieldOp::new(ctx, one);
    append(ctx, thunk.body(ctx), &op);
    let thunk = append(ctx, body, &thunk).unwrap();
    let op = mir::ForceOp::new(ctx, thunk);
    let forced = append(ctx, body, &op).unwrap();
    let op = mir::YieldOp::new(ctx, forced);
    append(ctx, body, &op);
    assert!(verify_operation(module, ctx).is_err());

    run(&mut ConvertClosures, ctx, module).unwrap();
    if let Err(e) = verify_operation(module, ctx) {
        panic!("{}\n{}", e.disp(ctx), module.disp(ctx));
    }
    let mir = module.disp(ctx).to_string();
    assert!(mir.contains("nix_mir.thunk (v"), "{mir}");
}

#[test]
fn check_mir_rejects_leftover_hir() {
    let (mut ctx, module) = try_lower_hir("1");
    let module = module.unwrap().get_operation();
    let err = run(&mut CheckMir, &mut ctx, module).expect_err("HIR is not MIR");
    assert!(
        err.disp(&ctx)
            .to_string()
            .contains("left over from nix_hir"),
        "{}",
        err.disp(&ctx)
    );
}

#[test]
fn every_hir_op_converts_itself() {
    fn converts<O: Op>() -> bool {
        op_impls_static::<O, dyn ToMIRDialect>()
    }
    let all = [
        converts::<hir::ConstOp>(),
        converts::<hir::ListOp>(),
        converts::<hir::AttrSetOp>(),
        converts::<hir::RecAttrSetOp>(),
        converts::<hir::LetInOp>(),
        converts::<hir::BindOp>(),
        converts::<hir::DynBindOp>(),
        converts::<hir::LambdaOp>(),
        converts::<hir::FormalsOp>(),
        converts::<hir::ApplyOp>(),
        converts::<hir::IfOp>(),
        converts::<hir::AssertOp>(),
        converts::<hir::WithOp>(),
        converts::<hir::SelectOp>(),
        converts::<hir::HasAttrOp>(),
        converts::<hir::UnaryOp>(),
        converts::<hir::BinaryOp>(),
        converts::<hir::InterpolateOp>(),
        converts::<hir::VarOp>(),
        converts::<hir::LookupOp>(),
        converts::<hir::YieldOp>(),
        converts::<RefOp>(),
        converts::<FuncOp>(),
    ];
    assert!(all.iter().all(|&c| c));
    // And every op the samples lower to.
    for (name, src) in samples("valid") {
        let (ctx, module) = try_lower_hir(&src);
        let module = module.unwrap();
        let body = module.get_body(&ctx, 0);
        for op in body
            .deref(&ctx)
            .iter(&ctx)
            .flat_map(|op| nested_ops(&ctx, op))
        {
            let op_dyn = Operation::get_op_dyn(op, &ctx);
            assert!(
                op_impls::<dyn ToMIRDialect>(op_dyn.op_ref()),
                "{name}: {}",
                Operation::get_opid(op, &ctx)
            );
        }
    }
}

#[test]
fn hir_values_convert_to_thunks() {
    let ctx = &Context::new();
    let hir_value: TypeHandle = hir::ValueType::get(ctx).into();
    let converted = type_cast::<dyn ToMIRType>(&*hir_value.deref(ctx))
        .unwrap()
        .convert(ctx)
        .unwrap();
    assert_eq!(converted, Ty::Thunk.handle(ctx));
    // Functions take thunks and return values.
    let main = FunctionType::get(ctx, vec![hir_value], vec![hir_value]);
    let main: TypeHandle = main.into();
    let converted = type_cast::<dyn ToMIRType>(&*main.deref(ctx))
        .unwrap()
        .convert(ctx)
        .unwrap();
    let expected = FunctionType::get(
        ctx,
        vec![Ty::Thunk.handle(ctx)],
        vec![Ty::Value.handle(ctx)],
    );
    assert_eq!(converted, expected.into());
}
