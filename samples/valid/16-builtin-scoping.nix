# How builtins are scoped. The values in comments are what Nix produces.
# Builtins live in the outermost *lexical* scope: 23 under their bare name
# (map, toString, import, true, ...), the rest only as `__name` (__add, ...).
{
  # Lexical globals beat `with`: <PRIMOP> (the builtin), not 1.
  globalBeatsWith = builtins.typeOf (with { map = 1; }; map);          # "lambda"
  # `length` is not a bare global, so `with` provides it: 1.
  withProvidesNonGlobal = with { length = 1; }; length;               # 1
  # ...which is why `with builtins;` makes `length` available.
  withBuiltins = with builtins; length [ 1 2 3 ];                     # 3
  # let and lambda bindings shadow builtins.
  letShadows = let map = 1; in map;                                   # 1
  paramShadows = (map: map) 2;                                        # 2
  # Prefixed globals.
  prefixed = __add 1 2;                                               # 3
  # Builtins are values: partial application and passing them around.
  partial = let inc = map (x: x + 1); in inc [ 1 2 ];                 # [ 2 3 ]
  passed = (f: f [ 3 1 2 ]) (builtins.sort builtins.lessThan);        # [ 1 2 3 ]
  # `builtins` is an attribute set.
  count = builtins.length (builtins.attrNames builtins);              # 118
  hasMap = builtins ? map;                                            # true
}
