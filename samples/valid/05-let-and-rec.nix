# Recursive bindings: let ... in and rec { }.
let
  a = 1;
  b = a + 1;            # refers to an earlier binding
  c = d * 2;            # refers to a later binding
  d = 5;
  fib = n: if n < 2 then n else fib (n - 1) + fib (n - 2);   # self-recursive
in
{
  inherit a b c d;
  fib10 = fib 10;
  recSet = rec {
    one = 1;
    two = one + 1;
    three = two + one;
  };
  nestedLet = let a = 100; in let b = a + 1; in b;
}
