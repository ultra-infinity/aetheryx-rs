# Pipe operators (experimental in Nix; evaluate with
#   nix eval --extra-experimental-features pipe-operators --file 14-pipe-operators.nix)
let
  double = x: x * 2;
  inc = x: x + 1;
in
{
  pipeRight = 3 |> double |> inc;      # inc (double 3) = 7
  pipeLeft = inc <| double <| 3;       # inc (double 3) = 7
}
