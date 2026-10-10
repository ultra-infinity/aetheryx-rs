#!/usr/bin/env bash
# Records what real Nix prints for every sample in valid/, into expected/.
#
#   samples/gen-expected.sh                  # regenerate
#   git diff --exit-code samples/expected    # check
#
# Output is `nix eval --file` (stdout and stderr), with the repository root
# replaced by @ROOT@ and $HOME by @HOME@ so that the files do not depend on
# the machine. The interpreter's tests substitute them back.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

for file in samples/valid/*.nix; do
  name=$(basename "$file" .nix)
  features=nix-command
  flags=()
  case "$name" in
    13-search-path) flags=(-I "samples=samples/valid") ;;
    14-pipe-operators) features="$features pipe-operators" ;;
  esac
  # A sample that fails in Nix records the error; the exit status is ignored.
  output=$(nix eval --extra-experimental-features "$features" "${flags[@]}" --file "$file" 2>&1 || true)
  output=${output//"$root"/@ROOT@}
  output=${output//"$HOME"/@HOME@}
  printf '%s\n' "$output" > "samples/expected/$name.out"
done
echo "nix: $(nix --version)" >&2
