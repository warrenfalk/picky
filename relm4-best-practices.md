# Relm4 Best Practices

Relm4 is easiest to use well when you treat it as a disciplined way to organize `gtk4-rs`, not as a magic abstraction that replaces GTK. Its center of gravity is Elm-style message flow: widgets emit messages, messages update the model, and the view reflects the model.

Metadata for this guide, including version baseline and sources, lives in [`metadata.md`](metadata.md).

## Core principles

- The model is the source of truth, not the widget tree.
- UI events should become messages, not ad-hoc callback logic.
- Components should be defined by state ownership and behavior, not just by visual layout.
- Dynamic repeated UI should default to factories.
- Async work should re-enter the app as messages rather than creating a parallel control flow.
- Shared state is a specific tool for real cross-component sharing, not the default architecture.
- Dropping to raw GTK is normal when it improves clarity or reaches APIs Relm4 does not abstract well.

## How to think about UI in Relm4

Relm4 wants you to think in terms of state transitions, not widget mutation. The default loop is:

1. receive an input message
2. update the model
3. reflect the new model in the view

The important implication is that widgets are not the source of truth. The model is. The `view!` macro, `#[watch]`, trackers, and factories all exist to make that model-to-view synchronization efficient and ergonomic.

### Recommended mindset

- Treat GTK signals as message producers, not as places to embed business logic.
- Treat the model as the authoritative state of the component.
- Treat the view as a projection of model state into widgets.
- Prefer explicit message flow over ad-hoc callback webs.
- Reach for raw widget mutation only when it is clearly view-specific and does not undermine the model as source of truth.

### Why this matters

- The simple example in the README shows the intended baseline: button clicks emit messages, `update()` mutates state, and `#[watch]` refreshes derived widget properties.
- The manual example on docs.rs shows the same architecture without macros: `init_root`, `init`, `update`, then `update_view`. This is useful because it makes clear that the macro is convenience, not a different architecture.

### Practical rule

If you are about to ask "how do I change this widget from somewhere else?", the first question should usually be "what message changes the model so the widget ends up correct as a consequence?"

## Core architecture and state flow

### Prefer message-driven state transitions

- Define small input enums that describe user intent or external events.
- Keep `update()` focused on state transitions and coordination, not widget plumbing.
- Use output messages and forwarding when a child needs to notify a parent about something meaningful.

This keeps data flow legible and makes it easier to reason about side effects, async work, and cross-component communication.

### Keep the model authoritative

- Store durable UI state in the model.
- Derive labels, enabled states, and visibility from the model.
- Avoid storing state only in widgets unless it is truly ephemeral and local to the widget.

### Use the macro and manual styles deliberately

- Prefer `#[relm4::component]` plus `view!` for normal application code.
- Use the manual `SimpleComponent` style when you need to understand or control the lifecycle more directly.
- Do not assume the macro hides a separate runtime model; it still maps to the same `update` and `update_view` flow.

### Keep only the widget references you actually need

With the component macro, `view_output!()` constructs the generated `Widgets` struct for you. Treat that struct as the place for persistent widget handles you genuinely need later.

- Keep references to widgets you must update outside their initial construction.
- Do not store every widget by default just because you can.
- If a widget only needs to exist in the tree and never needs later direct access, let GTK own it and leave it out of your stored widget state.

This keeps component state smaller and makes it clearer which widgets are part of your real update surface.

## Designing components and messages

### Choose components around state ownership

- Introduce a child component when a piece of UI has meaningful local state, its own message loop, or reusable behavior.
- Keep a component small enough that its input and output enums remain easy to understand.
- Avoid splitting components purely by visual layout if the state still wants to move together.

At application scale, this usually leads to one orchestrator component plus a set of domain-oriented children such as sidebar, content pane, dialog, preview, or background-document component.

### Forward meaning, not noise

- Child components should usually emit semantic outputs rather than leaking raw GTK events upward.
- Parents should forward or transform child output into parent input only when the parent truly owns the next decision.
- The v0.5 redesign and later builder APIs strongly reinforce this pattern through `launch(...).forward(...)` and related connector methods.

### Controller lifetime is architectural, not incidental

- If a parent owns a child component, store the `Controller` in the parent model when the child should stay alive.
- If you intentionally want the child runtime to outlive the controller handle, use `detach_runtime()`.
- Do not drop a `Controller` casually and then keep sending messages to it; the tips chapter explicitly calls this out as a common failure mode.

Controllers are also the normal interface to child components:

- use `widget()` to place the child in the parent view
- use `sender()` when the parent needs to send child input messages directly

