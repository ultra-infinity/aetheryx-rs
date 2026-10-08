# Recasting

This crate supplies the `Recast` and `Recaster` traits. These are intended for the translation of
arena indexes like those in the `triple_arena` crate, however they can be used for any purpose of
one-to-one recasting of values.

The traits currently need to be manually implemented, but there should be a macro in a future
version that will automate implementation.

This crate has "std" and "alloc" feature flags that can be turned off

#### License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
