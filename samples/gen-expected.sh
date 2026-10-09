#!/usr/bin/env bash
# Records what real Nix prints for every sample in valid/, into expected/.
#
#   samples/gen-expected.sh                  # regenerate
#   git diff --exit-code samples/expected    # check
#
# Output is `nix-instantiate --eval --strict` (stdout and stderr), with the
# repository root replaced by @ROOT@ and $HOME by @HOME@ so that the files do
# not depend on the machine. The interpreter's tests substitute them back.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

for file in samples/valid/*.nix; do
  name=$(basename "$file" .nix)
  flags=()
  case "$name" in
    13-search-path) flags=(-I "samples=samples/valid") ;;
    14-pipe-operators) flags=(--extra-experimental-features pipe-operators) ;;
  esac
  # A sample that fails in Nix records the error; the exit status is ignored.
  output=$(nix-instantiate --eval --strict "${flags[@]}" "$file" 2>&1 || true)
  output=${output//"$root"/@ROOT@}
  output=${output//"$HOME"/@HOME@}
  printf '%s\n' "$output" > "samples/expected/$name.out"
done
echo "nix: $(nix-instantiate --version)" >&2
