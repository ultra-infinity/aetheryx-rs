# Attribute sets: nested paths (merged), quoted and dynamic keys, inherit.
let
  x = 1;
  y = 2;
  src = { a = 10; b = 20; c = 30; };
  key = "dyn";
in
{
  # These three bindings are merged into one nested set `services`.
  services.nginx.enable = true;
  services.nginx.port = 80;
  services.ssh = { enable = false; };

  "quoted key" = "ok";
  "with-dash" = 1;
  ${key} = "dynamic";
  "${key}2" = "dynamic interpolated";

  inherit x y;
  inherit (src) a b;

  nestedSet = { inner = { deeper = "value"; }; };
  emptySet = { };
}
