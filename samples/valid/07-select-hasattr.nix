# Attribute selection, `or` defaults, and `?` membership tests.
let
  set = { a = { b = { c = "deep"; }; }; key = "a"; };
  name = "a";
in
{
  select = set.a.b.c;
  dynamicSelect = set.${name}.b.c;
  quotedSelect = set."a".b.c;
  orHit = set.a.b.c or "default";
  orMissing = set.a.x.y or "default";
  orNonAttrset = set.key.b or "default";      # `key` is a string, not a set
  has = set ? a;
  hasPath = set ? a.b.c;
  hasMissing = set ? a.x.c;
  hasDynamic = set ? ${name};
}
