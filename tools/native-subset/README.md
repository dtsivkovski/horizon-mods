# native-subset

Decides whether a native mod's owner update stays inside the subset that may merge without a reviewer.
`.github/workflows/owner-update.yml` runs it, built from `main`, on both packages of an update that
`scripts/owner_update.py` found eligible: the new `.hmod` and the previous release's. It reads them as
data. Nothing is extracted, compiled or run.

It checks the whole new package, not the diff: a clean diff can call a helper already in the tree. The
previous package only proves that the build inputs did not change.

```sh
native-subset --id <id> --version <version> \
  --old old.hmod --old-sha256 <hex> --new new.hmod --new-sha256 <hex> \
  --pin <horizon commit>=<horizon-mixins commit or -> [--pin ...]
```

It prints one JSON object: `ok`, `rules` (the ids below, sorted) and `findings` (rule, package, path,
line, detail). It exits 0 when the update is in the subset, 1 when it is not, and 2 when the
arguments are wrong. The workflow passes on only the exit status and the rule ids.

## Rules

| Id | Refused when |
| --- | --- |
| R1 | A package's SHA-256 differs from its entry. |
| R2 | A package is not a plain zip: data before, between or after the entries, an archive comment, zip64, a general-purpose flag other than UTF-8 names (so no encryption or data descriptor), compression other than stored or deflated, an extra field other than Unicode path, extended timestamp, NTFS and unix ids, a local header that differs from its central record in name, method, flags, CRC or sizes, or a CRC or size that differs from the data. |
| R3 | An entry is not a regular file: a link, a directory or a device. |
| R4 | A path is absolute, has `..`, `.`, an empty component, `\`, a control or non-ASCII character, a character Windows refuses, or a Unicode path field (0x7075) in either header that names anything but that header's own name. |
| R5 | Two paths are the same ignoring case, a path is given twice, or a file has the name of another file's directory. |
| R6 | A path is outside the layout, or `horizon-mod.toml`, `Cargo.toml` or `src/lib.rs` is missing. The layout is `horizon-mod.toml`, `Cargo.toml`, `Cargo.lock`, `uses.json`, `layouts.json`, `icon.png`, root `README*`, `LICENSE*`, `NOTICE*` and `COPYING*`, `src/**/*.rs`, `assets/**` and `data/**`. |
| R7 | The archive is over 64 MiB, has over 20,000 entries, an entry is over 32 MiB, or the entries are over 256 MiB together. |
| R8 | `Cargo.toml` differs from the previous one except `package.version` and `package.description`, or doesn't parse. |
| R9 | `Cargo.toml` has a dependency other than `horizon-mod-api = { path = ".horizon/sdk/api/crates/horizon-mod-api" }`, a build script (`package.build` other than `false`), a proc-macro or a `lib.path` other than `src/lib.rs`, or a table other than `[package]`, `[dependencies]`, `[lib]` and `[lints]`: no `[workspace]`, even empty, `[patch]` or `[replace]`, as the launcher refuses. |
| R10 | `Cargo.lock` is new, or differs from the previous one except the mod's own version. |
| R11 | `horizon-mod.toml` has `internals`, `inject`, `injection` or `wasm`, or `[native]` has keys other than `game`, `horizon` and `mixins` (`[[native.mixin]]`, `[[native.module]]`, bridges, state, loops, world control). |
| R12 | `horizon-mod.toml` differs from the previous one except `version`, `description`, `native.horizon` and `native.mixins`, has no `[native]`, or names another id or version than the entry. |
| R13 | `native.horizon` is not the commit of a horizon-rust/horizon SDK release (`sdk-<commit>`), or `native.mixins` is not the horizon-mixins revision that commit builds. |
| R14 | A `.rs` file is not UTF-8, doesn't parse, or uses syntax the checker can't read. |
| R15 | `unsafe`, anywhere, macro arguments included. |
| R16 | `extern`: an ABI, an extern block or an extern crate. |
| R17 | `macro_rules!`. |
| R18 | An attribute other than `derive`, `default`, `test`, `ignore`, `should_panic`, `cfg`, `cfg_attr` (whose attributes are checked the same way), lints, `doc`, `must_use`, `inline`, `cold`, `track_caller`, `non_exhaustive`, `deprecated`, `repr`, `rustfmt::skip` and the SDK's `horizon_mod` and `horizon_part`, which take no arguments. Arguments hold only paths, literals and lists of them (`doc` may also be a macro, checked as one); `derive` names paths. So `no_mangle`, `export_name`, `link*`, `path`, `feature`, `global_allocator`, `used`, `macro_use` and `no_std` are refused, and so is `#![forbid(unsafe_code)]` at the crate root, which the launcher refuses (`deny` it instead). |
| R19 | A macro other than `assert*`, `debug_assert*`, `format`, `format_args`, `write`, `writeln`, `print*`, `eprint*`, `vec`, `matches`, `todo`, `unimplemented`, `unreachable`, `panic`, `cfg`, `line`, `column`, `thread_local`, the SDK's `json`, `include_str` and `include_bytes`; one named through a path that doesn't start at `std`, `core`, `alloc`, `mod_api` or `serde_json`; a `use` that gives one of those names to something else; or arguments that don't parse as the macro's own grammar (expressions, `vec!`'s `x; n`, `matches!`'s pattern and guard, `cfg!`'s predicate, `thread_local!`'s statics, `json!`'s values), which are then checked as the code they are. So `include!`, `env!`, `option_env!`, `concat!`, `asm!` and `global_asm!` are refused. |
| R20 | `include_str!` or `include_bytes!` takes something other than one string literal: a relative path from the file's own directory, `..` included but never above the package, to a file of the package under `assets/` or `data/` that is not `.rs`. |
| R21 | `std`, `core` or `alloc`, anywhere in a path, is followed by a module other than `any`, `array`, `ascii`, `borrow`, `boxed`, `cell`, `char`, `clone`, `cmp`, `collections`, `convert`, `default`, `error`, `f32`, `f64`, `fmt`, `hash`, the integer modules, `iter`, `marker`, `mem`, `num`, `ops`, `option`, `primitive`, `rc`, `result`, `slice`, `str`, `string`, `sync`, `time` and `vec`. So `fs`, `net`, `process`, `env`, `os`, `path`, `ffi`, `thread`, `io`, `ptr`, `arch`, `panic` and `prelude` (whose `v1` re-exports `include!` and `env!`) are refused. |
| R22 | `std`, `core` or `alloc` itself is renamed (`use std as s`) or glob-imported (`use std::*`). |

Every `.rs` file in the package counts, whether a module reaches it or not. Only an item under exactly
`#[cfg(test)]` is skipped, apart from its attributes: `#[cfg(any(test, ...))]` is not.

## What it does not see

It is syntactic. A method call on a value the SDK returns is not checked, nor what the code does with
the API: server abuse, console commands, sending in-game data or a clipboard to the joined server. A
compiler soundness bug can give safe Rust any power. `assets/` and `data/` are not read. That is why
only vouched owners (`.github/auto-merge-owners.txt`) get it; `revoked.toml` is the remedy.

## Development

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
