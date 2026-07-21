---
nessemble: minor
---
Expose random-number functions to the pseudo-op scripting engine via the
[`rhai-rand`](https://docs.rs/rhai-rand) package: scripts can now call `rand()`,
`rand(min, max)`, `rand_float()`, `rand_bool()`, and the array `shuffle`/`sample`
helpers for procedural noise and randomized data tables. Available on native
builds; absent from the WebAssembly build (no system entropy source), the same as
filesystem access. Also add a `--mlist` flag that includes macro-created labels
in the `-l`/`--list` output — such labels (e.g. `\@`-uniquified loop targets) are
hidden from the list by default so it stays readable. Documents both changes and
adds guidance on choosing macros vs. scripts.
