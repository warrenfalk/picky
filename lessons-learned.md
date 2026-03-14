# Lessons Learned

## Migration Log

### Build environment

- `cargo check` failed before the Relm4 port even started because GTK development packages were not available outside the Nix dev shell. The error surfaced as missing `glib-2.0.pc`, `gtk4.pc`, and related pkg-config entries.
- This means verification for GTK and Relm4 changes in this repo should happen under `nix develop -c ...`, not plain `cargo check`.

### Relm4 API mismatches

- The first compile attempt against Relm4 `0.10.1` failed because `SimpleComponent::update` does not receive `&Root`. The manual migration needed direct window access for close and activation outcomes, so the component had to move from `SimpleComponent` to `Component`.
- The first factory conversion compile failed because `FactoryVecDeque::builder().launch_default()` returns a connector, not the actual `FactoryVecDeque`. For a factory with no child outputs, the correct call is `launch_default().detach()`.
- The factory setup also needed an explicit `FactoryVecDeque<ResultRow>` type annotation before `widget()` would resolve cleanly for the list box parent.
- Moving `ModuleRegistry` into a Relm4 worker required the `Module` trait to be `Send`. The modules in this repo already fit that requirement, but the trait boundary had to state it explicitly before the worker version could compile.
- GTK signal handlers that use `ComponentSender::input(...)` can panic during teardown if the component runtime is already gone. For shutdown-sensitive signals like `row-selected`, cloning `input_sender()` and using `send(...)` is safer because it fails without panicking.

### Verification

- After switching the root from `SimpleComponent` to `Component`, the Relm4 port compiled successfully under `nix develop -c cargo check`.
- `nix develop -c cargo test` passed all existing tests after the migration slice landed.
- After the factory conversion and worker migration, `nix develop -c cargo check` and `nix develop -c cargo test` still passed.

## Gaps In `relm4-best-practices.md`

- The guide is strong on architecture, but it does not include a short "migration from imperative gtk4-rs" section. That would be useful here because the main first step is converting callbacks and widget mutation into messages and model state without over-engineering the component tree.
- The guide recommends factories for dynamic lists, but it does not explain when a team should accept a temporary non-factory intermediate state during a migration. That decision matters in real codebases where stabilizing message flow first can reduce risk.
- The guide does not call out the practical tradeoff between `SimpleComponent` and `Component` during migration work. In real code, needing `&Root` for window lifecycle or dialog control is a common reason to start with `Component` even when the longer-term structure still follows the same message-driven model.
- The guide is intentionally architectural, but a short appendix with current-baseline manual trait signatures would make migration work faster. The first porting issue here was not conceptual, it was the exact 0.10 trait boundary between `SimpleComponent` and `Component`.
- The guide would benefit from one short factory-builder note covering the `connector -> detach/forward -> FactoryVecDeque` lifecycle. That distinction is obvious once you inspect the crate source or examples, but it is an easy place to lose time during a migration.
- The guide should mention the practical worker prerequisite that background services must be `Send`. That is obvious from the `Worker` trait once you read the source or examples, but it matters immediately when migrating a stateful service like a module registry out of the UI component.
- The guide should warn more explicitly about teardown behavior when GTK signals can still fire during widget destruction. In this migration, `row-selected` fired while the list was being torn down, and the panic only went away after switching from `ComponentSender::input(...)` to the fallible `input_sender().send(...)` path.
