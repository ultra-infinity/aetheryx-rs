//! Tests of the interpreter. Expected results are what Nix 2.28 produces.

use super::*;

/// Evaluates `src` as `/work/test.nix` and prints the result, on a thread
/// with a stack as large as the driver's.
fn eval(src: &str) -> std::result::Result<String, String> {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || eval_here(&src))
        .unwrap()
        .join()
        .unwrap()
}

fn eval_here(src: &str) -> std::result::Result<String, String> {
    let ctx = Context::new();
    let file = SourceFile::with_cwd(&ctx, "test.nix", src, std::path::Path::new("/work")).unwrap();
    let options = LowerOptions {
        home_dir: Some("/home/u".to_string()),
    };
    evaluate(ctx, &file, &options).map_err(|e| e.to_string())
}

fn ok(src: &str) -> String {
    eval(src).unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn err(src: &str) -> String {
    eval(src).expect_err(src)
}

#[test]
fn arithmetic_and_printing() {
    assert_eq!(ok("1 + 2 * 3"), "7");
    assert_eq!(ok("7 / 2"), "3");
    assert_eq!(ok("1.5e3"), "1500");
    assert_eq!(ok("-(5 - 7)"), "2");
    assert_eq!(ok(r#""a${"b"}c""#), r#""abc""#);
    assert_eq!(ok("{ b = 1; a = [ 1 2 ]; }"), "{ a = [ 1 2 ]; b = 1; }");
}

#[test]
fn laziness() {
    assert_eq!(ok(r#"(x: 1) (throw "a")"#), "1");
    assert_eq!(ok(r#"let x = throw "x"; in 1"#), "1");
    assert_eq!(ok(r#"with throw "ns"; 1"#), "1");
    assert_eq!(ok(r#"false && throw "rhs""#), "false");
    assert_eq!(ok(r#"true || throw "rhs""#), "true");
    assert_eq!(ok(r#"builtins.length [ (throw "elem") ]"#), "1");
    assert_eq!(ok(r#"{ a = throw "v"; } ? a"#), "true");
}

#[test]
fn errors_match_nix() {
    assert_eq!(err(r#"throw "l" + throw "r""#), "l");
    assert_eq!(
        err("true && 5"),
        "expected a Boolean but found an integer: 5"
    );
    assert_eq!(err("let x = x; in x"), "infinite recursion encountered");
    assert_eq!(err("{ a = 1; }.b"), "attribute 'b' missing");
    assert_eq!(
        err("1 2"),
        "attempt to call something which is not a function but an integer: 1"
    );
    assert_eq!(
        err("({ a }: a) {}"),
        "function 'anonymous lambda' called without required argument 'a'"
    );
    assert_eq!(
        err("let f = { a }: a; in f { }"),
        "function 'f' called without required argument 'a'"
    );
    assert_eq!(err("with {}; x"), "undefined variable 'x'");
    assert_eq!(
        err("9223372036854775807 + 1"),
        "integer overflow in adding 9223372036854775807 + 1"
    );
    // Constant `${"a"}` keys are static, so this is a compile-time duplicate.
    assert_eq!(
        err(r#"{ ${"a"} = 1; ${"a"} = 2; }"#),
        "attribute 'a' already defined at /work/test.nix:1:3"
    );
}

#[test]
fn errors_point_at_the_failing_expression() {
    let ctx = Context::new();
    let src = "let x = 1; in x + \"a\"";
    let file = SourceFile::with_cwd(&ctx, "t.nix", src, std::path::Path::new("/w")).unwrap();
    let Err(EvalError::Source(e)) = evaluate(ctx, &file, &LowerOptions::default()) else {
        panic!("expected an evaluation error");
    };
    assert_eq!(e.span().map(|s| (s.offset(), s.len())), Some((14, 7)));
}

#[test]
fn recursion() {
    assert_eq!(
        ok("let f = n: if n == 0 then 1 else n * f (n - 1); in f 10"),
        "3628800"
    );
    assert_eq!(ok("rec { a = 1; b = a + 1; }.b"), "2");
    assert_eq!(ok("({ a ? b + 1, b ? 10 }: a) { }"), "11");
    assert_eq!(
        ok("let fib = n: if n < 2 then n else fib (n - 1) + fib (n - 2); in fib 15"),
        "610"
    );
}

#[test]
fn call_depth_is_limited_like_nix() {
    // Nix 2.28 (max-call-depth = 10000): 9999 succeeds, 10000 fails.
    let count =
        |n: u32| format!("let count = n: if n == 0 then 0 else 1 + count (n - 1); in count {n}");
    assert_eq!(ok(&count(9999)), "9999");
    assert_eq!(
        err(&count(10000)),
        "stack overflow; max-call-depth exceeded"
    );
    // Iteration in a builtin does not nest calls.
    assert_eq!(
        ok("builtins.foldl' (a: b: a + b) 0 (builtins.genList (x: x) 50000)"),
        "1249975000"
    );
}

#[test]
fn scoping() {
    assert_eq!(
        ok("builtins.typeOf (with { map = 1; }; map)"),
        r#""lambda""#
    );
    assert_eq!(ok("with { length = 1; }; length"), "1");
    assert_eq!(
        ok(r#"let x = "let"; in (x: with { x = "with"; }; x) "param""#),
        r#""param""#
    );
    assert_eq!(ok("with { a = 1; }; with { a = 2; b = 3; }; a + b"), "5");
}

#[test]
fn lambdas_are_named_like_nix() {
    // Nix passes a binding's name on to a lambda that is directly the body
    // of the named one, through formals but not through `let` or `if`.
    let missing_x = |name: &str| format!("function '{name}' called without required argument 'x'");
    for (src, name) in [
        ("let h = y: { x }: x; in h 1 {}", "h"),
        ("let h = { a }: { x }: x; in h { a = 1; } {}", "h"),
        ("let h = { a ? 1 }: { x }: x; in h {} {}", "h"),
        ("let h = { a ? 1 }@args: { x }: x; in h {} {}", "h"),
        ("let h = y: z: { x }: x; in h 1 2 {}", "h"),
        ("{ f = a: b: { x }: x; }.f 1 2 {}", "f"),
        (
            "let h = y: let q = 1; in { x }: x; in h 1 {}",
            "anonymous lambda",
        ),
        (
            "let h = { a ? 1 }: let q = 1; in { x }: x; in h {} {}",
            "anonymous lambda",
        ),
        (
            "let h = y: if true then { x }: x else 1; in h 1 {}",
            "anonymous lambda",
        ),
    ] {
        assert_eq!(err(src), missing_x(name), "{src}");
    }
}

#[test]
fn bindings_resolve_like_nix() {
    for (src, expected) in [
        ("let a = b; b = 1; in a", "1"),
        ("let a = 1; b = a; c = b; in c", "1"),
        ("let x = 1; in let x = 2; y = x; in y", "2"),
        ("let x = 1; in let inherit x; y = x + 1; in y", "2"),
        ("rec { a = 1; b = { c = a; }; }.b.c", "1"),
        // Dynamic keys are evaluated in the recursive scope.
        (r#"rec { k = "z"; ${k} = k; }"#, r#"{ k = "z"; z = "z"; }"#),
        (
            r#"let k = "o"; in rec { ${k} = 1; k = "i"; }"#,
            r#"{ i = 1; k = "i"; }"#,
        ),
        ("({ a ? b, b ? a }: a) { b = 3; }", "3"),
        (
            r#"(rec { f = x: if x == 0 then "done" else f (x - 1); }).f 3"#,
            r#""done""#,
        ),
        // `let` bindings shadow `with`, whichever is innermost.
        ("with { a = 1; }; let a = 2; in a", "2"),
        ("let a = 2; in with { a = 1; }; a", "2"),
        ("with { a = 1; }; with { b = 2; }; let c = a + b; in c", "3"),
        ("(x: y: z: x) 1 2 3", "1"),
        (
            r#"let xs = [ (throw "a") 2 ]; in builtins.elemAt xs 1"#,
            "2",
        ),
    ] {
        assert_eq!(ok(src), expected, "{src}");
    }
}

#[test]
fn builtins_and_dynamic_attrs() {
    assert_eq!(ok("map (x: x * 2) [ 1 2 3 ]"), "[ 2 4 6 ]");
    assert_eq!(ok("builtins.length (builtins.attrNames builtins)"), "118");
    assert_eq!(ok("{ ${null} = 1; }"), "{ }");
    assert_eq!(
        ok(r#"rec { a = "b"; ${a} = 1; }"#),
        r#"{ a = "b"; b = 1; }"#
    );
    assert_eq!(ok("let a = [ 1 ]; in [ a a ]"), "[ [ 1 ] «repeated» ]");
}

#[test]
fn runtime_and_compiler_agree_on_paths() {
    for path in [
        "/..",
        "/a/../..",
        "/./a",
        "/etc/../etc/hosts",
        "/x/y//z",
        "/x/",
        "/",
    ] {
        assert_eq!(
            runtime::paths::canon_path(path),
            ir::paths::canon_path(path),
            "{path}"
        );
    }
}

/// Evaluates `src` on a thread with only `stack` bytes of stack.
fn eval_with_stack(src: String, stack: usize) -> std::result::Result<String, String> {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || eval_here(&src))
        .unwrap()
        .join()
        .expect("evaluation must fail with an error, not crash")
}

#[test]
fn running_out_of_stack_is_an_error_like_nix() {
    const STACK: usize = 4 * 1024 * 1024;
    let overflow = Err("stack overflow (possible infinite recursion)".to_string());
    // Deep syntax (Nix 2.28 reports this error for a sum of 200000 terms).
    let sum = vec!["1"; 200_000].join(" + ");
    assert_eq!(eval_with_stack(sum, STACK), overflow);
    // Deep evaluation, still under max-call-depth.
    let count = "let count = n: if n == 0 then 0 else 1 + count (n - 1); in count 9000";
    assert_eq!(eval_with_stack(count.to_string(), STACK), overflow);
    // Deep nesting either evaluates or fails the same way, depending on how
    // much stack the build's frames use; it never crashes. (Depth 5000 keeps
    // rnix's quadratic parse of nested lists fast.)
    let lists = "[".repeat(5000) + &"]".repeat(5000);
    let result = eval_with_stack(lists, STACK);
    assert!(result.is_ok() || result == overflow, "{result:?}");
}
