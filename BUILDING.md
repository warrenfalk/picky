# Building Picky

`nix build` builds the release package and its launcher wrappers. `nix flake
check` also runs the package's Rust tests and tests the dependency reuse guard.
`nix develop` provides the same pinned Rust toolchain for local Cargo commands;
local Cargo uses its own `target/` directory, independently of Nix's artifacts.

## Dependency caching

The flake uses [Crane](https://crane.dev/examples/quick-start.html) in two stages:

1. `buildDepsOnly` compiles dependencies against placeholder application sources.
2. `buildPackage` imports those Cargo artifacts, compiles Picky, runs its tests,
   and installs the binaries and graphics runtime wrappers.

Ordinary edits to existing Rust source files should reuse the dependency stage.
The first build after migrating to Crane must populate this cache. The artifacts
must remain in the local Nix store or be available from a configured binary
cache; garbage collection or building on another machine can require rebuilding.
Crane caches the dependency set together, rather than one derivation per crate.

## Keep compilation settings together

Use `commonArgs` in `flake.nix` for **all compilation settings** shared by the
dependency and application stages:

- Cargo feature and package selection flags (`cargoExtraArgs`, retaining
  `--locked`), including `--features`, `--no-default-features`, and `--workspace`.
- The Cargo profile (`CARGO_PROFILE`), target, `RUSTFLAGS`, linker settings, and
  environment variables consumed by Cargo, rustc, or dependency build scripts.
- Native libraries, tools, and setup hooks (`buildInputs`, `nativeBuildInputs`).
  These can change `PKG_CONFIG_PATH` and invalidate native dependency artifacts.
- Cargo build/test options and any hooks that affect compilation. Keep build
  and test feature selection consistent; dependency tests are compiled with
  `--no-run`, while the application stage actually runs the tests.

Both stages use one `craneLib` with `overrideToolchain rustToolchain`; the dev
shell uses that same toolchain. The overlay's `stable.latest` is fixed by
`flake.lock`, so merely publishing application changes does not advance Rust.

Application-only installation and wrapping belong in the final stage. Avoid
overriding compilation settings there. If a new hook needs real application
sources, consider that Crane replaces those sources in the dependency stage.
See Crane's [hook guidance](https://crane.dev/faq/control-when-hooks-run.html).

## Automatic reuse guard

After the final stage's `cargo build`, `nix/check-cargo-reuse.jq` inspects Cargo's
JSON artifact messages. Every package outside Cargo's workspace must report
`fresh: true`; otherwise the build fails with the recompiled package IDs. This
catches actual cache misses from flags, toolchains, native library discovery,
or build scripts, without maintaining a partial list of comparable settings.
Workspace packages are allowed to compile. The guard covers the release build;
it does not inspect the subsequent `cargo test` invocation.

`nix flake check` runs guard fixtures that accept workspace compilation and
reused dependencies, and reject rebuilt dependencies, proc macros, and build
scripts. To run just these fast checks:

```sh
nix build .#checks.x86_64-linux.cargo-reuse
```

If the guard fails, compare the two stages' settings and inspect Cargo's build
script inputs. Do not disable the guard simply to accept a slower build. Crane's
[rebuild troubleshooting](https://crane.dev/faq/constant-rebuilds.html) describes
common causes.

## Expected cache invalidation

Changes to `Cargo.lock`, dependency declarations, features, profiles, Rust/Nixpkgs
inputs, native libraries, or compilation environment can rebuild dependencies.
Adding/removing Cargo targets or changing build scripts can also change Crane's
placeholder source. Such changes are valid: the dependency stage rebuilds first,
and the final stage should still reuse its newly built artifacts.

`cleanCargoSource` excludes documentation and unrelated files from the package
source. If Rust later embeds non-Rust assets with `include_bytes!` or a build
script, extend the source filter to include those assets. Do not switch back to
an unfiltered repository source to fix a missing asset.