That is usually a better boundary than trying to reach into a child component's internal widgets.

## Handling async and background work

### Prefer official async pathways over ad-hoc threading

- Use commands, workers, async components, reducers, or the documented `spawn` helpers instead of inventing your own message bridge.
- Keep blocking work off the UI thread.
- Model async completion as messages that re-enter the normal update loop.

In practice, the split usually looks like this:

- use a worker for blocking or serialized background work
- use commands or `oneshot_command` for non-blocking async tasks
- use `AsyncComponent` when initialization or steady-state logic is naturally async

### Use async to initialize real state, not half-state

The v0.5 guidance explicitly recommends async initialization when data must be loaded before the component is truly ready. This avoids constructing a fake partially initialized model and then patching it up immediately afterwards. Relm4 also supports placeholder widgets during async initialization.

### Practical async rule

- Start background work from a message or initialization path.
- Return the result as a message.
- Update the model once the result arrives.
- Let the view react from model state.

That keeps async as an extension of MVU instead of a parallel architecture.

## Building lists and other dynamic collections

### Default to factories for dynamic repeated UI

Factories are the official Relm4 answer for lists and similar collections. The book is explicit here: if your UI is generated from a changing collection of data, a factory is usually the idiomatic path.

### Why factories are preferred

- They keep collection data in Rust-native structures instead of scattering row state across widgets.
- They support efficient reconciliation instead of brute-force rebuilding.
- They support per-item state and message handling through `FactoryComponent`.
- They support output forwarding from items to their parent.
- They avoid stale positional references by using `DynamicIndex` for reorderable elements.

### Mutation discipline matters

- Mutate `FactoryVecDeque` and related structures through their guard.
- Make several changes while the guard is alive.
- Let reconciliation happen automatically when the guard is dropped.

This RAII pattern is not incidental. It is the mechanism that prevents "I mutated data but forgot to render" bugs and allows Relm4 to optimize updates.

### Anti-pattern

Do not hand-roll dynamic list UIs with lots of manual widget insertion and removal unless you have a concrete reason to bypass factories. That approach usually recreates complexity that Relm4 already solved.

## Reuse with widget templates and typed abstractions

### Use widget templates to reduce repetitive view trees

- Reach for widget templates when the same subtree shape and styling repeats across screens or components.
- Use templates to encode structure and styling once, then customize via properties and template children.
- Prefer this over copy-pasting large `view!` blocks with minor edits.

Templates are an official maintainability feature, not just syntactic sugar.

### Use typed abstractions when the GTK API is too boilerplate-heavy

The v0.7/v0.8 direction adds typed views such as `TypedListView`, `TypedColumnView`, and `TypedGridView` specifically to reduce boilerplate and improve type safety around GTK collection widgets.

Practical rule:

- Stay with plain widgets when the base GTK API is already clear.
- Move to typed abstractions when they remove repeated setup and encode useful invariants.

## Performance and update discipline

### Do not update the whole view if only a small part changed

Relm4's efficient-update story is centered on two tools:

- trackers for struct-field change detection
- factories for collection reconciliation

The efficient UI chapter is explicit about the problem: once data and widgets are separated, you need a deliberate way to know what actually changed.

### Use `#[watch]` for straightforward derived properties

The simple counter example uses `#[watch]` to tie label text directly to model state. This is the most direct expression of the normal case: a widget property depends on model data and should refresh when that dependency changes.

### Use trackers when updates become too broad

- Reset tracking at the start of the update if your tracker strategy requires it.
- Use tracker setters or update helpers instead of raw assignment when you want changes to be observable.
- Guard expensive or specific widget updates with `#[track = "..."]`.

Trackers are not mandatory everywhere, but they are the official answer when "just watch the whole model" becomes too coarse.

## Shared state and cross-component data flow

### Use shared state to solve real sharing problems

Relm4 exposes `SharedState`, `Reducer`, and `AsyncReducer` as first-class tools. They are appropriate when several components need coordinated access to the same data source and message forwarding through multiple levels would become artificial or noisy.

### Prefer narrower mechanisms first

- Start with parent-child message passing when ownership is clear.
- Introduce shared state when multiple peers genuinely need to observe or mutate the same data.
- Prefer reducers when you want message-driven centralized mutation rather than arbitrary direct writes.

### Important behavioral detail

`SharedState` notifies subscribers when the write guard is dropped. That means the guard boundary is part of your update semantics. Keep write scopes tight and intentional.

This pattern fits cases where several parts of the UI genuinely depend on the same shared state machine or data source.

### Important `#[watch]` gotcha

