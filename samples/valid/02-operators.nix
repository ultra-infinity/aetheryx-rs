# Every binary and unary operator, including precedence and associativity.
{
  arith = 1 + 2 * 3 - 8 / 2;
  negate = -(5 - 7);
  floatDiv = 7.0 / 2;
  concatStrings = "foo" + "bar";
  lessThan = 1 < 2;
  lessEq = 2 <= 2;
  greater = 3 > 4;
  greaterEq = 4 >= 4;
  equal = [ 1 2 ] == [ 1 2 ];
  notEqual = { a = 1; } != { a = 2; };
  and = true && false;
  or = false || true;
  not = !false;
  implies = false -> false;
  concatLists = [ 1 ] ++ [ 2 3 ] ++ [ ];
  update = { a = 1; b = 1; } // { b = 2; c = 3; };
  shortCircuitAnd = false && throw "never evaluated";
  shortCircuitOr = true || throw "never evaluated";
}
