# Iced Best Practices Metadata

This document records the source hierarchy, validation approach, and version caveats behind [`iced-best-practices.md`](iced-best-practices.md).

## Scope

- Main guide target: current stable Iced `0.14.x`
- Stable API authority: docs.rs for `iced 0.14.0`
- Development-branch reference: `docs.iced.rs` for `0.15.0-dev`
- Official example validation: both the stable `0.14.0` tag and current `master` where useful
- Real-app pattern validation: `hecrj/icebreaker` and `airstrike/iced_receipts`

## Source hierarchy

The guide was built with this weighting:

1. current official docs and changelog
2. official examples
3. real applications
4. older official and community material
5. community threads and historical discussion

Reasoning:

- official docs and changelog define current concepts and release-scoped API truth
- official examples show intended composition patterns
- real apps validate whether those patterns survive at application scale
- older material is useful for rationale, migration context, and sharp edges, but not current signatures

## Current source of truth

Official sources checked:

- `https://docs.rs/iced/latest/iced/`
- `https://docs.iced.rs/iced/`
- `https://github.com/iced-rs/iced/blob/master/CHANGELOG.md`
- `https://github.com/iced-rs/iced/tree/master/examples`
- `https://book.iced.rs/`

Current baseline confirmed during this work:

- stable docs show `iced 0.14.0`
- development docs show `iced 0.15.0-dev`
- changelog records `0.14.0` on December 7, 2025
- the Pocket Guide sections used as conceptual authority are present in both stable and dev docs:
  - Concurrent Tasks
  - Passive Subscriptions
  - Scaling Applications

## Validation notes

The main recommendations were checked against code, not just prose.

### Mental model and app structure

Validated against:

- Pocket Guide sections in official docs
- `examples/todos`
- `examples/stopwatch`
- `examples/game_of_life`
- `icebreaker/src/main.rs`
- `iced_receipts/src/main.rs`

What this validated:

- state -> view -> message -> update is still the core model
- `application(...)` is the normal path for non-trivial apps
- screen enums and mapped child messages are idiomatic

### Message and screen design

Validated against:

- Pocket Guide "Scaling Applications"
- `icebreaker/src/main.rs`
- `icebreaker/src/screen/search.rs`
- `icebreaker/src/screen/conversation.rs`
- `iced_receipts/src/main.rs`
- `iced_receipts/src/action.rs`

What this validated:

- per-screen message enums
- parent routing with `Element::map`, `Task::map`, and `Subscription::map`
- child-local `Action` or `Instruction` types for parent-owned consequences

### Async and background work

Validated against:

- Pocket Guide "Concurrent Tasks"
- `examples/download_progress`
- `examples/changelog`
- `examples/websocket`
- `icebreaker/src/main.rs`
- `icebreaker/src/screen/search.rs`
- `icebreaker/src/screen/conversation.rs`

What this validated:

- `Task` is the current stable effect API
- batching, mapping, chaining, and cancellation are common patterns
- progress and long-running work are modeled as explicit state machines
- one-shot work and passive streams are kept separate

### State-driven subscriptions

Validated against:

- Pocket Guide "Passive Subscriptions"
- `examples/stopwatch`
- `examples/game_of_life`
- `examples/events`
- `examples/websocket`
- `examples/markdown`
- `icebreaker/src/main.rs`
- `icebreaker/src/screen/conversation.rs`
- `iced_receipts/src/main.rs`

What this validated:

- subscriptions are declared from current state
- screens turn subscriptions on and off by mode
- event filtering near the subscription boundary is common and useful
- custom passive streams via `Subscription::run` are part of the intended model

### State handling

Validated against:

- `examples/todos`
- `examples/game_of_life`
- `icebreaker/src/screen/search.rs`
- `icebreaker/src/screen/conversation.rs`
- `iced_receipts/src/main.rs`

What this validated:

- explicit mode enums outperform flag soup
- stale async results are handled by identity checks or versioning
- stable item keys and widget IDs matter when list identity or focus matters

## Important version caveats

These details were kept out of the main guide unless they materially affect current practice.

### Stable versus dev

- Stable API authority is `0.14.0`.
- The `master` branch and `docs.iced.rs` reflect `0.15.0-dev`.
- Official examples on `master` are valuable pattern evidence, but exact helper names or signatures may drift.

### Old material

Older tutorials and posts often predate the current stable effect model.

Common translation:

- older `Command` guidance generally maps to current `Task`
- older architectural prose can still be useful if the recommendation is conceptual rather than signature-specific

### Component trait

Current source review found `widget::lazy::component::Component` still present but explicitly deprecated since `0.13.0`, with a note that encapsulated state hampers a single source of truth. This is why the main guide recommends normal Elm-style composition first and treats `Component` as a trap for ordinary app structure.

### Examples as authority

- Official examples were treated as first-class evidence for composition patterns.
- Where a pattern appeared in both the stable tag and current `master`, confidence was high.
- Where a helper looked newer or more specialized, the main guide kept the recommendation conceptual instead of naming the helper unless necessary.

## Source notes on non-official material

These sources informed rationale and edge cases, but were not treated as current API authority:

- `deep-research-on-iced.md`
- the Unofficial Iced Guide
- older Iced tutorials and community posts cited in the research index

They were mainly used for:

- explaining why MVU-style discipline matters
- identifying version-drift hazards
- identifying sharp edges around async, subscriptions, and scaling patterns

## Topics intentionally not expanded in the main guide

These were judged real but too specialized to foreground for most readers:

- devtools, comet, and time-travel workflows
- end-to-end testing and headless testing
- custom widget internals
- shader and canvas-specific performance work
- background-service-first apps built around `Daemon`

They may deserve separate guides, but most Iced readers do not need them before they understand core application structure, messaging, async work, and state ownership.
