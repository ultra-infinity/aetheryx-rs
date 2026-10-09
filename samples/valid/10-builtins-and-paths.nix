# Builtins, __curPos and path forms that need care in the evaluator.
{
  pos = __curPos;
  mapped = map (x: x * 2) [ 1 2 3 ];
  filtered = builtins.filter (x: x > 1) [ 1 2 3 ];
  folded = builtins.foldl' (acc: x: acc + x) 0 [ 1 2 3 4 ];
  attrNames = builtins.attrNames { b = 1; a = 2; };
  typeOfs = map builtins.typeOf [ 1 1.0 "s" ./. true null [ ] { } (x: x) ];
  toStr = toString [ 1 "two" ];
  selfPath = ./10-builtins-and-paths.nix;
  parentDir = ./..;
}
