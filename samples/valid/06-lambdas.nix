# Lambdas: plain parameters, currying, patterns, defaults, ellipsis, @-bindings.
let
  id = x: x;
  add = x: y: x + y;
  greet = { name, greeting ? "Hello" }: "${greeting}, ${name}";
  open = { a, ... }: a;
  whole = args@{ a, b ? 2, ... }: a + b + (args.c or 0);
  wholePrefix = { a, ... }@args: builtins.attrNames args;
  defaultUsesFormal = { a, b ? a * 10 }: b;
  # Defaults may refer to any formal, including later ones.
  defaultUsesLaterFormal = { a ? b + 1, b ? 10 }: a;
in
{
  idResult = id 7;
  partial = (add 1) 2;
  greetDefault = greet { name = "nix"; };
  greetCustom = greet { name = "nix"; greeting = "Hi"; };
  openResult = open { a = 1; extra = true; };
  wholeResult = whole { a = 1; c = 3; };
  wholePrefixResult = wholePrefix { a = 1; z = 2; };
  defaultUsesFormalResult = defaultUsesFormal { a = 4; };
  defaultUsesLaterFormalResult = defaultUsesLaterFormal { };          # 11
  defaultUsesLaterFormalGiven = defaultUsesLaterFormal { b = 1; };    # 2
}
