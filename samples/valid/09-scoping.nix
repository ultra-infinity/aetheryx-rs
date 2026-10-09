# Name-resolution corner cases. Each comment gives the value real Nix produces.
let
  x = "let x";
in
{
  # A lambda parameter shadows the outer let: "param".
  paramShadowsLet = (x: x) "param";
  # An inner let shadows a lambda parameter: "inner let".
  letShadowsParam = (x: let x = "inner let"; in x) "param";
  # `inherit x;` inside a let refers to the *enclosing* x: "let x".
  inheritOuter = let inherit x; in x;
  # `inherit (s) x;` takes x from s: "from set".
  inheritFrom = let s = { x = "from set"; }; inherit (s) x; in x;
  # rec binds its own names: 3.
  recSelf = (rec { a = 1; b = a + 2; }).b;
  # A non-rec set does not: "let x".
  nonRecOuter = { x = "set x"; y = x; }.y;
  # true/false/null can be shadowed: 1.
  shadowTrue = let true = 1; in true;
  # with does not shadow a lambda parameter: "param".
  withVsParam = (x: with { x = "with x"; }; x) "param";
}
