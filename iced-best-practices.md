# Iced Best Practices

This guide is for current stable Iced, with the main advice aimed at `0.14.x`.
The architectural guidance here is intended to survive version changes. Exact helper names and some APIs do drift. In particular, older material may talk about `Command` where current Iced uses `Task`, and development-branch examples may show unreleased `0.15.0-dev` details.

If you need source weighting, validation notes, or version-specific caveats, see [`iced-best-practices-metadata.md`](iced-best-practices-metadata.md).

## 1. Start with the right mental model

Think of an Iced app as a state machine with a rendered projection:

- state is the source of truth
- `view` turns that state into widgets
- user input and external events become messages
- `update` changes state and optionally returns effects

This means:

- `view` should describe the UI you want right now, not try to manipulate widgets imperatively
- `update` should be where decisions happen
- effects should stay explicit instead of being hidden in widget code or callbacks

Treat Iced as a compositional Elm-style UI library, not as an imperative retained-widget toolkit. Reach for regular widget composition first. Only drop to custom widgets, `canvas`, `shader`, or other lower-level APIs when normal composition no longer fits the interaction you need.

## 2. Choose app structure by ownership

For anything beyond a toy app, use `iced::application(...)` instead of building everything around the minimal `run(...)` entry point. The application builder is the normal path once you need subscriptions, theming, window settings, fonts, or other runtime configuration.

Structure the app around ownership:

- keep cross-screen or app-wide state at the top
- keep screen-local state inside that screen
- split major modes into an enum like `Screen`
- use explicit loading, ready, editing, and error states instead of loosely related flags

Good Iced apps usually read like:

- a root app that owns navigation and shared state
- per-screen modules with their own state, messages, and view/update functions
- clear boundaries for tasks and subscriptions

If your program is primarily a background process that may open windows opportunistically, use `Daemon`. Otherwise, prefer a normal `application`.

## 3. Design messages around meaning, not widgets

Messages should describe meaningful events for the part of the app that receives them.

Prefer:

- `SearchChanged(String)`
- `Save`
- `ModelSelected(Id)`
- `DownloadFinished(Result<...>)`

Over broad or leaky messages like:

- `ButtonClicked`
- `TextInput1Changed`
- `ChildSaidSomething`

At module boundaries:

- give each screen or component its own `Message` enum
- map child messages into parent messages with `Element::map`, `Task::map`, and `Subscription::map`
- let the parent decide when a child message should trigger navigation or shared-state updates

When a child needs the parent to do something that is not just "run this task", use a small local `Action` or `Instruction` type. This works especially well for:

- screen transitions
- save-or-cancel flows
- parent-owned state changes
- combining a parent instruction with a task like focusing an input

This keeps child modules reusable and stops the root `Message` enum from becoming a dumping ground.

## 4. Keep state authoritative and explicit

Store durable state, not UI guesses.

Prefer storing:

- the current document, draft, selection, screen, request state, or filter
- enough identity to know which item or request a message belongs to
- explicit mode enums like `Loading`, `Ready`, `Saving`, `Error`

Be cautious about storing:

- text labels that can be derived from state
- multiple booleans that really describe one mode
- duplicated copies of the same domain data in different modules

A few durable rules help:

- use enums for major modes instead of flag combinations
- derive button labels, disabled states, and visible sections in `view`
- keep widget IDs and stable item keys when focus, selection, or list identity matters
- model startup and persistence explicitly with states like `Loading` and `Loaded`, not with half-initialized structs

Most of the time, Iced works better when you keep a single obvious source of truth and derive the UI from it repeatedly.

## 5. Use `Task` for active work

Use `Task` when your app decides to do something:

- fetch data
- save data
- focus or scroll a widget
- change a window setting
- start a background operation
- kick off work during app or screen initialization

Keep the pattern simple:

1. a message says what should happen
2. `update` mutates state and returns a `Task`
3. the task resolves into another message
4. `update` handles the result

Useful habits:

- return `Task::none()` when no effect is needed
- use `Task::batch` when one event should trigger multiple effects
- use `Task::map` when a child task needs to bubble to the parent
- keep cancellation handles for long-running work that should stop when state changes

For progress-producing or streaming work, model the lifecycle in state: idle, running, finished, errored. Let progress updates come back as messages instead of mutating shared state from the background job directly.

## 6. Use `Subscription` for passive work

Use a `Subscription` when the app is listening to something that exists independently of one specific user action:

- timer ticks
- keyboard or window events
- websocket or other long-lived streams
- runtime event feeds

The key rule is that subscriptions should be state-driven.

If the current state does not need a subscription, return `Subscription::none()`. If it does, construct exactly the subscription that should be active for that state. Think of `subscription` the same way you think of `view`: it declares what is active now.

In practice, this means:

- turn subscriptions on and off by state
- filter raw events close to the subscription boundary
- batch subscriptions when a screen needs more than one passive input
- use `Subscription::run` or `run_with` for custom passive streams

Use `Task` for one-shot work. Use `Subscription` for ongoing listening. Mixing those responsibilities is one of the easiest ways to make an Iced app harder to reason about.

## 7. Guard against stale async results

As soon as the app can switch screens, restart work, or issue overlapping requests, stale results become a real problem.

Protect yourself by:

- tagging work with an ID, version, or current selection
- checking that returned data still belongs to the active state before applying it
- dropping or aborting work when leaving the mode that needed it

Do not assume that the last result to arrive is still relevant.

## 8. Build screen and component boundaries for growth

When an app starts growing, the main problem is usually not widgets. It is ownership and routing.

A good default is:

- root app owns navigation and shared services
- each screen owns its local state and message enum
- screen `update` returns either a `Task` or a small `Action`
- parent maps child outputs and decides cross-screen consequences

This style scales well because:

- local modules stay understandable
- parent logic stays explicit
- async work remains traceable
- view composition stays straightforward

Avoid over-engineering early, but do split once a screen has its own lifecycle, async work, or state machine.

## 9. Avoid these common anti-patterns

Do not put side effects in `view`.
`view` should describe widgets from current state. It should not fetch data, write files, mutate state, or smuggle control flow through hidden behavior.

Do not block `update`.
Long-running I/O or computation should go through tasks or other background work. An `update` function that waits inline will make the app harder to reason about and can hurt responsiveness.

Do not keep redundant state unless it is buying you something concrete.
If a label, filtered list, or enabled state can be derived cheaply from existing state, derive it.

Do not leave subscriptions running "just in case".
If a timer, event listener, or stream is only needed in one mode, make that dependency explicit in `subscription`.

Do not let the root `Message` enum become the entire architecture.
Once screens start having real behavior, split their messages and map them upward.

Do not treat old community code as current API authority.
Older posts are still useful for rationale and pitfalls, but current stable Iced is centered on `Task`, the application builder, and state-driven subscriptions.

Do not default to the deprecated `Component` pattern for ordinary app structure.
Current Iced explicitly warns that encapsulated component state works against a single source of truth. Use normal Elm-style composition first.

## 10. A practical default for most apps

If you want a starting template that matches current practice, use this:

- a root app with `application(...)`
- a top-level state struct plus a `Screen` enum
- per-screen modules with local `Message` types
- parent routing via `map`
- `Task` for active work
- state-driven `Subscription` for passive work
- explicit loading, working, and error states
- derived UI in `view`, not mirrored UI state everywhere

That structure fits most Iced applications well, and it remains valid even as helper APIs evolve.
