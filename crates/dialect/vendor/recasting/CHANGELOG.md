
## [0.2.3] - 2026-08-26
for circular dev-dependency reasons, also fixes docs

## [0.2.2] - 2026-08-26
### Additions
- The `Recast` impls for `&mut T`, `Cell`, `RefCell`, and `Box` now accept `?Sized`
  types, which lets things like `Box<[T]>` and `&mut [T]` be recast

### Changes
- Removed the `Rc` and `Arc` impls which were only working if they had exclusive access
- Moved the `BTreeMap` impls from the "std" flag to the "alloc" flag

## [0.2.1] - 2023-08-29
### Changes
- Moved the `Arc` impl under the "std" flag because `alloc::sync` doesn't always exist

## [0.2.0] - 2023-08-29
### Additions
- Added some missing impls

### Changes
- There is now a separate "alloc" flag, and the flags are on by default

## [0.1.0] - 2023-08-20
### Additions
- Added the `Recast` and `Recaster` traits
