# stacksafe-macro compatibility patch

The Windows workspace patches `stacksafe-macro` 0.1.4, required by GPUI 0.2.2 through
`stacksafe` 0.1.4. Macro errors use `syn::Error` instead of `proc-macro-error2`, removing
the `pub_use_of_private_extern_crate` future-compatibility warning. Stack growth and
the generated function bodies retain the upstream behavior.

- Base package: [stacksafe-macro 0.1.4](https://crates.io/crates/stacksafe-macro/0.1.4),
  source revision `a5c029b7d3a5fe6771ecd79266b6e21313cfa174`.
- `src/lib.rs` and `LICENSE` are copied from the upstream
  [macro diagnostics fix](https://github.com/fast/stacksafe/commit/dfe5a659add859eb44a964fb2b7890908b9af71b),
  with Rust 2021 formatting applied to `src/lib.rs`.
- `Cargo.toml` is standalone, retains the 0.1.4 package version and Rust 1.80/2021
  requirements, and replaces `proc-macro-error2` with `proc-macro2`. Publishing is disabled.

Keep this source and its Apache-2.0 license tracked. The root `[patch.crates-io]`
entry applies it to ordinary Cargo builds as well as `scripts/build-desktop.ps1`.
It is not a member of the Windows workspace or the firmware project.

Remove this directory and the root patch/exclude entries once GPUI accepts a
`stacksafe` version whose macro no longer requires `proc-macro-error2`. Verify the
dependency tree, desktop build and desktop tests when changing the patch.
