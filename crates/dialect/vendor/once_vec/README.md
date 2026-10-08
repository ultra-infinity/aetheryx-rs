# once_vec

[![CI](https://github.com/vaivaswatha/once_vec/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/vaivaswatha/once_vec/actions/workflows/ci.yml)

`once_vec` provides a safe grow-only vector backed by `OnceCell`.

It is designed for cases where you want to append values without taking `&mut self`,
but do not need in-place mutation or removal after insertion. The crate is `no_std`
and only depends on `alloc`.

## Features

- Append-only storage
- Push without a mutable reference
- Immutable once inserted
- Chunked allocation in powers of two
- Efficient access and insertion:
  - `get` is O(1), but not as fast as a `Vec`, due to additional index
     computation.
  - `push` is amortized O(1) - likely faster than `Vec` because no
     data needs to be moved on reallocation.
- No `unsafe` code
- `no_std` compatible
- Does not implement `Sync`: it uses `core::cell` primitives internally,
  so shared concurrent access from multiple threads is not supported.

## Example

```rust
use alloc::vec::Vec;
use once_vec::OnceVec;

let values = OnceVec::<&str>::default();

let a = values.push("alpha");
let b = values.push("beta");

assert_eq!(a, 0);
assert_eq!(b, 1);
assert_eq!(values.get(0), Some(&"alpha"));
assert_eq!(values.get(1), Some(&"beta"));

let collected: Vec<_> = values.iter().copied().collect();
assert_eq!(collected, vec!["alpha", "beta"]);
```

## Internals

`OnceVec` keeps a fixed base table of chunk pointers. Each row points to a lazily
allocated chunk whose size doubles as you go down.

Example state (len = 10):

```
+-----------------------------+--------------------------------------------+
| base table row -> chunk     | chunk contents                             |
+-----------------------------+--------------------------------------------+
| row / chunk 0 (size 1)      | range [0..=0]   : [0]                      |
| row / chunk 1 (size 2)      | range [1..=2]   : [1][2]                   |
| row / chunk 2 (size 4)      | range [3..=6]   : [3][4][5][6]             |
| row / chunk 3 (size 8)      | range [7..=14]  : [7][8][9][.][.][.][.][.] |
| row / chunk 4 (size 16)     | range [15..=30] : not allocated yet        |
| ...                         | ...                                        |
+-----------------------------+--------------------------------------------+
```

`[n]` means initialized `OnceCell<T>` storing global index `n`.
`[.]` means allocated but uninitialized `OnceCell<T>`.

## Capacity

The default chunk count is `32`, which gives a theoretical maximum length of `2^32 - 1`
elements on 32-bit and larger targets, subject to available memory.

You can choose a smaller `N` when constructing the type if you want a lower maximum capacity.

## Comparison

### vs [`elsa::FrozenVec`](https://github.com/Manishearth/elsa)

- `FrozenVec` uses `stable_deref_trait::StableDeref` for the API that returns
  stable references to inserted items (`&T::Target`), which typically means
  storing values behind indirection (for example `Box<T>`, `Arc<T>`, etc.).
- Implementation includes `unsafe` internally (for example via `UnsafeCell`
  access patterns in `src/vec.rs`).
- `once_vec` is fully safe Rust and stores plain `T` directly.

### vs [`appendlist`](https://github.com/danieldulaney/appendlist/blob/master/src/appendlist.rs)

- Uses a `Vec<Vec<T>>` chunk layout and explicitly documents an unsafe-internal
  implementation strategy (for example with `UnsafeCell<Vec<Vec<T>>>`).
- Also provides O(1) indexing and append behavior with chunked growth.
- `once_vec` provides a similar append-only ergonomics with a fully safe,
  `OnceCell`-based implementation.

## Benchmarks

The repository includes a Criterion benchmark that compares `once_vec` with the
two crates listed above across three workloads:

- append from empty
- indexed reads over a populated collection
- full iteration over a populated collection

It reports two comparison modes:

- native storage, where each crate uses its most direct comparable element type
- boxed storage, where all three crates store `Box<usize>` for a more direct
  apples-to-apples comparison with `elsa::FrozenVec`

Run it with:

```bash
cargo bench --bench comparison
```

The results are available in `target/criterion/report/index.html`.

If you make changes to the code and want to compare the new results with a previous run,
you can use Criterion's baseline feature:

- `cargo bench --bench comparison -- --save-baseline name` to save the results as
a baseline for future comparisons.
- `cargo bench --bench comparison -- --baseline name` to compare the results
against a previously saved baseline.

Note: `elsa::FrozenVec` only exposes stable references for `StableDeref` items,
so its native-mode benchmark stores `Box<usize>` while the other native-mode
benchmarks store plain `usize` values.

### Results

These are the results on my machine. Values are mean time per benchmark iteration
in nanoseconds (lower is better).

#### append/native (ns)

| crate | 64 | 1024 | 16384 |
| --- | ---: | ---: | ---: |
| once_vec | 279.19 | 3107.10 | 44100.68 |
| appendlist | 255.77 | 3264.62 | 46947.33 |
| elsa_frozen_vec | 1750.05 | 25944.63 | 629079.94 |

#### append/boxed (ns)

| crate | 64 | 1024 | 16384 |
| --- | ---: | ---: | ---: |
| once_vec | 2124.13 | 41473.55 | 662905.74 |
| appendlist | 1819.71 | 39531.16 | 661521.36 |
| elsa_frozen_vec | 1832.50 | 26161.87 | 623053.12 |

#### get/native (ns)

| crate | 64 | 1024 | 16384 |
| --- | ---: | ---: | ---: |
| once_vec | 113.23 | 1849.50 | 35674.28 |
| appendlist | 103.05 | 1653.38 | 30620.56 |
| elsa_frozen_vec | 47.82 | 928.57 | 22775.42 |

#### get/boxed (ns)

| crate | 64 | 1024 | 16384 |
| --- | ---: | ---: | ---: |
| once_vec | 138.27 | 2125.34 | 43149.28 |
| appendlist | 117.51 | 2142.03 | 46130.42 |
| elsa_frozen_vec | 47.20 | 934.95 | 22480.88 |

#### iter/native (ns)

| crate | 64 | 1024 | 16384 |
| --- | ---: | ---: | ---: |
| once_vec | 109.20 | 1645.27 | 26220.76 |
| appendlist | 101.55 | 1646.10 | 26618.18 |
| elsa_frozen_vec | 15.21 | 495.29 | 8650.09 |

#### iter/boxed (ns)

| crate | 64 | 1024 | 16384 |
| --- | ---: | ---: | ---: |
| once_vec | 103.46 | 1651.88 | 26293.84 |
| appendlist | 89.26 | 1557.41 | 23518.99 |
| elsa_frozen_vec | 15.10 | 500.53 | 8679.69 |
