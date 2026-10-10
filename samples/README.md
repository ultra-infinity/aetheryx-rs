# Sample Nix files

Test inputs for `aetheryx`. Every file has been checked against Nix 2.28.3.

- `valid/` — files that real Nix evaluates. Each is self-contained unless its
  header comment says otherwise. Comments inside the files give the values
  Nix produces for the tricky cases.
- `invalid/` — files that real Nix rejects. The first line comment gives
  Nix's error message.

## Running

```sh
# Evaluate, as `nix eval --file` does
cargo run -p driver -- eval --file samples/valid/09-scoping.nix

# Print the IR: nix_hir, or nix_mir
cargo run -p driver -- debug hir samples/valid/09-scoping.nix
cargo run -p driver -- debug mir --no-locations samples/valid/09-scoping.nix

# Show a diagnostic
cargo run -p driver -- debug hir samples/invalid/duplicate-attr.nix

# Reference result from real Nix
nix eval --extra-experimental-features nix-command --file samples/valid/09-scoping.nix
```

## Expected outputs

`expected/` holds what `nix eval --file` prints for every file in `valid/`, with the
repository root and `$HOME` replaced by `@ROOT@` and `@HOME@`. The `eval`
crate's oracle test (`crates/eval/tests/samples.rs`) checks that our output is
identical. Regenerate after adding or changing a sample:

```sh
samples/gen-expected.sh
```

These files need extra arguments to evaluate with real Nix:

| File | Command |
| --- | --- |
| `12-package-like.nix` | see the header comment (call it with stub arguments) |
| `13-search-path.nix` | `nix eval -I samples=samples/valid --file samples/valid/13-search-path.nix` |
| `14-pipe-operators.nix` | `nix eval --extra-experimental-features "nix-command pipe-operators" --file samples/valid/14-pipe-operators.nix` |

## Contents

| File | Covers |
| --- | --- |
| `01-literals.nix` | numbers, strings, escapes, indented strings, paths, URIs, `true`/`false`/`null` |
| `02-operators.nix` | every binary/unary operator, precedence, short-circuiting |
| `03-interpolation.nix` | string, indented-string and path interpolation |
| `04-attrsets.nix` | nested paths, quoted and dynamic keys, `inherit`, `inherit (e)` |
| `05-let-and-rec.nix` | `let`, `rec`, forward references, self-recursion |
| `06-lambdas.nix` | currying, patterns, defaults, `...`, `@` bindings |
| `07-select-hasattr.nix` | `.`, `or`, `?`, dynamic selection |
| `08-control-flow.nix` | `if`, `assert`, `with` (including `with` not shadowing `let`) |
| `09-scoping.nix` | name resolution corner cases |
| `10-builtins-and-paths.nix` | builtins, `__curPos`, `./.`-style paths |
| `11-legacy-let.nix` | deprecated `let { body = ...; }` |
| `12-package-like.nix` | a realistic nixpkgs-style package function |
| `13-search-path.nix` | `<name>` search paths |
| `14-pipe-operators.nix` | experimental `\|>` and `<\|` |
| `15-attrset-merging.nix` | attribute set merging, including `rec`-ness of merged sets |
| `16-builtin-scoping.nix` | builtins are lexical globals that beat `with`; `__` prefixes; builtins as values |
| `invalid/duplicate-attr*.nix` | duplicate attributes |
| `invalid/duplicate-formal.nix` | duplicate formal arguments |
| `invalid/dynamic-*.nix` | dynamic attributes where Nix forbids them |
| `invalid/undefined-variable.nix` | unbound name outside any `with` |
| `invalid/missing-semicolon.nix`, `unclosed-brace.nix`, `unexpected-token.nix` | parse errors |
