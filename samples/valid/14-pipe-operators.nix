# Pipe operators (experimental in Nix; evaluate with
#   nix-instantiate --eval --strict --extra-experimental-features pipe-operators)
let
  double = x: x * 2;
  inc = x: x + 1;
in
{
  pipeRight = 3 |> double |> inc;      # inc (double 3) = 7
  pipeLeft = inc <| double <| 3;       # inc (double 3) = 7
}
