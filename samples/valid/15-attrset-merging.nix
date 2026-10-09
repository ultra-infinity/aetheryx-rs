# Attribute set merging rules (as implemented by Nix 2.28).
let
  x = "outer x";
in
{
  # Paths and literals merge, recursively.
  paths = { a.b = 1; a.c = 2; };
  pathIntoLiteral = { a = { b.c = 1; }; a.b.d = 2; };
  literalIntoLiteral = { a = { b = { c = 1; }; }; a = { b = { d = 2; }; }; };

  # The FIRST definition decides whether the merged set is `rec`, and every
  # merged binding is evaluated in that scope: `y` is 1, not "outer x".
  firstIsRec = { a = rec { x = 1; }; a.y = x; };

  # A `rec` on a later literal is ignored: `z` is "outer x".
  laterRecIgnored = { a = { w = 1; }; a = rec { x = 2; z = x; }; };

  # `inherit (e)` entries merge too.
  inheritMerges = { a = { inherit ({ q = 1; }) q; }; a.r = 2; };
}
