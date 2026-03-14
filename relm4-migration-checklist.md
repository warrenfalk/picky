# Relm4 Migration Checklist

## Phase 1: Replace Imperative Launcher

- [x] Add `relm4` to the project and switch application startup to `RelmApp`.
- [x] Replace the imperative `Rc<RefCell<UiState>>` launcher with a Relm4 root component.
- [x] Move query changes, selection changes, activation, and close behavior into message handling.
- [x] Keep `ModuleRegistry` and the existing search/activation domain layer intact for the first pass.
- [x] Preserve the existing window sizing, keyboard navigation, result rendering, and action shortcut behavior.
- [x] Verify the new UI shell compiles and tests pass under `nix develop`.

## Phase 2: Make The Results List Idiomatic Relm4

- [x] Convert the results list from full rebuilds to a `FactoryVecDeque`-backed list.
- [x] Move row rendering into a dedicated row factory component with row-local display state.
- [ ] Remove any remaining view-specific selection derivation from the parent where the row can own it cleanly.
- [ ] Re-check keyboard behavior and selection persistence after the factory conversion.

## Phase 3: Handle Blocking Work Properly

- [x] Move search work off the UI thread using a Relm4 worker or command pipeline.
- [x] Move activation work that can block on subprocesses off the UI thread where it improves responsiveness.
- [x] Re-enter the root component only through Relm4 messages for search completion and activation outcomes.
- [x] Decide whether `ModuleRegistry` stays in the UI component or becomes a worker-owned service boundary.

## Phase 4: Tighten Structure And Maintenance

- [ ] Add a small app-level module layout for Relm4-specific concerns, such as row components and workers.
- [ ] Consider a widget template for the repeated result row subtree if the row view keeps growing.
- [ ] Add focused tests around selection movement, action shortcut lookup, and refresh-on-dismiss behavior.
- [ ] Remove any obsolete launcher-era helpers and dead code once the factory/worker phases land.

## Exit Criteria

- [x] The UI uses Relm4 for the root component and the dynamic results list.
- [x] Search and activation work re-enter the app as messages instead of mutating GTK state directly.
- [ ] The current picker behavior is preserved for applications, windows, workspaces, and notifications.
- [x] The migration notes in `lessons-learned.md` document the friction points and guide updates needed.
