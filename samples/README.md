# Sample Nix files

Test inputs for the driver. Every file has been checked against Nix 2.28.3.

- `valid/` — files that real Nix evaluates. Each is self-contained unless its
  header comment says otherwise. Comments inside the files give the values
  Nix produces for the tricky cases.
- `invalid/` — files that real Nix rejects. The first line comment gives
  Nix's error message.

## Running

```sh
# Lower to nix_hir and print the IR
cargo run -p driver -- hir samples/valid/09-scoping.nix
cargo run -p driver -- hir --no-locations samples/valid/*.nix

# Show a diagnostic
cargo run -p driver -- hir samples/invalid/duplicate-attr.nix

# Reference result from real Nix
nix-instantiate --eval --strict samples/valid/09-scoping.nix
```

These files need extra arguments to evaluate with real Nix:

| File | Command |
| --- | --- |
| `12-package-like.nix` | see the header comment (call it with stub arguments) |
| `13-search-path.nix` | `nix-instantiate --eval -I samples=samples/valid samples/valid/13-search-path.nix` |
| `14-pipe-operators.nix` | `nix-instantiate --eval --strict --extra-experimental-features pipe-operators samples/valid/14-pipe-operators.nix` |

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
