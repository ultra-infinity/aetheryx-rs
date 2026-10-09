# if/then/else, assert, and with.
let
  check = x: assert x > 0; x * 2;
  scope = { inner = "from with"; shadowed = "from with"; };
  shadowed = "from let";
in
{
  ifTrue = if 1 < 2 then "yes" else "no";
  ifLazy = if true then "ok" else throw "never evaluated";
  nestedIf = if false then 1 else if true then 2 else 3;
  asserted = check 21;
  withScope = with scope; inner;
  # Lexical bindings win over `with`: this is "from let".
  withDoesNotShadow = with scope; shadowed;
  nestedWith = with { a = 1; }; with { a = 2; b = 3; }; a + b;   # inner `with` wins: 5
  withBuiltins = with builtins; length [ 1 2 3 ];
}