`#[watch]` does not make a component magically reactive to arbitrary external state. It updates watched properties when the component itself receives an input or command message.

That means:

- `#[watch]` works naturally for your component's own model changes
- `#[watch]` does not automatically react to `SharedState` changes unless the component subscribes and receives a message

If a watched expression depends on shared state, make sure the component is actually subscribed to the updates that should trigger a redraw.

## Working effectively with raw GTK when needed

Relm4 is a layer on top of `gtk4-rs`, not a closed world. The official docs and blog both treat GTK interop as normal.

### Good reasons to drop to raw GTK

- A widget or API surface is not covered by Relm4's abstractions.
- You need direct access to a lower-level GTK feature.
- The typed abstraction would be more awkward than the raw widget API for your specific case.

In practice, this often includes raw GTK list, file, and dialog APIs where Relm4's role is to organize state and message flow rather than replace GTK completely.

### Bad reasons to drop to raw GTK

- Avoiding messages because direct mutation seems faster to write.
- Replacing factories with hand-managed widget collections without a concrete limitation.
- Smuggling important application state into widget internals.

### Resources and icons are a GTK integration concern

Real applications often need custom icons and bundled resources. Treat this as normal GTK setup work rather than something that should distort your Relm4 architecture.

- Compile resources in `build.rs` when packaging icons or other bundled assets.
- Register the gresource bundle during application startup.
- Add the resource path to `gtk::IconTheme` if you want GTK to resolve custom icons by name.

This belongs at the application boundary, not inside random components.

## Pitfalls and anti-patterns to avoid

### Message recursion

If a view update emits the same message that triggered it, you can freeze the app in an infinite loop. The tips chapter calls this out directly. If a property update triggers a signal that sends the same input back into the component, block or structure that signal path intentionally.

### Dropping controllers too early

Dropping a `Controller` usually shuts down the component runtime. Sending to it afterwards can panic or fail. Store controllers deliberately or detach them intentionally; do not rely on accidental lifetimes.

### Treating widgets as the state store

This erodes the MVU model quickly. Keep durable state in the model and let widgets reflect it.

### Overusing shared state

Shared state is powerful, but if it replaces clear ownership everywhere, the codebase becomes harder to reason about. Use it where sharing is real, not as a shortcut around designing message flow.

### Reintroducing shared mutable model shortcuts

Older Relm design notes are useful here even though the APIs have changed: patterns equivalent to "just put the model behind shared interior mutability and poke it from anywhere" trade compile-time structure for runtime fragility. In practice this weakens message flow, increases incidental coupling, and can reintroduce borrow or ordering bugs that Relm-style architectures are trying to avoid.

### Rebuilding or manually patching large collections

For repeated dynamic UI, bypassing factories is usually a maintainability and performance regression.

### Copying old book or blog code blindly

The architecture guidance remains useful, but version drift is real. Always reconcile code samples with the current docs and changelog.

### Smuggling persistence directly into view components

In data-heavy apps, a stricter pattern usually works better: view components express intent, a document or domain component persists and validates the change, and only then does the view receive the resulting state update. If the view mutates durable data structures directly all over the tree, persistence and correctness become harder to reason about.

### Creating accidental lifetime or cycle problems

Historical Relm releases had to correct reference-cycle and lifecycle issues around streams, widgets, and callbacks. The transferable lesson for Relm4 is simple: be deliberate about ownership. Store controllers intentionally, detach runtimes only when you mean to, and avoid inventing side channels that make component lifetimes hard to reason about.

## Workflow and debugging

### Use GTK tooling to discover the right widget first

- When you are unsure which GTK widget or property model fits a screen, inspect GTK demos and widget docs before writing large amounts of Relm4 code.
- Pick the GTK structure first, then decide whether plain `view!`, a template, a factory, or a typed abstraction is the right Relm4 wrapper around it.
- Do not pick widgets by name alone. A widget that sounds close to what you want can still be the wrong shape and force avoidable refactors later.

### Use the GTK inspector while iterating

- Inspect the live widget tree to understand styling, layout, visibility, focus, and property state.
- Reach for it early when a `view!` tree "looks right" but renders wrong.

### Learn from official examples before inventing a pattern

- Use the small examples to anchor architecture decisions.
- If the problem is lists, async loading, state sharing, templates, dialogs, or forwarding, there is usually already an example that shows the intended shape.

### Prefer small reversible steps

- Start with a simple component and message loop.
- Add child components only when ownership or reuse demands it.
- Add factories when repeated dynamic UI appears.
- Add shared state only after message routing becomes artificial.

That sequence tends to preserve clarity and keeps the app close to Relm4's strongest path.
