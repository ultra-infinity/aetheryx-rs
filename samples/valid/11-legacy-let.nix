# Deprecated `let { ... }` syntax: evaluates to the `body` attribute.
let {
  a = 1;
  b = a + 1;
  body = b * 10;
}
