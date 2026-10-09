# Error: dynamic attributes not allowed in let.
let
  name = "x";
  ${name} = 1;
in
  name
