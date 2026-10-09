# String and path interpolation.
let
  name = "world";
  n = 3;
  dir = "sub";
in
{
  greeting = "hello ${name}!";
  nested = "outer ${"inner ${name}"}";
  number = "n = ${toString n}";
  indented = ''
    Hello, ${name}.
    Count: ${toString (n + 1)}
  '';
  path = ./${dir}/file.txt;
  onlyInterpolation = "${name}";
}
