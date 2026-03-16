# Repository Guidelines

## Important

Read `iced-best-practices.md` and follow the guidelines therein.

## Project Structure & Module Organization

`src/main.rs` is the binary entry point and delegates startup to `src/launcher.rs`. The Iced application state and UI live in `src/app.rs`. Shared domain types such as `Module`, `SearchResult`, and action outcomes live in `src/module.rs`. Individual picker data sources are in `src/modules/` (`applications.rs`, `niri_windows.rs`, `mako_notifications.rs`, etc.). The standalone renderer diagnostic binary is `src/bin/wgpu_probe.rs`. Migration notes and platform-specific guidance live at the repo root in `iced-*.md`.

## Build, Test, and Development Commands

- `nix develop`: enter the pinned dev shell with Rust, GTK, Vulkan, and Wayland tooling.
- `cargo run`: run `picky` locally.
- `WGPU_BACKEND=gl ICED_BACKEND=wgpu cargo run`: preferred graphics path when checking Iced rendering issues.
- `cargo check`: fast compile verification.
- `cargo test`: run unit tests.
- `cargo run --bin wgpu_probe`: inspect `wgpu` adapter/backend detection.
- `nix build`: build the packaged application and launcher wrapper.
- `nix fmt flake.nix`: format Nix files with `nixfmt-rfc-style`.

## Coding Style & Naming Conventions

Use standard Rust formatting (`cargo fmt`) with 4-space indentation. Prefer small, explicit functions and enums for UI state transitions. Use `snake_case` for functions, modules, and files; `CamelCase` for types; and descriptive message names in the Iced app (`QueryChanged`, `ActivateSelected`, etc.). Keep docs ASCII unless a file already uses Unicode.

## Testing Guidelines

Use Rust unit tests with `cargo test`. Prefer testing pure state transitions and module behavior over UI pixel output. Add focused tests near the relevant code (`src/app.rs`, `src/fuzzy.rs`, module files). Name tests for the behavior they guarantee, e.g. `ignores_stale_search_results` or `close_action_refreshes_when_window_disappears`.

## Commit & Pull Request Guidelines

Recent history favors short, imperative commit messages such as `fix install`, `Fix Iced wgpu runtime on Nix`, and `Add non-consuming notification action`. Keep commits narrowly scoped. PRs should describe user-visible behavior changes, include any required runtime env details (`ICED_BACKEND`, `WGPU_BACKEND`), and mention manual verification for compositor- or rendering-specific changes.

## Environment & Configuration Notes

This project depends on Nix packaging and graphics runtime behavior. If rendering differs between `cargo run` and installed builds, verify the `wgpu` backend first with `src/bin/wgpu_probe.rs` before changing UI code.
